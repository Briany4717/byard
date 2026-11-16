//! The connected screens, as a program reads them in `screens` (RFC-0068 §4).
//!
//! [`OutputFacts`] is what the compositor said about one `wl_output` (and its
//! `xdg_output`), copied out of the toolkit's types so the arithmetic is a
//! pure function with tests. [`screens`] is the list a program sees.

use byard_core::ScreenInfo;

/// What the compositor says about one output.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct OutputFacts {
    /// `wl_output.name` (version 4), else `xdg_output.name`.
    pub name: Option<String>,
    /// `wl_output.description`, else `xdg_output.description`.
    pub description: Option<String>,
    /// `wl_output.geometry`'s position, in the compositor's space.
    pub location: (i32, i32),
    /// `xdg_output.logical_position`.
    pub logical_position: Option<(i32, i32)>,
    /// `xdg_output.logical_size`.
    pub logical_size: Option<(i32, i32)>,
    /// The size of the current mode, in physical pixels.
    pub mode: Option<(i32, i32)>,
    /// `wl_output.scale`, the integer scale.
    pub scale_factor: i32,
    /// Whether the output is rotated a quarter turn, so the mode's axes are
    /// the other way round from the logical ones.
    pub rotated: bool,
}

/// The list of screens a program reads, ordered by `x` and then `y`.
///
/// - `name` is the connector, else `screen-N` (`N` counting from 1, in the
///   order the outputs were given, which is the order they were connected).
/// - The geometry is `xdg_output`'s when offered. Without it, it is the
///   `wl_output` position and the current mode divided by the integer scale.
/// - `scale` is the fractional scale when `fractional` says
///   `wp_fractional_scale_v1` is offered, else the integer one. A fractional
///   scale belongs to a surface, not an output, so it is read the way a
///   compositor derives it: the mode's width over the logical width, rounded
///   to the 1/120 the protocol counts in.
pub(crate) fn screens(outputs: &[OutputFacts], fractional: bool) -> Vec<ScreenInfo> {
    let mut list: Vec<ScreenInfo> = outputs
        .iter()
        .enumerate()
        .map(|(n, o)| screen(n, o, fractional))
        .collect();
    ScreenInfo::sort(&mut list);
    list
}

/// The name of the `n`th output (from 0, in connection order): the
/// connector when the compositor says it, else `screen-N` counting from 1.
pub(crate) fn name_of(n: usize, o: &OutputFacts) -> String {
    o.name
        .clone()
        .unwrap_or_else(|| format!("screen-{}", n + 1))
}

#[allow(
    clippy::cast_precision_loss,
    reason = "screen sizes are far inside f32's exact integers"
)]
fn screen(index: usize, facts: &OutputFacts, fractional: bool) -> ScreenInfo {
    let integer = facts.scale_factor.max(1);
    let (left, top) = facts.logical_position.unwrap_or(facts.location);
    let (width, height) = facts.logical_size.unwrap_or_else(|| {
        let (mode_w, mode_h) = facts.mode.unwrap_or((0, 0));
        let (across, down) = if facts.rotated {
            (mode_h, mode_w)
        } else {
            (mode_w, mode_h)
        };
        (across / integer, down / integer)
    });
    let scale = if fractional {
        derived_scale(facts).unwrap_or(integer as f32)
    } else {
        integer as f32
    };
    ScreenInfo {
        name: name_of(index, facts),
        description: facts.description.clone().unwrap_or_default(),
        x: left as f32,
        y: top as f32,
        width: width as f32,
        height: height as f32,
        scale,
    }
}

/// The scale a compositor with fractional scaling has set, read from the mode
/// and the logical size, to the nearest 1/120. `None` when either is missing
/// or the answer is not a scale (below 1, or not finite).
#[allow(
    clippy::cast_precision_loss,
    reason = "screen sizes are far inside f32's exact integers"
)]
fn derived_scale(o: &OutputFacts) -> Option<f32> {
    let mode = o.mode?;
    let (logical_w, logical_h) = o.logical_size?;
    // The mode's width runs along the logical width unless the output is
    // turned a quarter.
    let mode_w = if o.rotated { mode.1 } else { mode.0 };
    if logical_w <= 0 || logical_h <= 0 || mode_w <= 0 {
        return None;
    }
    let raw = mode_w as f32 / logical_w as f32;
    let scale = (raw * 120.0).round() / 120.0;
    (scale.is_finite() && scale >= 1.0).then_some(scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(name: &str, at: (i32, i32), logical: (i32, i32), mode: (i32, i32)) -> OutputFacts {
        OutputFacts {
            name: Some(name.to_string()),
            description: Some(format!("{name} monitor")),
            location: at,
            logical_position: Some(at),
            logical_size: Some(logical),
            mode: Some(mode),
            scale_factor: 1,
            rotated: false,
        }
    }

    #[test]
    fn screens_are_ordered_by_x_then_y_whatever_order_they_connected_in() {
        let list = screens(
            &[
                out("HDMI-A-1", (1920, 0), (1920, 1080), (1920, 1080)),
                out("DP-2", (0, 1080), (1920, 1080), (1920, 1080)),
                out("DP-1", (0, 0), (1920, 1080), (1920, 1080)),
            ],
            false,
        );
        let names: Vec<_> = list.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["DP-1", "DP-2", "HDMI-A-1"]);
    }

    #[test]
    fn an_output_with_no_name_is_screen_n_in_connection_order() {
        let mut a = out("x", (0, 0), (1280, 720), (1280, 720));
        a.name = None;
        let mut b = out("y", (1280, 0), (1280, 720), (1280, 720));
        b.name = None;
        let list = screens(&[a, b], false);
        assert_eq!(list[0].name, "screen-1");
        assert_eq!(list[1].name, "screen-2");
        // The number is the connection order, not the place on the desk.
        let mut left = out("x", (0, 0), (1280, 720), (1280, 720));
        left.name = None;
        let mut right = out("y", (1280, 0), (1280, 720), (1280, 720));
        right.name = None;
        let list = screens(&[right, left], false);
        assert_eq!(list[0].name, "screen-2");
        assert_eq!(list[1].name, "screen-1");
    }

    #[test]
    fn the_geometry_is_logical_and_falls_back_to_the_mode_over_the_integer_scale() {
        let mut o = out("DP-1", (10, 20), (1280, 720), (2560, 1440));
        o.scale_factor = 2;
        let s = &screens(&[o.clone()], false)[0];
        assert_eq!(
            (s.x, s.y, s.width, s.height, s.scale),
            (10.0, 20.0, 1280.0, 720.0, 2.0)
        );
        // No xdg_output: the same numbers from wl_output alone.
        o.logical_position = None;
        o.logical_size = None;
        let s = &screens(&[o], false)[0];
        assert_eq!(
            (s.x, s.y, s.width, s.height, s.scale),
            (10.0, 20.0, 1280.0, 720.0, 2.0)
        );
    }

    #[test]
    fn a_rotated_output_swaps_the_modes_axes() {
        let mut o = out("DP-3", (0, 0), (1080, 1920), (1920, 1080));
        o.logical_size = None;
        o.rotated = true;
        let s = &screens(&[o], false)[0];
        assert_eq!((s.width, s.height), (1080.0, 1920.0));
    }

    #[test]
    fn the_scale_is_fractional_only_where_the_protocol_is_offered() {
        // 2560x1440 shown as 1707x960: a scale of 1.5.
        let o = out("DP-1", (0, 0), (1707, 960), (2560, 1440));
        let with = &screens(std::slice::from_ref(&o), true)[0];
        assert!((with.scale - 1.5).abs() < 0.01, "{}", with.scale);
        let without = &screens(&[o], false)[0];
        assert!((without.scale - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_fractional_scale_that_cannot_be_read_is_the_integer_one() {
        let mut o = out("DP-1", (0, 0), (1707, 960), (2560, 1440));
        o.scale_factor = 2;
        o.mode = None;
        assert!((screens(&[o.clone()], true)[0].scale - 2.0).abs() < f32::EPSILON);
        // A logical size larger than the mode is not a scale.
        o.mode = Some((800, 600));
        assert!((screens(&[o], true)[0].scale - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_rescaled_screen_changes_only_its_scale() {
        let a = &screens(&[out("DP-1", (0, 0), (2560, 1440), (2560, 1440))], true)[0];
        let b = &screens(&[out("DP-1", (0, 0), (1707, 960), (2560, 1440))], true)[0];
        assert_eq!(a.name, b.name);
        assert_ne!(a, b);
    }
}
