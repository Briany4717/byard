//! What stands in for the Wayland shell backend where there is none: another
//! platform, or a build with `wayland-shell` turned off (RFC-0068 §3).
//!
//! A program with a `Panel` stops at startup, with `PanelsUnsupported` and the
//! name of what is missing. Panels never fall back to windows: a bar that
//! opens as an ordinary window is broken in a confusing way.

use byard_core::{ByardError, PlatformHost};

/// The same type as the real shell host, which cannot run here.
pub struct ShellHost {
    _private: (),
}

impl ShellHost {
    /// A host that cannot be run: see [`run`](Self::run).
    pub fn new(_title: impl Into<String>, _app_id: impl Into<String>) -> Self {
        Self { _private: () }
    }

    /// Always fails: this build has no Wayland shell backend.
    ///
    /// # Errors
    ///
    /// [`ByardError::Platform`] saying `PanelsUnsupported` and what is
    /// missing.
    pub fn run(self, _host: impl PlatformHost + 'static) -> Result<(), ByardError> {
        Err(ByardError::Platform(message()))
    }
}

/// The message, naming the platform.
fn message() -> String {
    crate::unsupported::not_built(std::env::consts::OS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_names_the_error_the_platform_and_the_feature() {
        let m = message();
        assert!(m.starts_with("PanelsUnsupported:"), "{m}");
        assert!(m.contains(std::env::consts::OS), "{m}");
        assert!(m.contains("wayland-shell"), "{m}");
    }
}
