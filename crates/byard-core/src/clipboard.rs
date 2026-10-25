//! # Clipboard
//!
//! The one capability a text field needs from the machine besides pixels and
//! key events (RFC-0041 §3): somewhere to put copied text and somewhere to
//! read pasted text from.
//!
//! The engine holds a `Box<dyn Clipboard>` and never names an operating
//! system. A windowed host supplies one backed by the real clipboard
//! (`byard-platform`'s `SystemClipboard`); everything headless, `byard shot`
//! and every test among it, uses [`MemoryClipboard`], so a test can paste
//! without touching the clipboard of the machine it runs on.

/// Text in, text out. Both calls are infallible on purpose: a clipboard that
/// cannot be reached (no display server, a sandbox) reads as empty and writes
/// as nothing, and never fails the edit or the frame that asked.
pub trait Clipboard: Send {
    /// The clipboard's text, or `None` when it holds none or cannot be read.
    fn read_text(&mut self) -> Option<String>;

    /// Replaces the clipboard's contents with `text`. A failure is dropped.
    fn write_text(&mut self, text: &str);
}

/// A clipboard that is a [`String`] in memory.
///
/// What a headless run uses, and what an app falls back to where there is no
/// system clipboard to open. Copy then paste inside one process works exactly
/// as it does on a real one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MemoryClipboard {
    text: Option<String>,
}

impl MemoryClipboard {
    /// An empty clipboard.
    #[must_use]
    pub const fn new() -> Self {
        Self { text: None }
    }
}

impl Clipboard for MemoryClipboard {
    fn read_text(&mut self) -> Option<String> {
        self.text.clone()
    }

    fn write_text(&mut self, text: &str) {
        self.text = Some(text.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memory_clipboard_reads_what_was_last_written() {
        let mut clip = MemoryClipboard::new();
        assert_eq!(clip.read_text(), None, "a new clipboard holds nothing");
        clip.write_text("first");
        clip.write_text("second");
        assert_eq!(clip.read_text().as_deref(), Some("second"));
        assert_eq!(clip.read_text().as_deref(), Some("second"), "reading keeps");
    }
}
