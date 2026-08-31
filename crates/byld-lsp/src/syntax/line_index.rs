//! High-performance line index for converting between byte offsets and LSP positions.

use byard_compiler::diagnostics::Span;
use lsp_types::{Position, Range};

/// Fast line index structure built from source code text.
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offsets where each line starts.
    line_starts: Vec<usize>,
    /// Total byte length of the source text.
    len: usize,
}

impl LineIndex {
    /// Builds a new `LineIndex` for the given source text.
    #[must_use]
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        for (byte_idx, c) in source.char_indices() {
            if c == '\n' {
                line_starts.push(byte_idx + 1);
            }
        }
        Self {
            line_starts,
            len: source.len(),
        }
    }

    /// Converts a 0-indexed byte offset into an LSP `Position` (line, character).
    #[must_use]
    pub fn offset_to_position(&self, source: &str, offset: usize) -> Position {
        let offset = offset.min(self.len);
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };

        let line_start = self.line_starts[line_idx];
        let line_text = &source[line_start..offset.min(self.len)];
        let utf16_char_count = line_text.chars().map(|c| c.len_utf16() as u32).sum();

        Position::new(line_idx as u32, utf16_char_count)
    }

    /// Converts an LSP `Position` into a 0-indexed byte offset in the source text.
    #[must_use]
    pub fn position_to_offset(&self, source: &str, pos: Position) -> Option<usize> {
        let line_idx = pos.line as usize;
        if line_idx >= self.line_starts.len() {
            return Some(self.len);
        }

        let line_start = self.line_starts[line_idx];
        let line_end = if line_idx + 1 < self.line_starts.len() {
            self.line_starts[line_idx + 1].saturating_sub(1)
        } else {
            self.len
        };

        let line_text = &source[line_start..line_end];
        let mut utf16_accum = 0;
        for (byte_offset, c) in line_text.char_indices() {
            if utf16_accum >= pos.character {
                return Some(line_start + byte_offset);
            }
            utf16_accum += c.len_utf16() as u32;
        }

        Some(line_start + line_text.len())
    }

    /// Converts a compiler `Span` (start/end byte offset) into an LSP `Range`.
    #[must_use]
    pub fn span_to_range(&self, source: &str, span: Span) -> Range {
        Range::new(
            self.offset_to_position(source, span.start as usize),
            self.offset_to_position(source, span.end as usize),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_index_basic() {
        let source = "View Main {\n  var x = 10\n}";
        let index = LineIndex::new(source);

        let pos = index.offset_to_position(source, 14); // inside line 1 `var x = 10`
        assert_eq!(pos.line, 1);
        assert_eq!(pos.character, 2);

        let offset = index.position_to_offset(source, pos).unwrap();
        assert_eq!(offset, 14);
    }
}
