//! What the whole app looks at: one document, and mint's verdict on it.
//!
//! The single rule this type exists to enforce is proposal §5.2's first one — **the GUI
//! never re-derives**. There is no parallel model of a target here, no cached "effective
//! rate", no mirrored validation. There is the document text, the `Config` mint made of it
//! (or the error mint refused it with), and nothing else. Every screen asks the library
//! again rather than asking this struct for a conclusion.

use std::path::{Path, PathBuf};
use std::time::Instant;

use mint::config::Config;

use crate::write_back::ConfigDocument;

pub struct AppState {
    pub path: PathBuf,
    /// The document as edited. `mint.toml` *is* the project file, so this is the project.
    pub text: String,
    /// Reparsed on every change; `None` while the text is not even valid TOML.
    pub doc: Option<ConfigDocument>,
    /// mint's own verdict. The error is kept as mint formatted it — the GUI does not
    /// paraphrase, because the paraphrase is where a second opinion creeps in.
    pub status: Result<Config, String>,
    /// How long the last parse + validate took. The spike question S2 asks is whether this
    /// can run per frame; a number answers it better than an opinion.
    pub validate_micros: u128,
    pub selected: Option<String>,
    /// Files the user dropped on the window. No file dialog in the spike — a drop is
    /// dependency-free and is how a mastering engineer would reach for it anyway.
    pub inputs: Vec<PathBuf>,
    pub dirty: bool,
}

impl AppState {
    pub fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_else(|err| {
            format!("# could not read {}: {err}\n", path.display())
        });
        let mut state = Self {
            path: path.to_path_buf(),
            text,
            doc: None,
            status: Err(String::new()),
            validate_micros: 0,
            selected: None,
            inputs: Vec::new(),
            dirty: false,
        };
        state.revalidate();
        state.selected = state.target_names().first().cloned();
        state
    }

    /// Reparse and re-ask mint whether the document is acceptable.
    pub fn revalidate(&mut self) {
        let started = Instant::now();
        self.doc = ConfigDocument::parse(&self.text).ok();
        self.status = match &self.doc {
            // Through the document rather than the raw text so that what is validated is
            // exactly what a Save would write.
            Some(doc) => doc.validate().map_err(|err| format!("{err:#}")),
            None => Config::from_toml_str(&self.text).map_err(|err| format!("{err:#}")),
        };
        self.validate_micros = started.elapsed().as_micros();
    }

    /// Names in the document's own order. `Config::target_names` sorts (its `targets` is a
    /// `BTreeMap`); the document order is the user's, and it is the one to show.
    pub fn target_names(&self) -> Vec<String> {
        match &self.doc {
            Some(doc) => doc.target_names(),
            None => Vec::new(),
        }
    }

    pub fn config(&self) -> Option<&Config> {
        self.status.as_ref().ok()
    }

    pub fn error(&self) -> Option<&str> {
        self.status.as_ref().err().map(String::as_str)
    }

    /// Apply an edit to the document and re-validate. Every field edit in the form goes
    /// through here, so the write-back path is exercised by ordinary use rather than only
    /// by a Save button that might be tested less.
    pub fn edit(&mut self, apply: impl FnOnce(&mut ConfigDocument) -> anyhow::Result<()>) {
        let Some(doc) = self.doc.as_mut() else { return };
        if apply(doc).is_ok() {
            self.text = doc.text();
            self.dirty = true;
            self.revalidate();
        }
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        std::fs::write(&self.path, &self.text)?;
        self.dirty = false;
        Ok(())
    }

    /// The CLI line that reproduces what is on screen. Nearly free, and it is what keeps a
    /// mixed CLI/GUI workflow reconciled (proposal §5.3).
    pub fn cli_command(&self) -> String {
        let mut line = format!("mint --config {}", self.path.display());
        if let Some(target) = &self.selected {
            line.push_str(&format!(" --target {target}"));
        }
        for input in &self.inputs {
            line.push_str(&format!(" {}", input.display()));
        }
        if self.inputs.is_empty() {
            line.push_str(" \"masters/*.wav\"");
        }
        line
    }
}
