//! Code formatting capability for Byld DSL documents.

use lsp_types::{FormattingOptions, Position, Range, TextEdit};

use crate::state::document::Document;

/// Handles textDocument/formatting request.
#[must_use]
pub fn handle_formatting(doc: &Document, options: FormattingOptions) -> Option<Vec<TextEdit>> {
    let tab = if options.insert_spaces {
        " ".repeat(options.tab_size as usize)
    } else {
        "\t".to_string()
    };

    let formatted = format_byd_source(&doc.content, &tab);
    if formatted == doc.content {
        return Some(Vec::new());
    }

    let end_pos = doc.line_index.offset_to_position(&doc.content, doc.content.len());
    let full_range = Range::new(Position::new(0, 0), end_pos);

    Some(vec![TextEdit {
        range: full_range,
        new_text: formatted,
    }])
}

/// Formats `.byd` DSL source code with proper indentation and block alignment.
fn format_byd_source(source: &str, indent_str: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut indent_level: usize = 0;

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            result.push('\n');
            continue;
        }

        let starts_with_closing = trimmed.starts_with('}') || trimmed.starts_with(']');
        if starts_with_closing {
            indent_level = indent_level.saturating_sub(1);
        }

        for _ in 0..indent_level {
            result.push_str(indent_str);
        }
        result.push_str(trimmed);
        result.push('\n');

        let opens = trimmed.chars().filter(|&c| c == '{' || c == '[').count();
        let closes = trimmed.chars().filter(|&c| c == '}' || c == ']').count();

        if !starts_with_closing {
            if opens > closes {
                indent_level += opens - closes;
            } else if closes > opens {
                indent_level = indent_level.saturating_sub(closes - opens);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_byd() {
        let input = "View Main{\nvar x = 10\nColumn{\nText(\"hello\")\n}\n}";
        let formatted = format_byd_source(input, "  ");
        let expected = "View Main{\n  var x = 10\n  Column{\n    Text(\"hello\")\n  }\n}\n";
        assert_eq!(formatted, expected);
    }
}
