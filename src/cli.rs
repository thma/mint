use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::audio::io_read;
use crate::config::{self, Config, PlannedOutput, ResolvedTarget};
use crate::json_report;
use crate::pipeline::{self, ProcessingReport, TaskFailure};
use crate::util::{duration_seconds, fmt_bytes, fmt_db, is_supported_input};

#[derive(Debug, Parser)]
#[command(name = "mint")]
#[command(about = "Audio processing CLI: render masters into distribution targets.")]
pub struct Args {
    /// Path to the TOML configuration. Defaults to ./mint.toml (bootstrapped if missing).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Input files and/or glob patterns.
    #[arg(required = true)]
    inputs: Vec<String>,

    /// Render only the named target(s); repeat the flag for several. Default: all targets.
    #[arg(long)]
    target: Vec<String>,

    /// Show planned actions without processing files.
    #[arg(long)]
    dry_run: bool,

    /// Number of parallel worker jobs (default: available CPU threads).
    #[arg(long)]
    jobs: Option<usize>,

    /// Seed for deterministic dither behavior.
    #[arg(long)]
    seed: Option<u64>,

    /// Stop after the first failure (default: collect all failures and continue).
    #[arg(long)]
    fail_fast: bool,

    /// Replace files that are already at the output paths, the input file included
    /// (equivalent to overwrite = true on every target).
    #[arg(long)]
    force: bool,

    /// Increase log verbosity.
    #[arg(short, long)]
    verbose: bool,

    /// Minimize output.
    #[arg(long)]
    quiet: bool,

    /// Emit the processing report (per-task metering + failures) as JSON to stdout
    /// instead of the human-readable report. Intended for QC automation.
    #[arg(long)]
    json: bool,
}

/// One unit of work: mint `input` into `target`.
struct Job<'a> {
    input: &'a Path,
    target: &'a ResolvedTarget,
}

pub fn run() -> Result<()> {
    let args = Args::parse();
    init_logging(args.verbose, args.quiet);

    let config_path = resolve_config_path(args.config.as_deref())?;
    let config = Config::from_path(&config_path)?;
    let targets = resolve_selected_targets(&config, &args.target, args.force)?;

    let inputs = resolve_inputs(&args.inputs)?;
    if inputs.is_empty() {
        bail!("no supported input files resolved from provided arguments");
    }

    // Pre-flight: the whole batch's plan, before any of it runs. Two jobs writing one path
    // is only visible from up here — `pipeline::process` sees one job at a time, and by the
    // time the second one looks at its destination the first has already written it (or, in
    // parallel, has not looked yet). Ahead of the `--dry-run` branch on purpose: a plan that
    // cannot be executed should not be printed as if it could.
    let probed = probe_inputs(&inputs);
    let planned = plan_outputs(&probed, &targets)?;
    config::check_planned_outputs(&planned, &inputs)?;

    // Keep stdout pure JSON when --json: route the config banner to stderr instead.
    if let Some(meta) = &config.meta
        && let Some(name) = &meta.name
        && !args.quiet
        && !args.json
    {
        println!("config: {name}");
    }

    if args.dry_run {
        return print_dry_run(&probed, &targets);
    }

    // Cross product: every input is rendered into every selected target.
    let jobs: Vec<Job> = inputs
        .iter()
        .flat_map(|input| {
            targets
                .iter()
                .map(move |target| Job { input, target })
        })
        .collect();

    let worker_count = args.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    });

    let mut reports: Vec<ProcessingReport> = Vec::new();
    let mut failures: Vec<TaskFailure> = Vec::new();
    let fail = |job: &Job, e: anyhow::Error| TaskFailure {
        target: job.target.name.clone(),
        input: job.input.display().to_string(),
        error: format!("{e:#}"),
    };

    if args.fail_fast || worker_count <= 1 {
        // Sequential path: strict ordering and fail-fast support.
        for job in &jobs {
            match pipeline::process(job.input, job.target, args.seed) {
                Ok(report) => reports.push(report),
                Err(err) => {
                    failures.push(fail(job, err));
                    if args.fail_fast {
                        break;
                    }
                }
            }
        }
    } else {
        // Parallel path: each (input, target) pair is an independent pipeline.
        if !args.quiet && !args.json {
            eprintln!("running {worker_count} parallel jobs over {} task(s)", jobs.len());
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(worker_count)
            .build()
            .context("failed to build thread pool")?;

        let batch: Vec<_> = pool.install(|| {
            use rayon::prelude::*;
            jobs.par_iter()
                .map(|job| pipeline::process(job.input, job.target, args.seed).map_err(|e| fail(job, e)))
                .collect()
        });

        for result in batch {
            match result {
                Ok(report) => reports.push(report),
                Err(failure) => failures.push(failure),
            }
        }
    }

    if args.json {
        let config_name = config.meta.as_ref().and_then(|m| m.name.as_deref());
        println!("{}", json_report::render(config_name, &reports, &failures)?);
    } else if !args.quiet {
        print_reports(&reports);
    }

    if !failures.is_empty() {
        // In JSON mode the failures are already in the document; keep stdout clean and
        // only signal via the non-zero exit. Otherwise list them on stderr.
        if !args.json {
            eprintln!("\n{} task(s) failed:", failures.len());
            for f in &failures {
                eprintln!("  [{}] {} => {}", f.target, f.input, f.error);
            }
        }
        bail!("processing completed with {} failure(s)", failures.len());
    }

    Ok(())
}

/// Config file looked up in the working directory when `--config` is omitted.
const DEFAULT_CONFIG: &str = "mint.toml";
const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../examples/mint.toml");

/// Pick the config path: the explicit `--config` if given, otherwise `./mint.toml`
/// when it exists. If missing, bootstrap one from `examples/mint.toml`.
fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    let default = PathBuf::from(DEFAULT_CONFIG);
    if default.is_file() {
        return Ok(default);
    }

    fs::write(&default, DEFAULT_CONFIG_TEMPLATE).with_context(|| {
        format!(
            "no config given and {DEFAULT_CONFIG} was missing; failed to create it from examples/mint.toml"
        )
    })?;

    eprintln!(
        "no {DEFAULT_CONFIG} found; created one from examples/mint.toml"
    );

    Ok(default)
}

/// Resolve the targets requested via `--target` (or all of them), applying the
/// global `--force` overwrite override.
fn resolve_selected_targets(
    config: &Config,
    requested: &[String],
    force: bool,
) -> Result<Vec<ResolvedTarget>> {
    let names = if requested.is_empty() {
        config.target_names()
    } else {
        for name in requested {
            if !config.targets.contains_key(name) {
                bail!(
                    "unknown target '{name}'; available: {}",
                    config.target_names().join(", ")
                );
            }
        }
        requested.to_vec()
    };

    names
        .iter()
        .map(|name| {
            let mut target = config.resolve_target(name)?;
            if force {
                target.overwrite = true;
            }
            Ok(target)
        })
        .collect()
}

/// An input with its header read, so the batch pays one probe per file no matter how many
/// consumers want the rate: the pre-flight, the dry-run's chain, and the dry-run's sizes.
///
/// `probe` is an `Err` for a file whose header cannot be read. That is deliberately **not**
/// fatal to the batch: an unreadable input has always been one task's failure, with the rest
/// of the run continuing and `--json` still producing a document, and making the pre-flight
/// strict about it would have quietly turned one bad file into "nothing ran".
struct ProbedInput<'a> {
    path: &'a Path,
    probe: Result<io_read::SourceProbe>,
}

/// Read every input's header. Cheap next to what follows (a decode, or nothing at all in a
/// dry run) and it is what makes the batch's plan knowable before the batch starts.
fn probe_inputs(inputs: &[PathBuf]) -> Vec<ProbedInput<'_>> {
    inputs
        .iter()
        .map(|path| ProbedInput {
            path,
            probe: io_read::probe_source(path)
                .with_context(|| format!("failed to probe {}", path.display())),
        })
        .collect()
}

/// Where every (input, target) pair will write. The delivery rate — and therefore `{rate}`
/// in the name — can follow the source, which is why this needs the probes.
///
/// Inputs whose header could not be read contribute nothing: their output path is not
/// computable, so they cannot be shown to collide with anything. They still become jobs, and
/// `pipeline::process` re-raises the same probe error as that job's own failure.
fn plan_outputs<'a>(
    probed: &[ProbedInput<'a>],
    targets: &'a [ResolvedTarget],
) -> Result<Vec<PlannedOutput<'a>>> {
    let mut planned = Vec::with_capacity(probed.len() * targets.len());
    for input in probed {
        let Ok(probe) = &input.probe else { continue };
        for target in targets {
            planned.push(PlannedOutput {
                input: input.path,
                target: &target.name,
                output: target
                    .render_output_path(input.path, target.delivery_rate(probe.sample_rate))?,
            });
        }
    }
    Ok(planned)
}

fn print_dry_run(probed: &[ProbedInput<'_>], targets: &[ResolvedTarget]) -> Result<()> {
    println!(
        "dry-run: {} file(s) x {} target(s)",
        probed.len(),
        targets.len()
    );
    for ProbedInput { path: input, probe } in probed {
        // A file the dry run cannot read is reported as such rather than skipped: the run
        // would fail on it too, and that is the thing worth knowing in advance.
        let probe = match probe {
            Ok(probe) => probe,
            Err(e) => {
                println!("\n{} — cannot be read: {e:#}", input.display());
                continue;
            }
        };
        let source_rate = probe.sample_rate;
        println!("\n{} [{} Hz]", input.display(), source_rate);
        for target in targets {
            let output = target.render_output_path(input, target.delivery_rate(source_rate))?;
            // Sizes are the reason this is worth printing at all: a five-minute DSD512
            // stereo track is ~1.7 GB, which is better learned here than after the encode.
            let size = probe
                .frames
                .zip(probe.channels)
                .and_then(|(frames, channels)| {
                    target.estimated_output_bytes(source_rate, frames, channels)
                })
                .map(|est| format!("  ({}{})", est.qualifier(), fmt_bytes(est.bytes())))
                .unwrap_or_default();
            // Whether a file is about to be lost is the one thing a dry run has no business
            // being quiet about — and the only place the user can learn it before it happens.
            // `symlink_metadata`, matching `pipeline::guard_output_path`: a dangling symlink
            // is something the rename replaces, and `exists()` would call it free.
            let replaces = if std::fs::symlink_metadata(&output).is_ok() {
                if target.overwrite {
                    "  (replaces existing)"
                } else {
                    "  (exists — will fail without --force)"
                }
            } else {
                ""
            };
            println!("  [{}] -> {}{}{}", target.name, output.display(), size, replaces);
            for step in target.describe(source_rate) {
                println!("      - {step}");
            }
            // The compatibility rules a config cannot be validated against because they
            // depend on the input: the delivered rate for a target that follows the source,
            // and the channel count. Same function the pipeline refuses the job with, so
            // the dry-run cannot promise a file the run will decline to write.
            if let Some(reason) = target.delivery_blocker(source_rate, probe.channels) {
                println!("      ! {reason} — this target will fail");
            }
        }
    }
    Ok(())
}

fn print_reports(reports: &[ProcessingReport]) {
    println!("processed {} task(s)", reports.len());
    for report in reports {
        let s = &report.source;
        let d = &report.delivered;

        // How much the limiter squashed to hit the loudness target (omitted when none):
        // peak reduction, plus mean depth and how much of the signal was touched —
        // one shaved needle and wall-to-wall limiting have the same peak figure.
        let limiting = match report.limiter_gain_reduction_db {
            Some(gr) if gr > 0.0 => {
                let detail = match (report.limiter_avg_gain_reduction_db, report.limiter_active_percent)
                {
                    (Some(avg), Some(pct)) => format!(" (avg -{avg:.1} dB over {pct:.1}%)"),
                    _ => String::new(),
                };
                format!(" | limited peak -{gr:.1} dB{detail}")
            }
            _ => String::new(),
        };
        // Naming the *requested* mode when flat TPDF ran is the whole point of reporting
        // it: a target that asked for `adaptive` and got TPDF because the format is not
        // s16 looked identical to one that asked for TPDF.
        let dither_tag = match (report.shaping_curve, report.dithered, report.dither_mode) {
            (Some(curve), _, _) => format!(" +shaped({curve})"),
            (None, true, Some(mode)) if mode != "tpdf" => {
                format!(" +dither(tpdf, {mode} skipped)")
            }
            (None, true, _) => " +dither".to_string(),
            (None, false, _) => String::new(),
        };

        let bwf_tag = if report.bwf { " (bwf)" } else { "" };
        println!("  [{}] {} -> {}", report.target, report.input, report.output);
        println!(
            "    {} -> {} Hz | {}/{}{}{}{}",
            report.in_rate, report.out_rate, report.codec, report.format, bwf_tag, dither_tag, limiting,
        );
        // What shipped and what it cost: shape, real size on disk, and the two settings a
        // re-render needs to reproduce it. `seed` absent means it cannot be reproduced.
        let mut facts = format!(
            "    {} ch | {:.1} s | {}",
            report.channels,
            duration_seconds(report.frames, report.out_rate),
            fmt_bytes(report.file_bytes),
        );
        if let Some(quality) = report.src_quality {
            facts.push_str(&format!(" | src {quality}"));
        }
        if let Some(seed) = report.seed {
            facts.push_str(&format!(" | seed {seed}"));
        }
        facts.push_str(&format!(" | in {:.2} s", report.elapsed_ms as f64 / 1000.0));
        println!("{facts}");
        // The DSD stage's own figures. Peak modulation is the level QC number for a 1-bit
        // deliverable — 50 % is the SACD 0 dB reference — and the overload count is the
        // only place a loop that was driven past what it can track becomes visible.
        if let Some(d) = &report.dsd {
            println!(
                "    {} via {} Hz x{} | order {} | peak modulation {:.1}% | overload {} | {}",
                d.rate,
                d.intermediate_pcm_rate,
                d.interpolation_factor,
                d.modulator_order,
                d.max_modulation_percent,
                d.overload_samples,
                fmt_bytes(d.file_bytes),
            );
        }
        // The static gain the render applied, which the before/after LUFS pair does not
        // show: the two figures can be identical on a file that was moved 6 dB and on one
        // that was not touched at all. The post-SRC correction is called out separately
        // because it measures something else — loudness the band limit removed.
        let gain = match (report.loudness_gain_db, report.post_resample_gain_db) {
            (Some(g), Some(p)) if p.abs() >= 0.005 => {
                format!("  gain {g:+.2} dB ({p:+.2} post-SRC)")
            }
            (Some(g), _) => format!("  gain {g:+.2} dB"),
            (None, _) => String::new(),
        };
        // Loudness metering, source -> delivered (integrated + range), plus the loudest
        // delivered momentary/short-term windows.
        println!(
            "    LUFS {} -> {} (I)  LRA {} -> {} LU  M {}  S {} LUFS{}",
            fmt_db(s.integrated_lufs),
            fmt_db(d.integrated_lufs),
            fmt_db(s.loudness_range_lu),
            fmt_db(d.loudness_range_lu),
            fmt_db(d.max_momentary_lufs),
            fmt_db(d.max_short_term_lufs),
            gain,
        );
        // Peak / crest metering of the delivered signal.
        println!(
            "    TP {} -> {} dBTP  PLR {}  PSR {} dB",
            fmt_db(s.true_peak_dbtp),
            fmt_db(d.true_peak_dbtp),
            fmt_db(d.plr()),
            fmt_db(d.psr()),
        );
        for w in &report.warnings {
            println!("    warn: {w}");
        }
    }
    let warned = reports.iter().filter(|r| !r.warnings.is_empty()).count();
    if warned > 0 {
        println!("({warned} task(s) with warnings)");
    }
}

fn resolve_inputs(raw_inputs: &[String]) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    let mut seen = HashSet::new();

    // Deduped by identity, not by spelling: `mint song.wav ./song.wav` — or, more often, a
    // glob whose matches overlap an explicit argument — named one file twice and got two
    // jobs racing onto one output. `canonicalize` needs the file to exist, which every input
    // here does; if it fails anyway, fall back to the path as written rather than dropping
    // the input.
    //
    // Announced rather than silent, because it is not always what the user meant: two
    // spellings of one file with `{stem}` in the naming would have rendered the same audio
    // under two names, and this collapses them to one.
    let mut accept = |path: PathBuf, resolved: &mut Vec<PathBuf>| {
        if !is_supported_input(&path) {
            return;
        }
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            resolved.push(path);
        } else {
            eprintln!("skipping {}: already named by another argument", path.display());
        }
    };

    for token in raw_inputs {
        if looks_like_glob(token) {
            let mut matches_for_pattern = 0usize;
            for entry in glob::glob(token).with_context(|| format!("invalid glob pattern: {token}"))? {
                let path = entry.with_context(|| format!("failed resolving glob path: {token}"))?;
                accept(path, &mut resolved);
                matches_for_pattern += 1;
            }

            if matches_for_pattern == 0 {
                bail!("glob produced no matches: {token}");
            }
        } else {
            let path = PathBuf::from(token);
            if !path.exists() {
                bail!("input path does not exist: {}", path.display());
            }
            accept(path, &mut resolved);
        }
    }

    Ok(resolved)
}

fn looks_like_glob(input: &str) -> bool {
    input.contains('*') || input.contains('?') || input.contains('[')
}

fn init_logging(verbose: bool, quiet: bool) {
    let level = if quiet {
        "error"
    } else if verbose {
        "debug"
    } else {
        "info"
    };

    let env = env_logger::Env::default().default_filter_or(level);
    let _ = env_logger::Builder::from_env(env).is_test(false).try_init();
}
