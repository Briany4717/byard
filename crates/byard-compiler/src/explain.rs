//! The error index: what each diagnostic means, by the name in its brackets
//! (`error[UnknownAttribute]`).
//!
//! The text is `errors.md` at the root of this crate, one `## Name` section an
//! error. The guide includes the same file as its *Error index* chapter, and
//! the guide's test checks every example in it, so `byard explain`, the book
//! and the compiler cannot say different things.

/// The whole index, as written.
pub const INDEX: &str = include_str!("../errors.md");

/// The section of the index for the error `name`, from its `## name` heading
/// to the next one, without the heading. `None` for a name no error has.
#[must_use]
pub fn explain(name: &str) -> Option<&'static str> {
    sections().find(|(n, _)| *n == name).map(|(_, text)| text)
}

/// Every section of the index, in order: the error's name and its text.
///
/// Read whatever ends its lines: a checkout on Windows ends them with `\r\n`.
pub fn sections() -> impl Iterator<Item = (&'static str, &'static str)> {
    sections_of(INDEX)
}

/// [`sections`] of any text written as the index is.
fn sections_of(index: &str) -> impl Iterator<Item = (&str, &str)> {
    index.split("\n## ").skip(1).map(|section| {
        let (name, text) = section.split_once('\n').unwrap_or((section, ""));
        (name.trim(), text.trim())
    })
}

/// The name of an error in the index closest to `name`, for a "did you
/// mean" when it names none: a different case or a letter off.
#[must_use]
pub fn closest(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    sections()
        .map(|(n, _)| n)
        .find(|n| n.to_ascii_lowercase() == lower)
        .or_else(|| crate::util::closest_match(name, sections().map(|(n, _)| n)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Every name `CompileError::kind` can give, read from the match that
    /// gives them, so an error added to it without a section is caught here.
    fn kinds() -> BTreeSet<String> {
        kinds_of(include_str!("diagnostics.rs"))
    }

    /// [`kinds`] of any text: line by line, so a checkout with `\r\n` line
    /// ends reads the same.
    fn kinds_of(src: &str) -> BTreeSet<String> {
        src.lines()
            .skip_while(|l| !l.contains("pub fn kind(&self)"))
            .take_while(|l| l.trim_end() != "    }")
            .filter_map(|l| l.split("=> \"").nth(1))
            .filter_map(|rest| rest.split('"').next())
            .map(str::to_string)
            .collect()
    }

    /// The names the list of errors without an example names.
    fn without_example() -> BTreeSet<String> {
        include_str!("../errors-without-example.txt")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn every_error_has_a_section_and_every_section_is_an_error() {
        let kinds = kinds();
        assert!(kinds.len() > 100, "the kinds were not read: {kinds:?}");
        let written: BTreeSet<String> = sections().map(|(n, _)| n.to_string()).collect();
        let missing: Vec<_> = kinds.difference(&written).collect();
        let extra: Vec<_> = written.difference(&kinds).collect();
        assert!(
            missing.is_empty(),
            "errors with no `## Name` section in errors.md: {missing:?}"
        );
        assert!(
            extra.is_empty(),
            "sections of errors.md that are no error (renamed or removed?): {extra:?}"
        );
    }

    #[test]
    fn the_sections_are_in_alphabetical_order_and_each_is_written_once() {
        let names: Vec<&str> = sections().map(|(n, _)| n).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted, "errors.md must list each error once, A to Z");
    }

    #[test]
    fn an_example_shows_the_error_of_its_own_section() {
        for (name, text) in sections() {
            let shown = text.contains("```byld,fails");
            if !shown {
                continue;
            }
            let quoted = text
                .lines()
                .skip_while(|l| l.trim_end() != "```diagnostic")
                .nth(1)
                .unwrap_or_else(|| panic!("{name}: an example with no `diagnostic` line after it"));
            assert!(
                quoted.contains(&format!("[{name}]:")),
                "{name}: the example quotes `{quoted}`, which is another error"
            );
        }
    }

    #[test]
    fn the_list_of_errors_without_an_example_only_shrinks() {
        let listed = without_example();
        for (name, text) in sections() {
            let shown = text.contains("```byld,fails");
            let on_list = listed.contains(name);
            assert!(
                !(shown && on_list),
                "{name} has an example now: take it off errors-without-example.txt"
            );
            assert!(
                shown || on_list,
                "{name} has no example: give its section a `byld,fails` example (the list only shrinks)"
            );
        }
        for name in &listed {
            assert!(explain(name).is_some(), "{name} on the list is no error");
        }
    }

    #[test]
    fn a_name_finds_its_section_and_a_near_one_is_suggested() {
        let text = explain("UnknownAttribute").expect("a section");
        assert!(text.contains("attribute"), "{text}");
        assert!(explain("unknownattribute").is_none());
        assert_eq!(closest("unknownattribute"), Some("UnknownAttribute"));
        assert_eq!(closest("UnknownAtribute"), Some("UnknownAttribute"));
    }

    /// What a checkout on Windows reads: the same index and the same errors,
    /// with `\r\n` line ends.
    #[test]
    fn the_index_reads_the_same_with_windows_line_ends() {
        let crlf = |text: &str| text.replace("\r\n", "\n").replace('\n', "\r\n");
        let index = crlf(INDEX);
        let names = |text: &str| {
            sections_of(text)
                .map(|(n, _)| n.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(INDEX), names(&index));
        let errors = kinds_of(&crlf(include_str!("diagnostics.rs")));
        assert_eq!(kinds(), errors);
        assert!(errors.len() > 100);
    }
}
