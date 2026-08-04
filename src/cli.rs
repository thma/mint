use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::audio::{io_read, io_write_dsd};
use crate::config::{Config, ResolvedTarget};
use crate::json_report;
use crate::pipeline::{self, ProcessingReport, TaskFailure};

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

    /// Allow output path to match input path.
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

    // Keep stdout pure JSON when --json: route the config banner to stderr instead.
    if let Some(meta) = &config.meta
        && let Some(name) = &meta.name
        && !args.quiet
        && !args.json
    {
        println!("config: {name}");
    }

    if args.dry_run {
        return print_dry_run(&inputs, &targets);
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

fn print_dry_run(inputs: &[PathBuf], targets: &[ResolvedTarget]) -> Result<()> {
    println!(
        "dry-run: {} file(s) x {} target(s)",
        inputs.len(),
        targets.len()
    );
    for input in inputs {
        // Header only: the planned chain needs the rate, the planned size needs the frame
        // and channel counts, and none of it is worth a decode.
        let probe = io_read::probe_source(input)
            .with_context(|| format!("failed to probe {}", input.display()))?;
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
            println!("  [{}] -> {}{}", target.name, output.display(), size);
            for step in target.describe(source_rate) {
                println!("      - {step}");
            }
            // The DSF writer's channel limit is the one thing a config cannot be validated
            // against at load time (it does not know the input), so a 7.1 source into a
            // DSD target currently fails only after the loudness and resample work.
            if target.codec.is_dsd()
                && let Some(channels) = probe.channels
                && channels > io_write_dsd::MAX_CHANNELS
            {
                println!(
                    "      ! {channels} channels exceeds the DSF limit of {} — this target will fail",
                    io_write_dsd::MAX_CHANNELS
                );
            }
        }
    }
    Ok(())
}

/// Byte count for humans. `MB` is 10^6, matching how storage and DSD data rates are
/// quoted. Both ends need a step: DSD512 files run into the gigabytes, and a short clip
/// would otherwise be reported as "0.0 MB".
fn fmt_bytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.2} GB", mb / 1000.0)
    } else if mb < 1.0 {
        format!("{:.0} kB", bytes as f64 / 1000.0)
    } else {
        format!("{mb:.1} MB")
    }
}

/// Format a dB/LU/LUFS figure, or "n/a" when the meter returned a non-finite value
/// (silence, or a clip shorter than the meter's integration time).
fn fmt_db(v: f64) -> String {
    if v.is_finite() { format!("{v:.1}") } else { "n/a".to_string() }
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
        let dither_tag = match (report.shaping_curve, report.dithered) {
            (Some(curve), _) => format!(" +shaped({curve})"),
            (None, true) => " +dither".to_string(),
            (None, false) => String::new(),
        };

        let bwf_tag = if report.bwf { " (bwf)" } else { "" };
        println!("  [{}] {} -> {}", report.target, report.input, report.output);
        println!(
            "    {} -> {} Hz | {}/{}{}{}{}",
            report.in_rate, report.out_rate, report.codec, report.format, bwf_tag, dither_tag, limiting,
        );
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
        // Loudness metering, source -> delivered (integrated + range), plus the loudest
        // delivered momentary/short-term windows.
        println!(
            "    LUFS {} -> {} (I)  LRA {} -> {} LU  M {}  S {} LUFS",
            fmt_db(s.integrated_lufs),
            fmt_db(d.integrated_lufs),
            fmt_db(s.loudness_range_lu),
            fmt_db(d.loudness_range_lu),
            fmt_db(d.max_momentary_lufs),
            fmt_db(d.max_short_term_lufs),
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

    for token in raw_inputs {
        if looks_like_glob(token) {
            let mut matches_for_pattern = 0usize;
            for entry in glob::glob(token).with_context(|| format!("invalid glob pattern: {token}"))? {
                let path = entry.with_context(|| format!("failed resolving glob path: {token}"))?;
                if is_supported_input(&path) && seen.insert(path.clone()) {
                    resolved.push(path);
                }
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
            if is_supported_input(&path) && seen.insert(path.clone()) {
                resolved.push(path);
            }
        }
    }

    Ok(resolved)
}

fn looks_like_glob(input: &str) -> bool {
    input.contains('*') || input.contains('?') || input.contains('[')
}

fn is_supported_input(path: &Path) -> bool {
    let ext = match path.extension().and_then(|x| x.to_str()) {
        Some(ext) => ext.to_ascii_lowercase(),
        None => return false,
    };

    matches!(ext.as_str(), "wav" | "wave" | "aif" | "aiff")
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
