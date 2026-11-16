//! Editions: the way byld changes what an accepted program means, or refuses
//! one it used to accept, without breaking the programs already written.
//!
//! A project names its edition with `edition = "2026"` in `byard.toml`. A
//! project that names none is the baseline edition, the one the language had
//! before editions existed, and it keeps that behaviour for as long as that
//! edition does. `byard fix` moves a project to the next edition: it rewrites
//! the sources so they mean what they meant, through the formatter's printer,
//! and then names the new edition in the manifest.
//!
//! A change that needs an edition is marked **(edition)** in
//! `docs/language/roadmap.md`. Today the two editions differ by nothing: 2027
//! is the one those changes will land in, and the machinery is here first so
//! that the first of them does not have to be written as a patch release that
//! breaks programs in the wild.

use std::fmt;

use crate::fmt::{FmtError, format};

/// A byld edition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Edition {
    /// The language as it was before editions. What a manifest with no
    /// `edition` key is.
    #[default]
    E2026,
    /// The edition the changes marked **(edition)** in the roadmap land in.
    E2027,
}

/// A rewrite that keeps a program of one edition meaning what it meant in the
/// next: the text of a file in, the text it should have out, or `None` when
/// the file has nothing to change.
type Migration = fn(&str) -> Option<String>;

impl Edition {
    /// The edition of a project that names none.
    pub const BASELINE: Self = Self::E2026;
    /// The newest edition there is.
    pub const LATEST: Self = Self::E2027;
    /// Every edition, oldest first.
    pub const ALL: [Self; 2] = [Self::E2026, Self::E2027];

    /// How the manifest writes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::E2026 => "2026",
            Self::E2027 => "2027",
        }
    }

    /// The edition `written` names, or what to say when there is none.
    ///
    /// # Errors
    ///
    /// The message names the editions there are.
    pub fn parse(written: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|e| e.as_str() == written)
            .ok_or_else(|| {
                let known: Vec<String> = Self::ALL.iter().map(|e| format!("`{e}`")).collect();
                format!(
                    "edition `{written}` is not one this byard knows; the editions are {}",
                    known.join(", ")
                )
            })
    }

    /// The edition after this one, if there is one.
    #[must_use]
    pub fn next(self) -> Option<Self> {
        Self::ALL.into_iter().find(|e| *e > self)
    }

    /// The rewrites that take a program from this edition to the next.
    fn migrations(self) -> &'static [Migration] {
        match self {
            // 2027 changes nothing yet.
            Self::E2026 | Self::E2027 => &[],
        }
    }

    /// `source`, written for this edition, as it is written for the next, or
    /// `None` when it needs no change. A file that changes is printed by the
    /// formatter, so the result is laid out as `byard fmt` lays it out.
    ///
    /// # Errors
    ///
    /// A file that does not parse cannot be rewritten, and is not touched.
    pub fn migrate(self, source: &str) -> Result<Option<String>, FmtError> {
        let mut text = source.to_string();
        let mut changed = false;
        for migration in self.migrations() {
            if let Some(next) = migration(&text) {
                text = next;
                changed = true;
            }
        }
        if !changed {
            return Ok(None);
        }
        format(&text).map(Some)
    }
}

impl fmt::Display for Edition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edition_is_read_as_it_is_written() {
        assert_eq!(Edition::parse("2026"), Ok(Edition::E2026));
        assert_eq!(Edition::parse("2027"), Ok(Edition::E2027));
        let err = Edition::parse("2030").expect_err("not an edition");
        assert!(
            err.contains("`2030`") && err.contains("`2026`, `2027`"),
            "{err}"
        );
    }

    #[test]
    fn editions_are_in_order_and_the_last_has_no_next() {
        assert_eq!(Edition::BASELINE.next(), Some(Edition::E2027));
        assert_eq!(Edition::LATEST.next(), None);
        assert_eq!(Edition::default(), Edition::BASELINE);
    }

    #[test]
    fn a_file_that_needs_no_change_is_left_as_written() {
        let source = "View Main()   {  Text(\"a\")  }";
        assert_eq!(Edition::E2026.migrate(source), Ok(None));
    }
}
