//! Surgical edits to a `mint.toml`, spike gate S6.
//!
//! **`mint.toml` IS the project file.** The GUI owns no config format, which means every
//! write has to leave a document `Config::from_path` still reads and a human still
//! recognises. `examples/mint.toml` is 92 lines of which roughly 55 are pedagogy, and it is
//! `include_str!`-embedded as the bootstrap template — a Save that eats the comments has
//! eaten the onboarding documentation.
//!
//! Hence the rules, all of them load-bearing:
//!
//! - **Never re-serialize the document wholesale.** `Config.targets` is a `BTreeMap`, so a
//!   serde round-trip would silently alphabetise the user's targets on every save.
//! - **Touch only the key that changed.** Not the table around it, not the blank line after
//!   it, not the comment above it.
//! - **Removing a value removes the key.** It does not write the default in its place —
//!   those are different documents with the same meaning today and possibly different
//!   meanings after a `builtin_fallback` change.
//! - **Never expand a preset.** `preset = "cd"` stays one line, not 22.
//!
//! What makes this checkable rather than hopeful is that a diff is a byte comparison: the
//! test in `gui/tests/toml_round_trip.rs` pokes every field type and asserts that every
//! byte outside the touched key is unchanged.

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item, Value};

/// A parsed `mint.toml` that remembers its own formatting.
pub struct ConfigDocument {
    doc: DocumentMut,
}

impl ConfigDocument {
    pub fn parse(text: &str) -> Result<Self> {
        let doc = text.parse::<DocumentMut>().context("not valid TOML")?;
        Ok(Self { doc })
    }

    /// The document as it now stands. Byte-identical to the input except where a setter
    /// was called.
    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// Validate the current text through mint's own loader. The GUI never decides whether a
    /// document is acceptable; it asks the library, which is the only thing that knows the
    /// four-layer merge and its exceptions.
    pub fn validate(&self) -> Result<mint::config::Config> {
        mint::config::Config::from_toml_str(&self.text())
    }

    pub fn target_names(&self) -> Vec<String> {
        self.doc
            .get("target")
            .and_then(Item::as_table)
            .map(|t| t.iter().map(|(name, _)| name.to_string()).collect())
            .unwrap_or_default()
    }

    /// What this target's own table says about `field`, exactly as it is written.
    ///
    /// The inspector's controls are seeded from here rather than from
    /// `FieldOrigin::stated`, and the difference is not pedantry: `stated` renders a float
    /// for a *reader* (`lufs` comes back as `{:.1}`), so a control seeded from it would show
    /// −14.3 for a document that says −14.35 and quietly write the rounded value the first
    /// time the user dragged it. Provenance comes from the library; the number a widget is
    /// holding comes from the bytes on disk.
    ///
    /// Only the target's own layer, which is what a control on this target may edit — what
    /// the value *resolves* to through the four layers is [`mint::config::Config`]'s answer
    /// and is never guessed at here.
    pub fn get(&self, target: &str, field: &str) -> Option<&Value> {
        self.doc
            .get("target")?
            .get(target)?
            .get(field)
            .and_then(Item::as_value)
    }

    /// Byte range of a top-level table, for the TOML drawer to scroll to.
    ///
    /// `toml_edit` only keeps spans for a document it parsed and has not been asked to
    /// change, which is exactly the case here: `AppState` re-parses from the buffer on every
    /// edit, so the span is always of the text currently on screen. `None` for a table that
    /// is not in the document — a `[defaults]` nobody wrote has nowhere to jump to.
    pub fn table_span(&self, table: &str) -> Option<std::ops::Range<usize>> {
        self.doc.get(table)?.span()
    }

    /// Set one field on one target, creating the key (or the whole `[target.NAME]` table)
    /// if it is not there yet.
    ///
    /// The decor dance is the whole point: `table[key] = value` in `toml_edit` replaces the
    /// value *and* its surrounding whitespace, so ` = 44100  # CD rate` would come back as
    /// `=44100`. Lifting the old decor across keeps the user's alignment and any trailing
    /// comment exactly where they were.
    pub fn set(&mut self, target: &str, field: &str, new: Value) -> Result<()> {
        let table = self.target_table_mut(target)?;
        let decor = table
            .get(field)
            .and_then(Item::as_value)
            .map(|existing| existing.decor().clone());
        table[field] = Item::Value(new);
        if let Some(decor) = decor
            && let Some(value) = table[field].as_value_mut()
        {
            *value.decor_mut() = decor;
        } else if let Some(value) = table[field].as_value_mut() {
            // A brand-new key: give it the one space either side that every hand-written
            // line in the file already uses, rather than toml_edit's bare default.
            value.decor_mut().set_prefix(" ");
        }
        Ok(())
    }

    /// Remove a field, so the target falls back to whatever the layers below it say.
    /// Returns whether anything was there. The comment above the key goes with it — that
    /// comment described the key.
    pub fn clear(&mut self, target: &str, field: &str) -> Result<bool> {
        let table = self.target_table_mut(target)?;
        Ok(table.remove(field).is_some())
    }

    fn target_table_mut(&mut self, target: &str) -> Result<&mut toml_edit::Table> {
        let targets = self
            .doc
            .entry("target")
            .or_insert_with(|| Item::Table(implicit_table()))
            .as_table_mut()
            .context("[target] is not a table")?;
        // `[target.cd]` is a dotted sub-table of an implicit `[target]`; marking the parent
        // implicit is what keeps a bare `[target]` header from being written out.
        targets.set_implicit(true);
        let entry = targets.entry(target).or_insert_with(|| Item::Table(toml_edit::Table::new()));
        match entry.as_table_mut() {
            Some(table) => Ok(table),
            None => bail!("[target.{target}] is not a table"),
        }
    }
}

fn implicit_table() -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    table.set_implicit(true);
    table
}

/// Spell a mint config value the way the TOML expects it.
///
/// Enums go through their `Display`, which `tests/enum_domains.rs` pins as being exactly
/// the spelling the parser accepts — so a value written here is a value mint can read back.
pub fn enum_value(spelling: impl std::fmt::Display) -> Value {
    Value::from(spelling.to_string())
}
