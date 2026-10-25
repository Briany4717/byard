use super::animation::hex_from_oklab;
use super::animation::oklab_from_hex;
use super::expr::eval_binary;
use super::expr::eval_compare;
use super::expr::eval_concat;
use super::expr::index_value;
use super::expr::structural_eq;
use super::instance::MAX_INSTANCE_DEPTH;
use super::states::resolve_state_attrs;
use super::*;
use crate::parser::ast::AttrKind;
use crate::parser::ast::BinOp;
use crate::parser::ast::ElementNode;
use crate::parser::ast::StyleStateKind;
use crate::parser::parse;
use animation::ramp_paint_prop;
use controls::checkbox_frame;

mod animation;
mod canvas;
mod controls;
mod instance;
mod layout;
mod lowering;
mod overlay;
mod paint;
mod props;
mod scroll;
mod states;

fn element(m: &Member) -> &ElementNode {
    match m {
        Member::Element(e) => e,
        _ => panic!("expected element"),
    }
}

// ── user-view registry & call-site recognition ──────────────────

/// Loads a multi-view file and lowers the named view to a render tree.
/// The linear value of an sRGB byte, for tests that identify a primitive by the
/// colour it was written with.
///
/// A colour literal is written encoded (`0x5B8DEF` is what the design tool
/// says) and reaches the frame decoded, because everything past the compiler
/// blends in linear light. A test comparing against `byte / 255` is comparing
/// the two spaces, which is exactly the mistake the double-encoding hid.
fn linear(byte: u8) -> f32 {
    crate::interp::intrinsics::srgb_to_linear(f32::from(byte) / 255.0)
}

fn lower_named(src: &str, name: &str) -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let view = parsed
        .views
        .iter()
        .find(|v| v.name.as_str() == name)
        .unwrap();
    let tree = interp.lower_view(view, &known);
    (interp, tree)
}

fn pointer(
    kind: byard_core::platform::EventKind,
    x: f32,
    y: f32,
    t: u64,
) -> byard_core::platform::InputEvent {
    byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: t,
        mods: byard_core::KeyModifiers::default(),
    }
}
