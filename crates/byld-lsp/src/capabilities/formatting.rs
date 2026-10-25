//! Code formatting capability for Byld DSL documents (RFC-0044).
//!
//! The layout is `byard_compiler::fmt`'s, the same one `byard fmt` writes, so
//! an editor's format-on-save and the command line never disagree.

use lsp_types::{FormattingOptions, Position, Range, TextEdit};

use crate::state::document::Document;

/// Handles textDocument/formatting request.
///
/// Returns one edit replacing the whole file, or none when it is already
/// formatted. A document that does not parse, or one the formatter cannot
/// prove it keeps the meaning of, gets no edits: the editor has the file as
/// it was, and its diagnostics say what is wrong.
///
/// The editor's tab size and `insert_spaces` are ignored: the style has no
/// options.
#[must_use]
pub fn handle_formatting(doc: &Document, _options: &FormattingOptions) -> Option<Vec<TextEdit>> {
    let Ok(formatted) = byard_compiler::fmt::format(&doc.content) else {
        return Some(Vec::new());
    };
    if formatted == doc.content {
        return Some(Vec::new());
    }

    let end_pos = doc
        .line_index
        .offset_to_position(&doc.content, doc.content.len());
    let full_range = Range::new(Position::new(0, 0), end_pos);

    Some(vec![TextEdit {
        range: full_range,
        new_text: formatted,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Uri;

    fn edits_for(content: &str) -> Vec<TextEdit> {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let doc = Document::new(uri, Some(1), content.to_string());
        handle_formatting(&doc, &FormattingOptions::default()).unwrap()
    }

    /// The edit is the whole file, and its text is what `fmt::format` returns.
    #[test]
    fn formats_through_the_compiler_formatter() {
        let input = "View Main(){\nColumn #[gap:8]{\nText(\"hello\")\n}\n}";
        let edits = edits_for(input);
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].new_text,
            byard_compiler::fmt::format(input).unwrap()
        );
        assert!(
            edits[0]
                .new_text
                .contains("    Column #[gap: 8] {\n        Text(\"hello\")\n    }")
        );
        assert_eq!(edits[0].range.start, Position::new(0, 0));
    }

    /// The old line re-indenter let an interpolation's brace shift every line
    /// after it.
    #[test]
    fn a_brace_in_a_string_does_not_move_the_indentation() {
        let input = "View Main() {\nvar count = 0\nText(\"{count} taps\")\nText(\"after\")\n}\n";
        let edits = edits_for(input);
        assert_eq!(
            edits[0].new_text,
            "View Main() {\n    var count = 0\n    Text(\"{count} taps\")\n    Text(\"after\")\n}\n"
        );
    }

    /// The editor's tab size is not an option the style has.
    #[test]
    fn the_editors_tab_size_is_ignored() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let doc = Document::new(uri, Some(1), "View Main() {\nText(\"x\")\n}\n".to_string());
        let tabs = FormattingOptions {
            tab_size: 2,
            insert_spaces: false,
            ..FormattingOptions::default()
        };
        let edits = handle_formatting(&doc, &tabs).unwrap();
        assert_eq!(edits[0].new_text, "View Main() {\n    Text(\"x\")\n}\n");
    }

    #[test]
    fn a_formatted_document_has_no_edits() {
        assert_eq!(edits_for("View Main() {\n    Text(\"x\")\n}\n").len(), 0);
    }

    /// A file that does not parse is left to its diagnostics.
    #[test]
    fn a_document_that_does_not_parse_has_no_edits() {
        assert_eq!(edits_for("View Main() {\nText(\n").len(), 0);
    }
}
