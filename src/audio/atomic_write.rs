//! Write-to-temp-then-rename, so a write that fails partway through cannot destroy the
//! deliverable that was already on disk.
//!
//! Every writer in [`crate::audio`] used to open the final path directly
//! (`WavWriter::create`, `fs::write`, `File::create`), all of which truncate in place. A
//! disk-full, a decode error surfacing late, or a Ctrl-C therefore replaced a good master
//! with a short one — the failure mode a mastering tool can least afford, because the
//! output of the *previous* run is often the only copy of a render that took minutes.
//!
//! The staging file is a sibling of the target, so [`fs::rename`] stays within one
//! filesystem and replaces the destination in one step on both platforms std supports here
//! (POSIX `rename(2)`; Windows `MoveFileEx`/`FileRenameInfoEx` with replace-if-exists).
//! Either the old file is there or the new one is — never a truncated hybrid.
//!
//! Four things worth knowing before trusting this more than it deserves:
//!
//! - **No `fsync`.** The guarantee on offer is "an error never destroys the old file", not
//!   "the new file survives a power cut". Syncing a multi-gigabyte DSD render to buy the
//!   second one is a cost every normal run would pay for a case a mastering workflow does
//!   not care about.
//! - **No TOCTOU protection.** `pipeline`'s `overwrite` check runs before the DSP and the
//!   rename happens minutes later. Two mint processes racing on one path are a user error;
//!   within one run the pre-flight in `cli.rs` catches it.
//! - **Replacing is not editing in place.** The delivered file gets the staging file's
//!   mode, ownership and xattrs, not the replaced file's, and a symlink at the output path
//!   is replaced by a regular file rather than written through. A prepared delivery folder
//!   with pre-set permissions or symlinks behaves differently than it used to.
//! - **A killed process leaves its staging file behind**, and nothing sweeps them. The name
//!   says what it is and what wrote it — `.<final name>.<pid>.<n>.mint-tmp` — and
//!   `cli::is_supported_input` rejects that extension, so a leftover cannot re-enter a later
//!   batch through a glob. On a big DSD render it is still gigabytes worth noticing.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

/// Distinguishes staging files created within one process. The pid alone is not enough:
/// `cli.rs` runs jobs under rayon, so several targets can be staging siblings of the same
/// directory at the same moment, and two inputs can even share a final file name.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Bytes of the final file name a staging name may carry. The rest of the budget goes to the
/// leading dot, the pid, the counter and `.mint-tmp` — generously, since a pid can be seven
/// digits and the counter is unbounded — leaving the whole name inside a 255-byte `NAME_MAX`.
const NAME_BUDGET: usize = 200;

/// First `max` bytes of `name`, on a UTF-8 character boundary when the name is UTF-8.
///
/// Splitting an `OsStr` by bytes is only safe on Unix, where it is just bytes. Everywhere
/// else this stays with the lossy UTF-8 view, which is fine: the truncated name is a human
/// hint on a temporary file, not something anything reads back.
fn truncate_os_str(name: &std::ffi::OsStr, max: usize) -> std::ffi::OsString {
    if name.len() <= max {
        return name.to_os_string();
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        // Back off to a character boundary so a truncated UTF-8 name stays valid UTF-8;
        // continuation bytes are 0b10xxxxxx.
        let bytes = name.as_bytes();
        let mut end = max;
        while end > 0 && (bytes[end] & 0xC0) == 0x80 {
            end -= 1;
        }
        std::ffi::OsString::from_vec(bytes[..end].to_vec())
    }
    #[cfg(not(unix))]
    {
        let lossy = name.to_string_lossy();
        let mut end = max.min(lossy.len());
        while end > 0 && !lossy.is_char_boundary(end) {
            end -= 1;
        }
        std::ffi::OsString::from(&lossy[..end])
    }
}

/// A file being written to a temporary sibling of `final_path`.
///
/// Create the real file at [`path`](Self::path), write it however that writer likes, then
/// call [`commit`](Self::commit). Dropping without committing removes the staging file, so
/// the `?` on any intervening error cleans up on its way out.
#[derive(Debug)]
pub struct Staged {
    temp: PathBuf,
    final_path: PathBuf,
    committed: bool,
}

impl Staged {
    /// Reserve a staging path next to `final_path`. Creates nothing yet.
    pub fn new(final_path: &Path) -> Result<Self> {
        let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
        let name = final_path
            .file_name()
            .with_context(|| format!("output path has no file name: {}", final_path.display()))?;

        // Assembled as an `OsString` rather than through `format!`, so a path this crate can
        // otherwise write is not turned into an error by being non-UTF-8.
        //
        // The final name is only carried along so a leftover staging file is recognizable,
        // and it is truncated to keep room for the suffix: the staging name has to fit the
        // same `NAME_MAX` (255 on ext4/APFS/NTFS) as the real one, and a deliverable named
        // right up against that limit must not become unwritable just because staging added
        // twenty characters. Uniqueness comes from the pid and the counter, not the name.
        let mut temp_name = std::ffi::OsString::with_capacity(NAME_BUDGET + 32);
        temp_name.push(".");
        temp_name.push(truncate_os_str(name, NAME_BUDGET));
        temp_name.push(format!(
            ".{}.{}.mint-tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        Ok(Self {
            temp: parent.join(temp_name),
            final_path: final_path.to_path_buf(),
            committed: false,
        })
    }

    /// Where the writer must actually write.
    pub fn path(&self) -> &Path {
        &self.temp
    }

    /// Move the finished staging file onto the final path.
    pub fn commit(mut self) -> Result<()> {
        fs::rename(&self.temp, &self.final_path).with_context(|| {
            format!(
                "failed to move {} into place at {}",
                self.temp.display(),
                self.final_path.display()
            )
        })?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed {
            // Best effort: the write that is unwinding past us already has the real error,
            // and a failure to clean up must not mask it.
            let _ = fs::remove_file(&self.temp);
        }
    }
}

/// [`fs::write`] with the staging dance — the one-shot form for writers that serialize to a
/// `Vec<u8>` first (`encode_wav_f64`, the bext splice, FLAC, MP3).
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let staged = Staged::new(path)?;
    fs::write(staged.path(), bytes)
        .with_context(|| format!("failed to write output: {}", path.display()))?;
    staged.commit()
}

/// [`File::create`] with the staging dance, for writers that stream. The caller owns the
/// `File` and must keep the `Staged` alive until it has finished writing, then commit it.
pub fn create(path: &Path) -> Result<(File, Staged)> {
    let staged = Staged::new(path)?;
    let file = File::create(staged.path())
        .with_context(|| format!("failed to create output: {}", path.display()))?;
    Ok((file, staged))
}
