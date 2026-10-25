//! The system clipboard (RFC-0041 §3).

use byard_core::Clipboard;

/// The operating system's clipboard, behind [`byard_core::Clipboard`].
///
/// **Created on first use.** Opening the clipboard connects to the display
/// server (the Wayland `data-control` protocol, X11 or the platform's own),
/// which an app that never copies or pastes has no reason to do, so
/// [`new`](Self::new) opens nothing and the connection is made by the first
/// copy, cut or paste and kept after that. Nothing in this type costs anything
/// for an app with no text field in use.
///
/// A clipboard that cannot be opened (no display server, a sandbox that hides
/// it) reads as empty and writes as nothing, and is tried again at the next
/// use, so one that appears later still works. It never fails an edit.
#[derive(Default)]
pub struct SystemClipboard {
    inner: Option<arboard::Clipboard>,
}

impl SystemClipboard {
    /// A handle that has not opened the clipboard yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { inner: None }
    }

    /// The open clipboard, opening it now if this is the first use.
    fn open(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.inner.is_none() {
            self.inner = arboard::Clipboard::new().ok();
        }
        self.inner.as_mut()
    }

    /// Whether the clipboard has been opened yet. Test-facing: the witness
    /// that merely constructing one, or never pasting, opens nothing.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.inner.is_some()
    }
}

impl Clipboard for SystemClipboard {
    fn read_text(&mut self) -> Option<String> {
        self.open()?.get_text().ok()
    }

    fn write_text(&mut self, text: &str) {
        if let Some(clipboard) = self.open() {
            // A failed write is dropped, as the trait promises: the field has
            // already done everything it can for the user.
            let _ = clipboard.set_text(text.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Building the handle touches no display server. This is the lazy half
    /// of RFC-0041 §3, and it is the half that costs an app nothing: the
    /// test would fail on a machine with no display if `new` opened one.
    #[test]
    fn constructing_the_clipboard_opens_nothing() {
        let clipboard = SystemClipboard::new();
        assert!(!clipboard.is_open());
        let clipboard = SystemClipboard::default();
        assert!(!clipboard.is_open());
    }
}
