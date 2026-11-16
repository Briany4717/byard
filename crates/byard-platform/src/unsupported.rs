//! Why a program with a `Panel` cannot run, in the words `PanelsUnsupported`
//! says them (RFC-0068 §3): each names the platform or the missing protocol,
//! and what to do about it.
//!
//! Panels never fall back to windows: a bar that opens as an ordinary window is
//! broken in a confusing way, and a clear stop is better.

/// The name of the error, which every message starts with.
pub(crate) const NAME: &str = "PanelsUnsupported";

/// There is no Wayland session to connect to.
#[cfg_attr(
    not(all(target_os = "linux", feature = "wayland-shell")),
    allow(dead_code, reason = "only the Wayland backend connects")
)]
pub(crate) fn no_wayland(detail: &str) -> String {
    format!(
        "{NAME}: this program writes `Panel`, which is a Wayland layer surface, and there is no \
         Wayland session to connect to ({detail}); run it from a Wayland compositor that offers \
         zwlr_layer_shell_v1, such as Hyprland, Sway or KDE"
    )
}

/// The compositor does not offer layer shell.
#[cfg_attr(
    not(all(target_os = "linux", feature = "wayland-shell")),
    allow(dead_code, reason = "only the Wayland backend connects")
)]
pub(crate) fn no_layer_shell() -> String {
    format!(
        "{NAME}: this program writes `Panel`, and this compositor does not offer \
         zwlr_layer_shell_v1; panels run on Hyprland, Sway, river, niri, KDE and other \
         wlroots-style compositors, and not on GNOME's Mutter"
    )
}

/// The build has no shell backend: another platform, or the `wayland-shell`
/// feature turned off.
#[cfg_attr(
    all(target_os = "linux", feature = "wayland-shell"),
    allow(dead_code, reason = "only a build without the backend says it")
)]
pub(crate) fn not_built(platform: &str) -> String {
    format!(
        "{NAME}: this program writes `Panel`, which is a Wayland layer surface, and this build \
         has no Wayland shell backend ({platform}); panels need Linux with the `wayland-shell` \
         feature of byard-platform"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_starts_with_the_name_and_says_what_is_missing() {
        let none = no_wayland("WAYLAND_DISPLAY is not set");
        assert!(none.starts_with("PanelsUnsupported:"), "{none}");
        assert!(none.contains("WAYLAND_DISPLAY is not set"), "{none}");
        let shell = no_layer_shell();
        assert!(shell.starts_with("PanelsUnsupported:"), "{shell}");
        assert!(shell.contains("zwlr_layer_shell_v1"), "{shell}");
        let build = not_built("macos");
        assert!(build.starts_with("PanelsUnsupported:"), "{build}");
        assert!(
            build.contains("macos") && build.contains("wayland-shell"),
            "{build}"
        );
    }
}
