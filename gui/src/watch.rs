//! File-watch on `mint.toml`, because the GUI is not the only thing that writes it.
//!
//! Proposal §5.2: *"treat external edits as the normal case, not a conflict"*. That framing
//! is the design. `mint.toml` **is** the project file (Part III §4) and mixed CLI/GUI is the
//! expected mode — a branch is switched, a colleague's edit is pulled, the file is fixed in
//! vim while the workbench is open on the other monitor. A GUI that only notices on restart
//! is a GUI showing a plan for a document that no longer exists.
//!
//! Two things this module gets right that a naive watch does not:
//!
//! - **It watches the directory, not the file.** Every careful writer replaces a file by
//!   renaming a sibling onto it — text editors, `git checkout`, and mint's own
//!   `audio::atomic_write`. A watch on the path follows the *inode*, so the first such save
//!   unhooks it and it goes on watching bytes nobody will ever write again. Watching the
//!   parent non-recursively and filtering by file name survives that.
//! - **It reports "something happened", never "here is the new content".** What the change
//!   *means* is [`crate::state::AppState::absorb_external_change`]'s to decide, because the
//!   answer depends on whether this window has unsaved edits — and because a watcher that
//!   fired on our own save would otherwise reload the buffer out from under the cursor. The
//!   comparison that makes a self-write invisible is a plain text comparison, done there.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use anyhow::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

pub struct ConfigWatcher {
    rx: Receiver<()>,
    /// Dropping the watcher stops the OS notifications, so it is held for its lifetime and
    /// never read.
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// Start watching the directory `path` lives in. The `Context` is what makes this useful
    /// at all: egui only repaints on demand, so an event that does not wake the app is an
    /// event the app learns about the next time the mouse moves.
    pub fn new(path: &Path, ctx: egui::Context) -> Result<Self> {
        let (tx, rx) = channel();
        let directory = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            // A bare `mint.toml`, which is what the CLI's own default argument produces.
            _ => PathBuf::from("."),
        };
        let name = path.file_name().map(PathBuf::from);

        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let Ok(event) = event else { return };
            // Non-recursive still means every file in the directory, and a render writing
            // `./out/cd/take-01.wav` beside the config would otherwise wake the app once per
            // staging file.
            let ours = event
                .paths
                .iter()
                .any(|changed| changed.file_name().map(PathBuf::from) == name);
            if !ours {
                return;
            }
            if tx.send(()).is_ok() {
                ctx.request_repaint();
            }
        })?;
        watcher.watch(&directory, RecursiveMode::NonRecursive)?;
        Ok(Self { rx, _watcher: watcher })
    }

    /// Whether anything has happened since the last ask.
    ///
    /// Coalescing, not counting: one save is several events on every platform (a create, a
    /// rename and two metadata writes is a normal editor's idea of saving one file), and what
    /// the caller does about it is idempotent — re-read the file and compare.
    ///
    /// Both error arms end the drain, and they mean different things: `Empty` is the usual
    /// one, while `Disconnected` says the watcher thread is gone and there will never be
    /// another event. Neither is reported — a lost watch costs the convenience it provides
    /// and nothing else, and saying so once per frame would be noise.
    pub fn changed(&self) -> bool {
        let mut changed = false;
        while let Ok(()) = self.rx.try_recv() {
            changed = true;
        }
        changed
    }
}
