//! What a `Panel`'s spec says to the compositor, as the numbers the protocol
//! takes (RFC-0068 §3).
//!
//! Every function here is pure: a [`LayerSpec`] in, the values of
//! `zwlr_layer_surface_v1` out, so the translation is tested without a
//! compositor. The backend only sends what they return.

use byard_core::surface::placement::{Anchors, Layer};
use byard_core::surface::{KeyboardMode, LayerSpec};
use smithay_client_toolkit::shell::wlr_layer::{self, KeyboardInteractivity};

/// The name of the global a compositor offers layer shell as, which is what
/// its presence is looked for by.
pub(crate) const INTERFACE: &str = "zwlr_layer_shell_v1";

/// The layer-shell version that has `on_demand` keyboard interactivity.
pub(crate) const ON_DEMAND_SINCE: u32 = 4;

/// The highest version of `zwlr_layer_shell_v1` Byard speaks.
pub(crate) const MAX_VERSION: u32 = 4;

/// The version Byard binds when the compositor offers `advertised`: the lower
/// of the two, since a client may not ask for more than the compositor has and
/// has nothing to say in a later version.
pub(crate) fn bound_version(advertised: u32) -> u32 {
    advertised.min(MAX_VERSION)
}

/// The anchor bitset (`top` 1, `bottom` 2, `left` 4, `right` 8).
pub(crate) fn anchor(anchors: Anchors) -> wlr_layer::Anchor {
    let mut out = wlr_layer::Anchor::empty();
    for (ours, theirs) in [
        (Anchors::TOP, wlr_layer::Anchor::TOP),
        (Anchors::BOTTOM, wlr_layer::Anchor::BOTTOM),
        (Anchors::LEFT, wlr_layer::Anchor::LEFT),
        (Anchors::RIGHT, wlr_layer::Anchor::RIGHT),
    ] {
        if anchors.contains(ours) {
            out |= theirs;
        }
    }
    out
}

/// The layer the surface lives in.
pub(crate) fn layer(layer: Layer) -> wlr_layer::Layer {
    match layer {
        Layer::Background => wlr_layer::Layer::Background,
        Layer::Bottom => wlr_layer::Layer::Bottom,
        Layer::Top => wlr_layer::Layer::Top,
        Layer::Overlay => wlr_layer::Layer::Overlay,
    }
}

/// The keyboard interactivity to send at layer-shell `version`.
///
/// `on_demand` is a version-4 value: a version-3 compositor would take it as
/// a protocol error. There, a panel that wants the keys when clicked gets
/// `exclusive` while it has a focused text field (`focused_field`) and `none`
/// otherwise, which looks the same to the person typing (RFC-0068 §3).
pub(crate) fn keyboard(
    mode: KeyboardMode,
    version: u32,
    focused_field: bool,
) -> KeyboardInteractivity {
    match mode {
        KeyboardMode::Exclusive => KeyboardInteractivity::Exclusive,
        KeyboardMode::OnDemand if version >= ON_DEMAND_SINCE => KeyboardInteractivity::OnDemand,
        KeyboardMode::OnDemand if focused_field => KeyboardInteractivity::Exclusive,
        KeyboardMode::None | KeyboardMode::OnDemand => KeyboardInteractivity::None,
    }
}

/// `set_margin`'s four values, in the protocol's order (top, right, bottom,
/// left).
pub(crate) fn margins(spec: &LayerSpec) -> (i32, i32, i32, i32) {
    let m = spec.margin;
    (m.top, m.right, m.bottom, m.left)
}

/// The size to send with `set_size`: whole logical pixels, never negative. A
/// zero on an axis tells the compositor to stretch it, which is right only on
/// an axis anchored at both ends; the interpreter sends zero nowhere else.
pub(crate) fn requested_size(size: (f32, f32)) -> (u32, u32) {
    (whole(size.0), whole(size.1))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to the range of the target first"
)]
fn whole(v: f32) -> u32 {
    if v.is_nan() {
        return 0;
    }
    v.ceil().clamp(0.0, 1.0e6) as u32
}

/// The logical size a surface is drawn at, from the size the compositor
/// configured it with and the size it asked for: a zero from the compositor
/// means "your choice", which is what was asked.
pub(crate) fn configured_size(configure: (u32, u32), asked: (u32, u32)) -> (u32, u32) {
    (
        if configure.0 == 0 {
            asked.0
        } else {
            configure.0
        },
        if configure.1 == 0 {
            asked.1
        } else {
            configure.1
        },
    )
}

/// The logical size of a window after a configure: the compositor's choice on
/// an axis it makes one for, else the size it has now, else the one the spec
/// opens it at.
pub(crate) fn window_size(
    configured: (Option<u32>, Option<u32>),
    current: (u32, u32),
    opening: (f32, f32),
) -> (u32, u32) {
    let (ow, oh) = requested_size(opening);
    let pick = |c: Option<u32>, now: u32, open: u32| {
        c.filter(|v| *v > 0)
            .unwrap_or(if now > 0 { now } else { open })
    };
    (
        pick(configured.0, current.0, ow),
        pick(configured.1, current.1, oh),
    )
}

/// The size of the buffer for a surface of `logical` pixels at `scale`, in
/// physical pixels: rounded to the nearest pixel, as the fractional-scale
/// protocol says a client sizes its buffer (`round(logical * scale)`), and never
/// zero, which a swapchain cannot have.
pub(crate) fn physical_size(logical: (u32, u32), scale: f64) -> (u32, u32) {
    let px = |l: u32| -> u32 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a screen's size times a scale is far inside u32"
        )]
        let v = (f64::from(l) * scale).round() as u32;
        v.max(1)
    };
    (px(logical.0), px(logical.1))
}

/// The scale a `wp_fractional_scale_v1` `preferred_scale` carries, which is
/// the scale times 120.
pub(crate) fn fractional_scale(preferred: u32) -> f64 {
    f64::from(preferred) / 120.0
}

/// Everything a layer surface is told, as the protocol takes it. What was
/// sent last is kept beside the surface, so a change to the spec sends the
/// requests that differ and no others, and a commit is only made when one did.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Wire {
    /// `set_layer` (at creation, then reactive).
    pub layer: wlr_layer::Layer,
    /// `set_anchor`.
    pub anchor: wlr_layer::Anchor,
    /// `set_size`, in logical pixels.
    pub size: (u32, u32),
    /// `set_exclusive_zone`.
    pub zone: i32,
    /// `set_margin`: top, right, bottom, left.
    pub margin: (i32, i32, i32, i32),
    /// `set_keyboard_interactivity`.
    pub keyboard: KeyboardInteractivity,
}

/// Which requests differ between two [`Wire`]s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "one switch per request")]
pub(crate) struct Changes {
    pub layer: bool,
    pub anchor: bool,
    pub size: bool,
    pub zone: bool,
    pub margin: bool,
    pub keyboard: bool,
}

impl Changes {
    /// Whether anything needs sending, and so a commit.
    pub(crate) fn any(self) -> bool {
        self.layer || self.anchor || self.size || self.zone || self.margin || self.keyboard
    }
}

impl Wire {
    /// What `spec` says to a compositor whose layer shell is at `version`, with
    /// `focused_field` whether the panel has a focused text field (the version-3
    /// stand-in for `on_demand`). `size` is the size the panel asks for.
    pub(crate) fn of(
        spec: &LayerSpec,
        size: (f32, f32),
        version: u32,
        focused_field: bool,
    ) -> Self {
        let mut size = requested_size(size);
        // A size of zero means "stretch", which the protocol allows only along
        // an axis anchored at both ends and otherwise answers with a protocol
        // error that ends the connection. The interpreter never sends one
        // there (it measures the content first); a one-pixel surface is the
        // safe answer if one ever arrives.
        let across = Anchors::LEFT | Anchors::RIGHT;
        let down = Anchors::TOP | Anchors::BOTTOM;
        if size.0 == 0 && !spec.anchors.contains(across) {
            size.0 = 1;
        }
        if size.1 == 0 && !spec.anchors.contains(down) {
            size.1 = 1;
        }
        let size_for_zone = byard_core::surface::Size::new(
            i32::try_from(size.0).unwrap_or(i32::MAX),
            i32::try_from(size.1).unwrap_or(i32::MAX),
        );
        Self {
            layer: layer(spec.layer),
            anchor: anchor(spec.anchors),
            size,
            zone: spec.zone(size_for_zone),
            margin: margins(spec),
            keyboard: keyboard(spec.keyboard, version, focused_field),
        }
    }

    /// The requests to send to go from `previous` (what the compositor has,
    /// `None` for a surface that has had nothing sent yet) to this.
    pub(crate) fn changes_from(&self, previous: Option<&Self>) -> Changes {
        let Some(p) = previous else {
            // A new layer surface has been sent nothing, but the protocol's
            // defaults are the values of an empty `LayerSpec`: anchor none, size
            // 0, zone 0, margin 0, no keyboard. Only a difference from those
            // needs a request; the layer is sent with creation.
            return Self {
                layer: self.layer,
                anchor: wlr_layer::Anchor::empty(),
                size: (0, 0),
                zone: 0,
                margin: (0, 0, 0, 0),
                keyboard: KeyboardInteractivity::None,
            }
            .changes_from(Some(self));
        };
        Changes {
            layer: self.layer != p.layer,
            anchor: self.anchor != p.anchor,
            size: self.size != p.size,
            zone: self.zone != p.zone,
            margin: self.margin != p.margin,
            keyboard: self.keyboard != p.keyboard,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byard_core::surface::Size;
    use byard_core::surface::placement::Margin;
    use byard_core::surface::{Exclusive, KeyboardMode};

    #[test]
    fn the_global_looked_for_is_the_one_the_protocol_names() {
        use smithay_client_toolkit::reexports::client::Proxy;
        use smithay_client_toolkit::reexports::protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;
        assert_eq!(ZwlrLayerShellV1::interface().name, INTERFACE);
    }

    #[test]
    fn anchors_are_the_protocols_bitset() {
        assert_eq!(anchor(Anchors::NONE).bits(), 0);
        assert_eq!(anchor(Anchors::TOP).bits(), 1);
        assert_eq!(anchor(Anchors::BOTTOM).bits(), 2);
        assert_eq!(anchor(Anchors::LEFT).bits(), 4);
        assert_eq!(anchor(Anchors::RIGHT).bits(), 8);
        // `edge: top` is the top and both perpendicular edges.
        assert_eq!(
            anchor(Anchors::TOP | Anchors::LEFT | Anchors::RIGHT).bits(),
            1 | 4 | 8
        );
        assert_eq!(anchor(Anchors::BOTTOM | Anchors::RIGHT).bits(), 2 | 8);
    }

    #[test]
    fn layers_map_one_to_one() {
        assert_eq!(layer(Layer::Background), wlr_layer::Layer::Background);
        assert_eq!(layer(Layer::Bottom), wlr_layer::Layer::Bottom);
        assert_eq!(layer(Layer::Top), wlr_layer::Layer::Top);
        assert_eq!(layer(Layer::Overlay), wlr_layer::Layer::Overlay);
    }

    fn bar(exclusive: Exclusive, anchors: Anchors) -> LayerSpec {
        LayerSpec {
            anchors,
            exclusive,
            ..LayerSpec::new("bar")
        }
    }

    #[test]
    fn the_exclusive_zone_is_the_thickness_zero_or_minus_one() {
        let top = Anchors::TOP | Anchors::LEFT | Anchors::RIGHT;
        let left = Anchors::LEFT | Anchors::TOP | Anchors::BOTTOM;
        // `true` on an `edge` reserves the thickness along it: the height for
        // a top or bottom bar and the width for a left or right one.
        assert_eq!(bar(Exclusive::Thickness, top).zone(Size::new(0, 40)), 40);
        assert_eq!(bar(Exclusive::Thickness, left).zone(Size::new(64, 0)), 64);
        // `false` sends 0 so the panel respects what others reserved, and
        // `ignore` sends -1 so it covers it too.
        assert_eq!(bar(Exclusive::Off, top).zone(Size::new(0, 40)), 0);
        assert_eq!(bar(Exclusive::Ignore, top).zone(Size::new(0, 40)), -1);
        // A number is that many pixels, and never negative: -1 is `ignore`.
        assert_eq!(bar(Exclusive::Pixels(12), top).zone(Size::new(0, 40)), 12);
        assert_eq!(bar(Exclusive::Pixels(-5), top).zone(Size::new(0, 40)), 0);
        // A panel in no single edge reserves nothing: the protocol would
        // ignore a zone there.
        assert_eq!(
            bar(Exclusive::Thickness, Anchors::NONE).zone(Size::new(100, 40)),
            0
        );
    }

    #[test]
    fn on_demand_keyboard_needs_version_four() {
        use KeyboardInteractivity as K;
        assert_eq!(keyboard(KeyboardMode::None, 4, true), K::None);
        assert_eq!(keyboard(KeyboardMode::Exclusive, 3, false), K::Exclusive);
        assert_eq!(keyboard(KeyboardMode::OnDemand, 4, false), K::OnDemand);
        assert_eq!(keyboard(KeyboardMode::OnDemand, 4, true), K::OnDemand);
        // Version 3: exclusive while a field is focused, none otherwise.
        assert_eq!(keyboard(KeyboardMode::OnDemand, 3, true), K::Exclusive);
        assert_eq!(keyboard(KeyboardMode::OnDemand, 3, false), K::None);
        assert_eq!(keyboard(KeyboardMode::OnDemand, 1, false), K::None);
    }

    #[test]
    fn the_bound_version_is_the_lower_of_the_compositors_and_ours() {
        assert_eq!(bound_version(1), 1);
        assert_eq!(bound_version(3), 3);
        assert_eq!(bound_version(4), 4);
        assert_eq!(bound_version(5), 4);
    }

    #[test]
    fn margins_go_in_the_protocols_order() {
        let spec = LayerSpec {
            margin: Margin {
                top: 1,
                right: 2,
                bottom: 3,
                left: 4,
            },
            ..LayerSpec::new("m")
        };
        assert_eq!(margins(&spec), (1, 2, 3, 4));
        // A negative margin is allowed by the protocol and kept.
        let pulled = LayerSpec {
            margin: Margin {
                top: -8,
                ..Margin::default()
            },
            ..LayerSpec::new("m")
        };
        assert_eq!(margins(&pulled), (-8, 0, 0, 0));
    }

    #[test]
    fn a_size_is_whole_pixels_rounded_up_and_never_negative() {
        assert_eq!(requested_size((0.0, 40.0)), (0, 40));
        assert_eq!(requested_size((120.2, 39.5)), (121, 40));
        assert_eq!(requested_size((-3.0, f32::NAN)), (0, 0));
    }

    #[test]
    fn a_zero_from_the_compositor_means_the_size_that_was_asked() {
        assert_eq!(configured_size((1920, 40), (0, 40)), (1920, 40));
        assert_eq!(configured_size((0, 0), (300, 80)), (300, 80));
        assert_eq!(configured_size((1000, 0), (300, 80)), (1000, 80));
    }

    #[test]
    fn a_bar_says_what_the_spec_says_in_the_protocols_numbers() {
        let spec = LayerSpec {
            layer: Layer::Overlay,
            anchors: Anchors::TOP | Anchors::LEFT | Anchors::RIGHT,
            exclusive: Exclusive::Thickness,
            keyboard: KeyboardMode::OnDemand,
            margin: Margin {
                top: 4,
                right: 8,
                bottom: 0,
                left: 8,
            },
            ..LayerSpec::new("bar")
        };
        let wire = Wire::of(&spec, (0.0, 40.0), 4, false);
        assert_eq!(wire.layer, wlr_layer::Layer::Overlay);
        assert_eq!(wire.anchor.bits(), 1 | 4 | 8);
        assert_eq!(wire.size, (0, 40), "0 on the axis the compositor stretches");
        assert_eq!(wire.zone, 40);
        assert_eq!(wire.margin, (4, 8, 0, 8));
        assert_eq!(wire.keyboard, KeyboardInteractivity::OnDemand);
        // The same panel on a version-3 compositor, with nothing focused.
        let old = Wire::of(&spec, (0.0, 40.0), 3, false);
        assert_eq!(old.keyboard, KeyboardInteractivity::None);
    }

    #[test]
    fn a_zero_size_is_kept_only_along_an_axis_anchored_at_both_ends() {
        let bar = LayerSpec {
            anchors: Anchors::TOP | Anchors::LEFT | Anchors::RIGHT,
            ..LayerSpec::new("bar")
        };
        // Stretched across, 40 high: the protocol's 0 stays.
        assert_eq!(Wire::of(&bar, (0.0, 40.0), 4, false).size, (0, 40));
        // Zero high on an axis that is not stretched would be a protocol
        // error: one pixel instead.
        assert_eq!(Wire::of(&bar, (0.0, 0.0), 4, false).size, (0, 1));
        let corner = LayerSpec {
            anchors: Anchors::TOP | Anchors::RIGHT,
            ..LayerSpec::new("clock")
        };
        assert_eq!(Wire::of(&corner, (0.0, 0.0), 4, false).size, (1, 1));
    }

    #[test]
    fn only_the_requests_that_differ_are_sent() {
        let spec = LayerSpec {
            anchors: Anchors::TOP | Anchors::LEFT | Anchors::RIGHT,
            exclusive: Exclusive::Thickness,
            ..LayerSpec::new("bar")
        };
        let before = Wire::of(&spec, (0.0, 40.0), 4, false);
        // Nothing changed: nothing to send, so nothing to commit.
        assert!(!before.changes_from(Some(&before)).any());
        // A taller bar sends a size and a new zone, and nothing else.
        let taller = Wire::of(&spec, (0.0, 56.0), 4, false);
        assert_eq!(
            taller.changes_from(Some(&before)),
            Changes {
                size: true,
                zone: true,
                ..Changes::default()
            }
        );
        // A bar that stops reserving sends the zone alone.
        let floating = Wire::of(
            &LayerSpec {
                exclusive: Exclusive::Off,
                ..spec.clone()
            },
            (0.0, 40.0),
            4,
            false,
        );
        assert_eq!(
            floating.changes_from(Some(&before)),
            Changes {
                zone: true,
                ..Changes::default()
            }
        );
        // The keyboard and the layer are their own requests.
        let moved = Wire::of(
            &LayerSpec {
                layer: Layer::Bottom,
                keyboard: KeyboardMode::Exclusive,
                ..spec
            },
            (0.0, 40.0),
            4,
            false,
        );
        assert_eq!(
            moved.changes_from(Some(&before)),
            Changes {
                layer: true,
                keyboard: true,
                ..Changes::default()
            }
        );
    }

    #[test]
    fn a_new_surface_sends_what_differs_from_the_protocols_defaults() {
        let spec = LayerSpec {
            anchors: Anchors::BOTTOM,
            ..LayerSpec::new("dock")
        };
        let wire = Wire::of(&spec, (300.0, 64.0), 4, false);
        let first = wire.changes_from(None);
        assert!(first.anchor && first.size);
        assert!(
            !first.zone && !first.margin && !first.keyboard,
            "defaults are not sent"
        );
    }

    #[test]
    fn a_window_keeps_its_size_when_the_compositor_has_no_opinion() {
        // First configure: the compositor says nothing, so the spec's size.
        assert_eq!(
            window_size((None, None), (0, 0), (360.0, 240.0)),
            (360, 240)
        );
        // The user resized it, or a tiling compositor chose a width.
        assert_eq!(
            window_size((Some(500), Some(300)), (360, 240), (360.0, 240.0)),
            (500, 300)
        );
        // One axis chosen: the other stays.
        assert_eq!(
            window_size((Some(500), None), (360, 240), (360.0, 240.0)),
            (500, 240)
        );
        assert_eq!(
            window_size((Some(0), None), (360, 240), (100.0, 100.0)),
            (360, 240)
        );
    }

    #[test]
    fn the_buffer_is_the_logical_size_times_the_scale_rounded() {
        assert_eq!(physical_size((1920, 40), 1.0), (1920, 40));
        assert_eq!(physical_size((1920, 40), 2.0), (3840, 80));
        // 1.5: 1707 logical pixels is 2560.5, which rounds up as the
        // fractional-scale protocol says.
        assert_eq!(physical_size((1707, 33), 1.5), (2561, 50));
        assert_eq!(
            physical_size((0, 0), 1.0),
            (1, 1),
            "a swapchain is never empty"
        );
    }

    #[test]
    fn a_preferred_fractional_scale_is_in_hundred_and_twentieths() {
        assert!((fractional_scale(120) - 1.0).abs() < f64::EPSILON);
        assert!((fractional_scale(180) - 1.5).abs() < f64::EPSILON);
        assert!((fractional_scale(150) - 1.25).abs() < f64::EPSILON);
    }

    /// The spec a panel of `source` is created with, as the interpreter makes it
    /// once the content is measured.
    fn created_spec(source: &str) -> byard_core::SurfaceSpec {
        use byard_compiler::interp::eval::Interpreter;
        use byard_core::frame::RenderFrame;
        let parsed = byard_compiler::parser::parse(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.enable_surfaces();
        interp.set_screens(vec![byard_core::ScreenInfo::new("screen-1", 1280.0, 800.0)]);
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let main = parsed.views.iter().find(|v| v.name.as_str() == "Main");
        let tree = interp.lower_view(main.unwrap(), &known);
        let mut spec = None;
        for _ in 0..4 {
            interp.set_now_ms(16);
            interp.tick();
            let mut out = Vec::new();
            interp.render_surfaces(
                &tree,
                &mut || {
                    let mut f = RenderFrame::new();
                    f.request_full_redraw();
                    f
                },
                &mut out,
            );
            for change in interp.take_surface_changes() {
                if let byard_core::SurfaceChange::Create { id, spec: s } = change {
                    interp.set_surface_size(id, (1280.0, s.size.1));
                    spec = Some(s);
                }
            }
        }
        spec.expect("the panel was created")
    }

    /// The exclusive zone sent is the bar's thickness, for a bar whose height
    /// is its content's and for one with an explicit `height`, and it follows
    /// the thickness when the content grows.
    #[test]
    fn a_bars_zone_is_its_measured_or_written_height() {
        let content =
            created_spec("View Main() { Panel #[edge: top] { Text(\"Hello\") #[p: 8] } }");
        let layer_spec = content.layer.as_ref().unwrap();
        let wire = Wire::of(layer_spec, content.size, 4, false);
        assert!(wire.size.1 > 0, "measured height {:?}", content.size);
        assert_eq!(
            wire.zone,
            i32::try_from(wire.size.1).unwrap(),
            "{content:?}"
        );
        let written =
            created_spec("View Main() { Panel #[edge: top, height: 40] { Text(\"Hello\") } }");
        let wire = Wire::of(written.layer.as_ref().unwrap(), written.size, 4, false);
        assert_eq!((wire.size, wire.zone), ((0, 40), 40));
        // A bottom bar and a side bar reserve their own thickness.
        let left = created_spec("View Main() { Panel #[edge: left, width: 64] { Text(\"x\") } }");
        let wire = Wire::of(left.layer.as_ref().unwrap(), left.size, 4, false);
        assert_eq!((wire.size, wire.zone), ((64, 0), 64));
    }
}
