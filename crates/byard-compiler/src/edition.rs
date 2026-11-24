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
            // What 2027 changes that a rewrite can keep the meaning of: `a || b ? x : y`
            // read `a || (b ? x : y)`, and still does with the parentheses
            // written. (Overflow, graphemes and what a list shows in a text
            // have no rewrite: nothing says which programs relied on them.)
            Self::E2026 => &[parenthesise_ternary_after_or],
            Self::E2027 => &[],
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

/// `a || b ? x : y` was `a || (b ? x : y)` and is `(a || b) ? x : y` from 2027:
/// the old meaning is written out, so the program keeps it.
fn parenthesise_ternary_after_or(source: &str) -> Option<String> {
    use crate::parser::ast::{BinOp, Expr, Member};
    use crate::resolve::walk;

    let parsed = crate::parser::parse(source);
    if !parsed.errors.is_empty() {
        return None;
    }
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut note = |e: &mut Expr| {
        if let Expr::Binary {
            op: BinOp::Or,
            rhs,
            span,
            ..
        } = e
        {
            if let Expr::Ternary { span: t, .. } = rhs.as_ref() {
                if t.end == span.end {
                    spans.push((t.start as usize, t.end as usize));
                }
            }
        }
    };
    for view in &parsed.views {
        for member in &view.body {
            let mut copy: Member = member.clone();
            walk::member_exprs(&mut copy, &mut note);
        }
        for param in &view.params {
            if let Some(default) = &param.default {
                let mut copy = default.clone();
                walk::expr(&mut copy, &mut note);
            }
        }
    }
    for decl in &parsed.fns {
        let mut copy = decl.body.clone();
        walk::expr(&mut copy, &mut note);
    }
    for decl in &parsed.consts {
        let mut copy = decl.init.clone();
        walk::expr(&mut copy, &mut note);
    }
    if spans.is_empty() {
        return None;
    }
    // Insert from the end, so the places before it do not move.
    spans.sort_unstable();
    spans.dedup();
    let mut inserts: Vec<(usize, char)> = Vec::new();
    for (start, end) in spans {
        inserts.push((start, '('));
        inserts.push((end, ')'));
    }
    inserts.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut text = source.to_string();
    for (at, c) in inserts {
        text.insert(at, c);
    }
    Some(text)
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
    fn a_ternary_after_an_or_is_parenthesised_and_keeps_its_meaning() {
        use crate::parser::span_eq::parsed_files_span_eq;
        use crate::parser::{parse, parse_in};

        let old = "View Main() {\n    var a = true\n    var b = false\n    Text(a || b ? \"on\" : \"off\")\n    Text(\"{a || b ? 1 : 2}\")\n}\n";
        let moved = Edition::E2026
            .migrate(old)
            .expect("formats")
            .expect("changes");
        assert!(moved.contains("a || (b ? \"on\" : \"off\")"), "{moved}");
        assert!(moved.contains("{a || (b ? 1 : 2)}"), "{moved}");
        // What the old file meant in 2026 is what the new one means in 2027.
        let before = parse_in(old, Edition::E2026);
        let after = parse_in(&moved, Edition::E2027);
        assert!(after.errors.is_empty(), "{:?}", after.errors);
        assert!(parsed_files_span_eq(&before, &after));
        // And the same text means something else in 2027, which is why it moves.
        assert!(!parsed_files_span_eq(
            &before,
            &parse_in(old, Edition::E2027)
        ));
        // Moving again changes nothing; a file with no such ternary is left alone.
        assert_eq!(Edition::E2026.migrate(&moved).expect("formats"), None);
        assert_eq!(
            Edition::E2026.migrate("View Main() { Text(\"a\") }"),
            Ok(None)
        );
        let _ = parse(old);
    }

    #[test]
    fn a_file_that_needs_no_change_is_left_as_written() {
        let source = "View Main()   {  Text(\"a\")  }";
        assert_eq!(Edition::E2026.migrate(source), Ok(None));
    }
}
