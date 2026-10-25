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
mod lowering;
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

// ── RFC-0018: Grid ────────────────────────────────────────────────────

#[test]
fn grid_auto_places_children_into_columns() {
    // Two 1fr columns in a 200px-wide grid → 100px each; two auto-flowed
    // children land one per column (x≈0 and x≈100).
    let frame = checkbox_frame(
        "View C() {\n Grid #[columns: \"1fr 1fr\", width: 200, height: 100] {\n \
               Box #[bg: 0xFF0000, height: 40] {}\n \
               Box #[bg: 0x00FF00, height: 40] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2, "two child fills");
    let mut xs: Vec<f32> = frame.instances().iter().map(|i| i.rect[0]).collect();
    xs.sort_by(f32::total_cmp);
    assert!(xs[0] < 5.0, "first column near x=0, got {xs:?}");
    assert!(
        (xs[1] - 100.0).abs() < 5.0,
        "second column near x=100, got {xs:?}"
    );
}

#[test]
fn grid_explicit_col_placement_pins_a_child() {
    // A lone child pinned to column 2 sits in the right column (x≈100),
    // proving `set_grid_item` wires `col:` through to Taffy.
    let frame = checkbox_frame(
        "View C() {\n Grid #[columns: \"1fr 1fr\", width: 200, height: 100] {\n \
               Box #[bg: 0xFF0000, height: 40, col: 2] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 1, "one child fill");
    assert!(
        (frame.instances()[0].rect[0] - 100.0).abs() < 5.0,
        "pinned to column 2 (x≈100), got {:?}",
        frame.instances()[0].rect
    );
}

#[test]
fn grid_row_gap_offsets_the_second_row() {
    // One 1fr column, two explicit 40px rows, row_gap 20 → the second row
    // starts at y = 40 + 20 = 60. (Explicit row tracks so the assertion is
    // independent of grid `align-content`, which stretches *auto* rows to
    // fill a fixed-height container.)
    let frame = checkbox_frame(
        "View C() {\n Grid #[columns: \"1fr\", rows: \"40 40\", row_gap: 20, width: 100] {\n \
               Box #[bg: 0xFF0000] {}\n \
               Box #[bg: 0x00FF00] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2);
    let mut ys: Vec<f32> = frame.instances().iter().map(|i| i.rect[1]).collect();
    ys.sort_by(f32::total_cmp);
    assert!(ys[0] < 5.0, "first row at top, got {ys:?}");
    assert!(
        (ys[1] - 60.0).abs() < 5.0,
        "second row after 40px row + 20px gap, got {ys:?}"
    );
}

#[test]
fn grid_invalid_template_reports_a_diagnostic_and_still_renders() {
    let parsed = parse("View C() {\n Grid #[columns: \"1fr bogus\"] { Box #[bg: 0xFF0000] {} }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::InvalidGridTemplate { .. })),
        "expected InvalidGridTemplate, got {:?}",
        interp.errors()
    );
    // Non-fatal: the grid still lays out (falls back to a single auto column).
    assert_eq!(frame.instances().len(), 1, "the child still renders");
}

// ── RFC-0018: ZStack ──────────────────────────────────────────────────

#[test]
fn zstack_overlaps_children_at_the_same_origin() {
    // Two children with bg: the small one centres over the big one (they
    // overlap), unlike a Column which would stack them vertically.
    let frame = checkbox_frame(
        "View C() {\n ZStack #[width: 100, height: 100] {\n \
               Box #[bg: 0xFF0000, width: 100, height: 100] {}\n \
               Box #[bg: 0x00FF00, width: 40, height: 40] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2, "two child fills");
    // Declaration order: big first (bottom), small second (on top).
    let big = frame.instances()[0].rect;
    let small = frame.instances()[1].rect;
    assert!(
        big[0] < 5.0 && big[1] < 5.0,
        "big child at origin, got {big:?}"
    );
    // Small (40) centred in the 100 stack → (100 − 40) / 2 = 30.
    assert!(
        (small[0] - 30.0).abs() < 5.0 && (small[1] - 30.0).abs() < 5.0,
        "small child centred, got {small:?}"
    );
}

#[test]
fn zstack_alignment_pins_child_to_corner() {
    // `bottom_end` puts the small child at the bottom-right corner.
    let frame = checkbox_frame(
        "View C() {\n ZStack #[width: 100, height: 100, alignment: bottom_end] {\n \
               Box #[bg: 0xFF0000, width: 100, height: 100] {}\n \
               Box #[bg: 0x00FF00, width: 20, height: 20] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2);
    let small = frame.instances()[1].rect;
    assert!(
        (small[0] - 80.0).abs() < 5.0 && (small[1] - 80.0).abs() < 5.0,
        "small child at bottom-right (80,80), got {small:?}"
    );
}

// ── RFC-0005: default text wrap ───────────────────────────────────────

#[test]
fn text_wraps_to_parent_width_by_default() {
    // A long line in a narrow fixed-width column wraps with NO explicit
    // `wrap`/`width`, default wrap reflows it to the column's width.
    let parsed = parse(
        "View C() {\n Column #[width: 120] {\n \
               Text(\"This is a fairly long sentence that must wrap within a narrow column.\")\n \
             }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.texts().len(), 1, "one text run");
    let wrap = frame.text_wraps()[0];
    assert!(wrap.is_some(), "default wrap is on");
    assert!(
        wrap.unwrap() <= 130.0,
        "wrapped to ~the 120px column (not the natural width), got {wrap:?}"
    );
}

#[test]
fn wrap_false_opts_out_to_a_single_line() {
    let parsed = parse(
        "View C() {\n Column #[width: 120] {\n \
               Text(\"A long unwrapped line that overflows the column.\") #[wrap: false]\n \
             }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.texts().len(), 1);
    assert!(
        frame.text_wraps()[0].is_none(),
        "wrap: false → single line (no wrap width), got {:?}",
        frame.text_wraps()[0]
    );
}

#[test]
fn slider_drag_sets_float_value() {
    let parsed =
        parse("View C() {\n var vol = 0.0\n Slider #[bind: vol, min: 0, max: 1, width: 100]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("vol"))
        .unwrap();
    interp.tick();

    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // PointerDown at ~50% of track (x=50 on a 100px track starting at x=0).
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::PointerDown,
        pos: (50.0, 5.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp.tick();

    let val = match interp.peek(sig) {
        Value::Float(f) => f,
        other => panic!("expected Float, got {other:?}"),
    };
    assert!(
        (val - 0.5).abs() < 0.1,
        "slider at 50% should be ~0.5, got {val}"
    );
}

// ── Structural for/when in render tree ──────────────────────────

#[test]
fn when_true_includes_then_branch() {
    // RFC-0018: `when` lowers to one reactive `When` node; its taken branch is
    // expanded at paint time. With the condition true, the branch paints.
    let parsed = parse("View C() {\n var show = true\n when show { Text(\"visible\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(tree.len(), 1);
    assert!(
        matches!(tree[0], RenderNode::When { .. }),
        "when → When node"
    );
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(frame.texts().len(), 1, "then-branch paints when true");
    assert_eq!(frame.texts()[0].text, "visible");
}

#[test]
fn when_false_emits_nothing_without_else() {
    let parsed = parse("View C() {\n var hide = false\n when hide { Text(\"hidden\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(frame.texts().is_empty(), "false, no else → nothing paints");
}

#[test]
fn when_reacts_to_a_var_flip_at_runtime() {
    // RFC-0018: the whole point, flipping the guard `var` mounts/unmounts the
    // subtree at runtime, with no re-lowering.
    let parsed = parse(
        "View C() {\n var show = false\n when show { Text(\"hi\") } else { Text(\"bye\") }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let show = interp
        .var_signal(&crate::symbol::Symbol::intern("show"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(frame.texts()[0].text, "bye", "else branch first");

    interp.write_var(show, Value::Bool(true));
    interp.tick();
    let mut frame2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame2, 400.0, 300.0);
    assert_eq!(frame2.texts()[0].text, "hi", "then branch after flip");
}

#[test]
fn for_loop_emits_one_node_per_item() {
    // RFC-0018: `for` lowers to one reactive `For` node; the driver renders one
    // pooled body per element at paint time.
    let parsed =
        parse("View C() {\n var items = [1, 2, 3]\n for item in items { Text(\"{item}\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert!(matches!(tree[0], RenderNode::For { .. }), "for → For node");
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let texts: Vec<&str> = frame.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, ["1", "2", "3"], "one node per item, in order");
}

#[test]
fn for_reacts_to_list_growth_and_element_change() {
    // RFC-0018: growing the list mounts more rows; changing an element updates
    // its row, all without re-lowering.
    let parsed = parse("View C() {\n var xs = [10, 20]\n for x in xs { Text(\"{x}\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let xs = interp
        .var_signal(&crate::symbol::Symbol::intern("xs"))
        .unwrap();
    interp.tick();
    let mut f = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f, 400.0, 300.0);
    let t: Vec<&str> = f.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(t, ["10", "20"]);

    // Grow + change: [10, 20] → [10, 99, 30].
    interp.write_var(
        xs,
        Value::List(vec![Value::Int(10), Value::Int(99), Value::Int(30)]),
    );
    interp.tick();
    let mut f2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f2, 400.0, 300.0);
    let t2: Vec<&str> = f2.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(t2, ["10", "99", "30"], "grew to 3 rows, element updated");

    // Shrink: [10, 99, 30] → [7].
    interp.write_var(xs, Value::List(vec![Value::Int(7)]));
    interp.tick();
    let mut f3 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f3, 400.0, 300.0);
    let t3: Vec<&str> = f3.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(t3, ["7"], "shrank to 1 row");
}

// ── RFC-0005: default text wrap ──────────────────────────────────────

#[test]
fn explicit_width_pins_wrap_and_yields_a_taller_leaf() {
    let long = "the quick brown fox jumps over the lazy dog again and again";
    // Same text: the first wraps to the 400px root by default, the second is
    // pinned to width 120 (both wrap, wrap is default now).
    let src = format!("View C() {{\n Text(\"{long}\")\n Text(\"{long}\") #[width: 120]\n}}");
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.texts().len(), 2);
    assert_eq!(frame.text_wraps().len(), 2, "wrap slice parallel to texts");
    let w0 = frame.text_wraps()[0].expect("default wrap on the first line");
    let w1 = frame.text_wraps()[1].expect("explicit-width line still wraps");
    assert!(w0 > 200.0, "first wraps to the wide root, got {w0}");
    assert!(
        (w1 - 120.0).abs() < 1.0,
        "second wraps to its 120 width, got {w1}"
    );

    // The 120-wide leaf is narrower and at least as tall (more lines) as the
    // one that wrapped to the wide root.
    let wide = frame.rects()[1]; // C root is rects[0]; first Text is rects[1]
    let narrow = frame.rects()[2];
    assert!(
        (narrow.width - 120.0).abs() < 1.0,
        "the pinned leaf is 120 wide, got {}",
        narrow.width
    );
    assert!(
        narrow.height >= wide.height,
        "the narrower leaf wraps onto ≥ lines: {} vs {}",
        narrow.height,
        wide.height
    );
}

#[test]
fn reactive_demo_example_reacts_live() {
    // Ties the shipped RFC-0018 example to the suite: `when` toggles a subtree
    // and `for` grows a list, both at runtime.
    let src = include_str!("../../../examples/reactive_demo.byd");
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let show = interp
        .var_signal(&crate::symbol::Symbol::intern("showList"))
        .unwrap();
    let names = interp
        .var_signal(&crate::symbol::Symbol::intern("names"))
        .unwrap();
    let render = |interp: &mut Interpreter| {
        interp.tick();
        let mut f = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut f, 640.0, 520.0);
        f.texts().iter().map(|t| t.text.clone()).collect::<Vec<_>>()
    };

    // Initially: list shown → both names present.
    let t0 = render(&mut interp);
    assert!(t0.contains(&"Ada".to_string()) && t0.contains(&"Alan".to_string()));
    assert!(!t0.iter().any(|s| s.starts_with("List hidden")));

    // Hide the list live.
    interp.write_var(show, Value::Bool(false));
    let t1 = render(&mut interp);
    assert!(!t1.contains(&"Ada".to_string()), "list unmounted live");
    assert!(
        t1.iter().any(|s| s.starts_with("List hidden")),
        "else branch"
    );

    // Show again + grow the list live → four rows.
    interp.write_var(show, Value::Bool(true));
    interp.write_var(
        names,
        Value::List(vec![
            Value::Str("Ada".into()),
            Value::Str("Alan".into()),
            Value::Str("Grace".into()),
            Value::Str("Katherine".into()),
        ]),
    );
    let t2 = render(&mut interp);
    for n in ["Ada", "Alan", "Grace", "Katherine"] {
        assert!(t2.contains(&n.to_string()), "{n} row mounted after growth");
    }
}

// ── RFC-0017: Overlay & z-layer system ───────────────────────────────

/// Finds the emitted solid box closest to the given colour channels.
fn find_solid_by_red(
    frame: &byard_core::frame::RenderFrame,
    red: f32,
) -> Option<(usize, byard_core::BoxInstance)> {
    frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - red).abs() < 0.05)
        .map(|(i, b)| (i, *b))
}

#[test]
fn overlay_takes_no_flow_space_and_paints_above_main() {
    // A main box (red) followed by an overlay whose scrim (blue, grow:1)
    // fills the viewport. The overlay must not displace the main box, and
    // its scrim must paint *above* the main box (nearer draw-order depth).
    let src = "View C() {\n \
            Box #[bg: 0xFF0000, width: 40, height: 40] {}\n \
            Overlay #[modal: false] {\n \
                Box #[bg: 0x0000FF, grow: 1] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let (red_i, red) = find_solid_by_red(&frame, 1.0).expect("main red box emitted");
    let (blue_i, blue) = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| b.color[2] > 0.95 && b.color[0] < 0.05)
        .map(|(i, b)| (i, *b))
        .expect("overlay scrim emitted");

    // Main box keeps its natural 40×40 at the origin, the overlay's 0×0
    // flow leaf did not push it down.
    assert!((red.rect[0]).abs() < 0.01 && (red.rect[1]).abs() < 0.01);
    assert!((red.rect[2] - 40.0).abs() < 0.01);
    // The scrim fills the whole viewport.
    assert!((blue.rect[2] - 400.0).abs() < 0.5 && (blue.rect[3] - 300.0).abs() < 0.5);
    // Painter's order: the overlay is emitted after the main tree, so its
    // depth is strictly nearer (smaller NDC-z) → it composites on top.
    assert!(
        frame.solid_depths()[blue_i] < frame.solid_depths()[red_i],
        "overlay scrim must paint above the main box"
    );
}

#[test]
fn overlay_center_anchor_positions_content_in_the_viewport() {
    let src = "View C() {\n \
            Overlay {\n \
                Column #[anchor: center, bg: 0x222222, width: 100, height: 60] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let dialog = frame
        .instances()
        .iter()
        .find(|b| (b.color[0] - linear(0x22)).abs() < 0.02)
        .expect("dialog emitted");
    // Centred: (400−100)/2 = 150, (300−60)/2 = 120.
    assert!(
        (dialog.rect[0] - 150.0).abs() < 1.0,
        "x centred, got {}",
        dialog.rect[0]
    );
    assert!(
        (dialog.rect[1] - 120.0).abs() < 1.0,
        "y centred, got {}",
        dialog.rect[1]
    );
}

#[test]
fn overlay_bottom_anchor_pins_content_to_the_viewport_bottom() {
    let src = "View C() {\n \
            Overlay {\n \
                Column #[anchor: bottom, bg: 0x333333, width: 200, height: 80] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let sheet = frame
        .instances()
        .iter()
        .find(|b| (b.color[0] - linear(0x33)).abs() < 0.02)
        .expect("sheet emitted");
    // Pinned to the bottom: y = 300 − 80 = 220; centred x = (400−200)/2 = 100.
    assert!(
        (sheet.rect[1] - 220.0).abs() < 1.0,
        "y bottom, got {}",
        sheet.rect[1]
    );
    assert!(
        (sheet.rect[0] - 100.0).abs() < 1.0,
        "x centred, got {}",
        sheet.rect[0]
    );
}

#[test]
fn modal_overlay_blocks_the_main_tree_and_dismisses_on_outside_tap() {
    // A main button sits behind a modal overlay. Its scrim fills the
    // viewport; a small confirm button is centred. Tapping the scrim (an
    // outside tap) fires `dismiss` and must NOT reach the button behind.
    let src = "View C() {\n \
            var open = true\n \
            var behind = false\n \
            var confirmed = false\n \
            Button(\"behind\") #[width: 400, height: 300] => behind = true\n \
            Overlay #[modal: true, dismiss => open = false] {\n \
                Box #[bg: 0x000000, opacity: 0.3, grow: 1] {}\n \
                Column #[anchor: center, width: 80, height: 40] {\n \
                    Button(\"ok\") #[width: 80, height: 40] => confirmed = true\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let open = interp
        .var_signal(&crate::symbol::Symbol::intern("open"))
        .unwrap();
    let behind = interp
        .var_signal(&crate::symbol::Symbol::intern("behind"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tap the top-left corner, over the scrim, outside the centred content.
    let tap = |t: u64, p: (f32, f32)| {
        [
            byard_core::platform::InputEvent {
                kind: byard_core::platform::EventKind::PointerDown,
                pos: p,
                delta: (0.0, 0.0),
                payload: None,
                time_ms: t,
                mods: byard_core::KeyModifiers::default(),
            },
            byard_core::platform::InputEvent {
                kind: byard_core::platform::EventKind::PointerUp,
                pos: p,
                delta: (0.0, 0.0),
                payload: None,
                time_ms: t + 20,
                mods: byard_core::KeyModifiers::default(),
            },
        ]
    };
    interp.dispatch_events(&tap(0, (10.0, 10.0)));
    interp.tick();

    assert_eq!(
        interp.peek(open),
        Value::Bool(false),
        "outside tap dismissed"
    );
    assert_eq!(
        interp.peek(behind),
        Value::Bool(false),
        "modal scrim blocked the button behind it"
    );
}

#[test]
fn modal_overlay_content_wins_over_the_scrim() {
    // A tap on the centred confirm button fires its action, not the scrim's
    // dismiss, the content is registered after the scrim, so it wins.
    let src = "View C() {\n \
            var open = true\n \
            var confirmed = false\n \
            Overlay #[modal: true, dismiss => open = false] {\n \
                Box #[bg: 0x000000, grow: 1] {}\n \
                Column #[anchor: center, width: 80, height: 40] {\n \
                    Button(\"ok\") #[width: 80, height: 40] => confirmed = true\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let open = interp
        .var_signal(&crate::symbol::Symbol::intern("open"))
        .unwrap();
    let confirmed = interp
        .var_signal(&crate::symbol::Symbol::intern("confirmed"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Centre of the viewport = centre of the confirm button.
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (200.0, 150.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (200.0, 150.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 20,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();

    assert_eq!(
        interp.peek(confirmed),
        Value::Bool(true),
        "content button fired"
    );
    assert_eq!(
        interp.peek(open),
        Value::Bool(true),
        "scrim dismiss did NOT fire"
    );
}

#[test]
fn escape_dismisses_the_topmost_modal_overlay() {
    let src = "View C() {\n \
            var open = true\n \
            Overlay #[modal: true, dismiss => open = false] {\n \
                Box #[grow: 1] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let open = interp
        .var_signal(&crate::symbol::Symbol::intern("open"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::KeyDown,
        pos: (0.0, 0.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key("Escape".into())),
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp.tick();
    assert_eq!(
        interp.peek(open),
        Value::Bool(false),
        "Escape dismissed the modal"
    );
}

#[test]
fn when_gated_overlay_unmounts_live_on_dismiss() {
    // RFC-0018 × RFC-0017: a `when`-gated modal overlay now dismisses at
    // runtime, tapping the scrim flips the guard `var`, and the very next
    // render unmounts the overlay (no hot-reload needed). This is the headline
    // reactivity win: overlays are live.
    let src = "View C() {\n \
            var open = true\n \
            when open {\n \
                Overlay #[modal: true, dismiss => open = false] {\n \
                    Box #[bg: 0x000000, opacity: 0.4, grow: 1] {}\n \
                    Column #[anchor: center, bg: 0xFFFFFF, width: 100, height: 60] {}\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // The overlay is mounted: the white dialog surface is present.
    assert!(
        frame.instances().iter().any(|b| b.color[0] > 0.95),
        "overlay mounted initially"
    );

    // Tap the scrim (top-left, outside the centred content).
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 20,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();
    let mut frame2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame2, 400.0, 300.0);
    // `open` flipped false → the `when` unmounts the overlay entirely.
    assert!(
        !frame2.instances().iter().any(|b| b.color[0] > 0.95),
        "overlay unmounted live after dismiss"
    );
    assert!(frame2.instances().is_empty(), "nothing left on screen");
}

#[test]
fn non_modal_overlay_lets_taps_fall_through() {
    // A non-modal overlay (a snackbar-style surface) must not block the main
    // tree: a tap on the button behind still fires.
    let src = "View C() {\n \
            var behind = false\n \
            Button(\"behind\") #[width: 400, height: 300] => behind = true\n \
            Overlay #[modal: false] {\n \
                Column #[anchor: bottom, width: 100, height: 20] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let behind = interp
        .var_signal(&crate::symbol::Symbol::intern("behind"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tap top-left, away from the bottom-anchored surface.
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (10.0, 10.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (10.0, 10.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 20,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(behind),
        Value::Bool(true),
        "non-modal overlay let the tap fall through"
    );
}

#[test]
fn overlay_demo_example_renders_dialog_above_the_base_app() {
    // Ties the shipped visual example to the test suite: it must parse,
    // lower, and render, with the modal dialog surface compositing above the
    // base app (RFC-0017). Guards the demo against silent breakage.
    let src = include_str!("../../../examples/overlay_demo.byd");
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 900.0, 560.0);

    // The base app background (0x14141C) is emitted early; the dialog surface
    // (0xECE6F0) is an overlay emitted later, so it sits at a nearer depth than
    // the base app. Both are matched by their written colour, decoded: the
    // frame is in linear light.
    let base = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - linear(0x14)).abs() < 0.01)
        .map(|(i, _)| i)
        .expect("base app background emitted");
    let (dialog, dialog_box) = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| {
            (b.color[0] - linear(0xEC)).abs() < 0.02 && (b.color[1] - linear(0xE6)).abs() < 0.02
        })
        .map(|(i, b)| (i, *b))
        .expect("dialog surface emitted");
    assert!(
        frame.solid_depths()[dialog] < frame.solid_depths()[base],
        "the modal dialog must composite above the base app"
    );

    // No dialog text line may overflow the dialog surface, line wrap is not
    // built yet, so the example is authored to fit. Guards the reported
    // overflow against regression: every dark-on-light label painted inside
    // the surface must end before the surface's right edge.
    let mut measurer = byard_core::text::TextMeasurer::new();
    let surf_left = dialog_box.rect[0];
    let surf_right = dialog_box.rect[0] + dialog_box.rect[2];
    for (line, wrap) in frame.texts().iter().zip(frame.text_wraps()) {
        let inside = line.x >= surf_left && line.x < surf_right && line.color[0] < 0.5;
        if inside {
            // Honour the wrap width (RFC-0018): a wrapped label's laid-out
            // width is bounded to it, not the full one-line measurement.
            let (w, _) = measurer.measure_wrapped(&line.text, line.font_size, *wrap, 400, None);
            assert!(
                line.x + w <= surf_right + 0.5,
                "dialog text {:?} overflows the surface: {} + {} > {}",
                line.text,
                line.x,
                w,
                surf_right
            );
        }
    }
}

#[test]
fn nested_overlays_stack_in_mount_order() {
    // An overlay whose content mounts a second overlay: both are collected,
    // and the inner one is emitted later (on top).
    let src = "View C() {\n \
            Overlay #[modal: false] {\n \
                Box #[bg: 0x111111, grow: 1] {}\n \
                Overlay #[modal: false] {\n \
                    Box #[bg: 0x222222, grow: 1] {}\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let outer = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - linear(0x11)).abs() < 0.01)
        .map(|(i, _)| i)
        .expect("outer overlay box");
    let inner = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - linear(0x22)).abs() < 0.01)
        .map(|(i, _)| i)
        .expect("inner overlay box");
    assert!(
        frame.solid_depths()[inner] < frame.solid_depths()[outer],
        "nested overlay stacks above its parent overlay"
    );
}

// ── RFC-0023 ripple ink ─────────────────────────────────────────────

/// A 200×100 box at the layout origin with a semi-transparent white
/// ripple, triggered by `on pressed`, the RFC-0023 guide example shape.
const RIPPLE_SRC: &str = "View V() {
        let btn = style {
            bg: 0x6750A4, radius: 20, ripple: 0x80FFFFFF
            on pressed { ripple_active: true }
        }
        Box #[..btn, width: 200, height: 100] {}
    }";

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

/// Lowers `src`, renders once at `t = 0` (registering the hit region),
/// presses at `(x, y)` with press identity `press_t`, and renders once at
/// `t = 10`, the frame that spawns the ripple, so its `start_ms` is 10.
fn pressed_ripple_named(src: &str, x: f32, y: f32, press_t: u64) -> (Interpreter, Vec<RenderNode>) {
    use byard_core::platform::EventKind as K;
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.set_now_ms(0);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[pointer(K::PointerDown, x, y, press_t)]);
    let spawn_frame = render_at(&mut interp, &tree, 10);
    assert_eq!(
        spawn_frame.ripples().len(),
        1,
        "the press spawns one ripple"
    );
    (interp, tree)
}

/// [`pressed_ripple_named`] over the canonical [`RIPPLE_SRC`] view.
fn pressed_ripple_interp(x: f32, y: f32, press_t: u64) -> (Interpreter, Vec<RenderNode>) {
    pressed_ripple_named(RIPPLE_SRC, x, y, press_t)
}

/// Renders `tree` at `ms` and returns the frame.
fn render_at(
    interp: &mut Interpreter,
    tree: &[RenderNode],
    ms: u32,
) -> byard_core::frame::RenderFrame {
    interp.set_now_ms(ms);
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(tree, &mut frame, 400.0, 300.0);
    frame
}

#[test]
fn a_press_spawns_a_ripple_at_the_tap_point() {
    let (mut interp, tree) = pressed_ripple_interp(30.0, 40.0, 1);
    let frame = render_at(&mut interp, &tree, 50);

    assert_eq!(frame.ripples().len(), 1, "one press spawns one ripple");
    let r = frame.ripples()[0];
    // The circle is centred on the tap point, in absolute coordinates.
    assert!(
        (r.params[0] - 30.0).abs() < 0.01,
        "center x: {:?}",
        r.params
    );
    assert!(
        (r.params[1] - 40.0).abs() < 0.01,
        "center y: {:?}",
        r.params
    );
    // Mid-animation: expanding and still visible.
    assert!(r.params[2] > 0.0, "radius has started expanding");
    assert!(r.params[3] > 0.0 && r.params[3] < 1.0, "fade in flight");
    // The ink colour is the resolved `ripple:` colour (50% white).
    assert!((r.color[3] - 0.5).abs() < 0.01, "ink alpha: {:?}", r.color);
    // Clipped to the element's rounded rect: rect and radii carried over.
    assert_eq!(r.rect, [0.0, 0.0, 200.0, 100.0]);
    assert_eq!(r.radii, [20.0; 4]);
    assert!(
        interp.has_active_animations(),
        "a live ripple keeps requesting frames"
    );
}

#[test]
fn ripple_expands_monotonically_and_retires_after_its_duration() {
    let (mut interp, tree) = pressed_ripple_interp(30.0, 40.0, 1);
    let r1 = render_at(&mut interp, &tree, 50).ripples()[0].params[2];
    let f2 = render_at(&mut interp, &tree, 150);
    let r2 = f2.ripples()[0].params[2];
    assert!(r2 > r1, "radius grows with time ({r1} → {r2})");

    // Past the 300 ms default duration the ripple is gone, and with it the
    // frame demand (no other animation is in flight in this view).
    let f3 = render_at(&mut interp, &tree, 400);
    assert!(f3.ripples().is_empty(), "faded ripple retires");
    assert!(!interp.has_active_animations(), "no lingering frame demand");
}

#[test]
fn the_auto_max_radius_reaches_the_farthest_corner() {
    // Tap at the top-left origin of the 200×100 box: the farthest corner
    // is (200, 100), so the ink must grow to cover hypot(200, 100).
    let (mut interp, tree) = pressed_ripple_interp(0.0, 0.0, 1);
    let expected = 200.0_f32.hypot(100.0);
    // One ms before retirement the ease-out ramp has essentially arrived.
    let frame = render_at(&mut interp, &tree, 299);
    let r = frame.ripples()[0].params[2];
    assert!(
        r > expected * 0.99,
        "radius {r} must reach the farthest corner ({expected})"
    );
}

#[test]
fn a_hold_spawns_once_while_rapid_taps_spawn_one_ripple_each() {
    use byard_core::platform::EventKind as K;
    let (mut interp, tree) = pressed_ripple_interp(30.0, 40.0, 1);
    // Held press across two renders: still exactly one ripple.
    let _ = render_at(&mut interp, &tree, 20);
    let held = render_at(&mut interp, &tree, 40);
    assert_eq!(held.ripples().len(), 1, "a hold never respawns");

    // Release and tap again (a fresh press identity): a second ripple
    // joins the first, still fading, their ink pools on the GPU.
    interp.dispatch_events(&[pointer(K::PointerUp, 30.0, 40.0, 60)]);
    interp.dispatch_events(&[pointer(K::PointerDown, 80.0, 50.0, 80)]);
    let both = render_at(&mut interp, &tree, 100);
    assert_eq!(both.ripples().len(), 2, "each tap spawns its own ripple");
}

#[test]
fn ripple_radius_and_duration_props_override_the_defaults() {
    let src = "View V() {
            let btn = style {
                bg: 0x6750A4, ripple: 0x80FFFFFF
                ripple_radius: 10, ripple_duration: 100
                on pressed { ripple_active: true }
            }
            Box #[..btn, width: 200, height: 100] {}
        }";
    // Spawned at t = 10 by the helper.
    let (mut interp, tree) = pressed_ripple_named(src, 30.0, 40.0, 1);

    let mid = render_at(&mut interp, &tree, 100);
    assert_eq!(mid.ripples().len(), 1);
    assert!(
        mid.ripples()[0].params[2] <= 10.0,
        "`ripple_radius` caps the expansion: {:?}",
        mid.ripples()[0].params
    );
    let done = render_at(&mut interp, &tree, 120);
    assert!(
        done.ripples().is_empty(),
        "`ripple_duration: 100` retires the ink after 100 ms"
    );
}

#[test]
fn a_ripple_without_an_active_trigger_never_spawns() {
    use byard_core::platform::EventKind as K;
    // `ripple:` set but no `ripple_active` anywhere: pressing must not ink.
    let src = "View V() {
            Box #[bg: 0x6750A4, ripple: 0x80FFFFFF, width: 200, height: 100] {}
        }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.set_now_ms(0);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[pointer(K::PointerDown, 30.0, 40.0, 1)]);
    let frame = render_at(&mut interp, &tree, 50);
    assert!(frame.ripples().is_empty(), "no trigger, no ink");
}

#[test]
fn ripple_depth_sits_between_the_background_and_the_children() {
    // A box with a child label: the ink must draw above the box fill but
    // beneath the text (RFC-0023 compositing order).
    let src = "View V() {
            let btn = style {
                bg: 0x6750A4, ripple: 0x80FFFFFF
                on pressed { ripple_active: true }
            }
            Box #[..btn, width: 200, height: 100] {
                Text(\"Save\") #[color: 0xFFFFFF]
            }
        }";
    let (mut interp, tree) = pressed_ripple_named(src, 30.0, 40.0, 1);
    let frame = render_at(&mut interp, &tree, 50);
    let ripple_depth = frame.ripples()[0].depth;
    let bg_depth = frame.solid_depths()[0];
    let text_depth = frame.text_depths()[0];
    // Later emission = nearer = smaller NDC-z.
    assert!(
        ripple_depth < bg_depth,
        "ink above the background ({ripple_depth} vs {bg_depth})"
    );
    assert!(
        text_depth < ripple_depth,
        "children above the ink ({text_depth} vs {ripple_depth})"
    );
}

// ── RFC-0023 §2 backdrop blur / vibrancy ────────────────────────────

fn rendered_frame(src: &str) -> (Interpreter, byard_core::frame::RenderFrame) {
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    (interp, frame)
}

#[test]
fn blur_props_emit_a_backdrop_with_the_resolved_fields() {
    let (_interp, frame) = rendered_frame(
        "View V() { Box #[width: 200, height: 100, radius: 16, blur: 20, \
             backdrop_tint: 0x80FFFFFF, blur_saturation: 2.0, blur_quality: high] {} }",
    );
    assert_eq!(frame.backdrops().len(), 1);
    let b = frame.backdrops()[0];
    assert_eq!(b.rect, [0.0, 0.0, 200.0, 100.0]);
    assert_eq!(b.radii, [16.0; 4]);
    assert!((b.blur - 20.0).abs() < f32::EPSILON);
    assert!((b.tint[3] - 0.5).abs() < 0.01, "tint alpha: {:?}", b.tint);
    assert!((b.saturation - 2.0).abs() < f32::EPSILON);
    assert_eq!(b.quality, byard_core::frame::BLUR_QUALITY_HIGH);
}

#[test]
fn blur_clamps_to_the_max_radius_and_defaults_saturation() {
    let (_interp, frame) =
        rendered_frame("View V() { Box #[width: 100, height: 100, blur: 500] {} }");
    let b = frame.backdrops()[0];
    assert!(
        (b.blur - byard_core::frame::BLUR_MAX_RADIUS).abs() < f32::EPSILON,
        "500 clamps to the max, got {}",
        b.blur
    );
    assert!((b.saturation - 1.8).abs() < 0.01, "vibrancy default 1.8");
    assert_eq!(b.quality, byard_core::frame::BLUR_QUALITY_AUTO);
}

#[test]
fn a_tint_without_blur_lowers_to_a_plain_translucent_fill() {
    // No blur → no barrier, no off-screen work: the identical composite
    // is a translucent fill over the content behind.
    let (_interp, frame) =
        rendered_frame("View V() { Box #[width: 100, height: 100, backdrop_tint: 0x80336699] {} }");
    assert!(frame.backdrops().is_empty(), "no blur, no backdrop barrier");
    let wash = frame
        .decorated()
        .iter()
        .find(|d| (d.base.color[3] - 0.5).abs() < 0.01)
        .expect("the tint lowers to a translucent decorated fill");
    assert_eq!(wash.base.rect, [0.0, 0.0, 100.0, 100.0]);
}

// ── Spacer flexes (RFC-0005) ─────────────────────────────────────────

#[test]
fn a_spacer_absorbs_the_free_space_of_its_row() {
    // RFC-0005: `Spacer` is a *flexible* gap (`grow`, default 1). Before it
    // flexed, a trailing item sat glued to the leading one instead of at the
    // far end of the row.
    let (_interp, frame) = rendered_frame(
        "View V() { Row #[width: 300, height: 20] { \
                 Box #[bg: 0xFF0000, width: 20, height: 20] {} \
                 Spacer \
                 Box #[bg: 0x00FF00, width: 20, height: 20] {} \
             } }",
    );
    let x_of = |red: bool| {
        frame
            .instances()
            .iter()
            .find(|b| (b.color[0] > 0.8) == red && (b.color[1] > 0.8) != red)
            .map(|b| b.rect[0])
            .expect("both boxes are emitted")
    };
    assert!((x_of(true) - 0.0).abs() < 0.5, "the first box stays put");
    assert!(
        (x_of(false) - 280.0).abs() < 0.5,
        "the second is pushed to the far end, got {}",
        x_of(false)
    );
}

#[test]
fn a_spacer_honours_grow_and_basis() {
    // `grow: 0` degenerates to a fixed `basis`-sized gap; two spacers with
    // different `grow` split the free space in proportion.
    let (_interp, frame) = rendered_frame(
        "View V() { Row #[width: 300, height: 20] { \
                 Spacer #[grow: 0, basis: 40] \
                 Box #[bg: 0xFF0000, width: 20, height: 20] {} \
                 Spacer #[grow: 1] \
                 Box #[bg: 0x00FF00, width: 20, height: 20] {} \
                 Spacer #[grow: 3] \
             } }",
    );
    let x_of = |red: bool| {
        frame
            .instances()
            .iter()
            .find(|b| (b.color[0] > 0.8) == red && (b.color[1] > 0.8) != red)
            .map(|b| b.rect[0])
            .unwrap()
    };
    assert!((x_of(true) - 40.0).abs() < 0.5, "the fixed 40px gap held");
    // 300 − 40 − 20 − 20 = 220 free, split 1 : 3 → 55 then 165.
    assert!(
        (x_of(false) - (40.0 + 20.0 + 55.0)).abs() < 1.0,
        "the free space split 1:3, got {}",
        x_of(false)
    );
}

#[test]
fn the_backdrop_barrier_snapshots_the_content_behind_it() {
    // A solid card behind, then a bg-less glass pane with a child label:
    // the barrier must capture the card (and nothing later), and the
    // pane's depth must sit between the card and the label.
    let src = "View V() {
            Column #[width: 300, height: 200] {
                Box #[bg: 0xFF3366, width: 300, height: 80] {}
                Box #[blur: 12, width: 300, height: 80] {
                    Text(\"glass\") #[color: 0x000000]
                }
            }
        }";
    let (_interp, frame) = rendered_frame(src);
    assert_eq!(frame.backdrops().len(), 1);
    let mark = frame.backdrop_marks()[0];
    assert_eq!(mark.solid, 1, "the card was emitted behind the barrier");
    assert_eq!(mark.text, 0, "the pane's own label comes after it");
    let d = frame.backdrops()[0].depth;
    assert!(d < frame.solid_depths()[0], "pane above the card");
    assert!(frame.text_depths()[0] < d, "label above the pane");
}

#[test]
fn blur_animates_as_a_paint_prop() {
    let sample = |f: &byard_core::frame::RenderFrame| f.backdrops().first().map_or(0.0, |b| b.blur);
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[width: 100, height: 100, blur: on ? 16.0 : 4.0 with anim.linear(1000)] }",
        sample,
    );
    assert!((a - 4.0).abs() < 0.5, "starts near 4, got {a}");
    assert!((b - 10.0).abs() < 1.5, "~halfway, got {b}");
    assert!((c - 16.0).abs() < 0.5, "arrives at 16, got {c}");
}

#[test]
fn a_state_override_retargets_the_base_with_animation() {
    use byard_core::platform::EventKind as K;
    // RFC-0010 × RFC-0012/0016: the state changes the target, the base
    // owns the curve, `on hover { blur: 16 }` over
    // `blur: 4 with anim.linear(1000)` must ramp, not pop.
    let src = "View V() {
            let glass = style {
                blur: 4.0 with anim.linear(1000)
                on hover { blur: 16 }
            }
            Box #[..glass, width: 200, height: 100] {}
        }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.set_now_ms(0);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Hover the pane; the next renders sample the retargeted ramp.
    interp.dispatch_events(&[pointer(K::PointerMove, 50.0, 50.0, 1)]);
    let blur_at = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame.backdrops().first().map_or(-1.0, |b| b.blur)
    };
    let a = blur_at(&mut interp, 0);
    let b = blur_at(&mut interp, 500);
    let c = blur_at(&mut interp, 1000);
    assert!((a - 4.0).abs() < 0.5, "starts at the base value, got {a}");
    assert!(
        (b - 10.0).abs() < 1.5,
        "~halfway to the hover target, got {b}"
    );
    assert!(
        (c - 16.0).abs() < 0.5,
        "arrives at the hover target, got {c}"
    );
}

#[test]
fn an_animated_color_ramps_its_alpha_channel_too() {
    // RFC-0023 regression: a translucent `backdrop_tint` fades in, the
    // alpha byte animates alongside the OKLab channels. The old 3-channel
    // path dropped alpha entirely, collapsing `0x00FFFFFF → 0x80FFFFFF`
    // into an instant opaque white.
    let sample =
        |f: &byard_core::frame::RenderFrame| f.backdrops().first().map_or(-1.0, |b| b.tint[3]);
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[width: 100, height: 100, blur: 8, \
             backdrop_tint: on ? 0x80FFFFFF : 0x00FFFFFF with anim.linear(1000)] }",
        sample,
    );
    assert!(a.abs() < 0.02, "starts fully transparent, got {a}");
    assert!(
        (b - 0.25).abs() < 0.05,
        "~half the 0.5 target mid-ramp, got {b}"
    );
    assert!((c - 0.502).abs() < 0.02, "arrives at 0x80 alpha, got {c}");
}

#[test]
fn deepest_rect_overlap_counts_the_tallest_stack() {
    let r = |x: f32| [x, 0.0, 100.0, 100.0];
    assert_eq!(deepest_rect_overlap(&[]), 0);
    assert_eq!(deepest_rect_overlap(&[r(0.0)]), 1);
    // Disjoint panes never stack.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(200.0), r(400.0)]), 1);
    // Two overlapping panes plus a distant third: the cluster is 2.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(80.0), r(400.0)]), 2);
    // A chain a∩b, b∩c clusters to 3 around b, deliberately
    // conservative (see the fn docs), erring toward the diagnostic.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(80.0), r(160.0)]), 3);
    // Three co-located panes: a genuine 3-deep stack.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(10.0), r(20.0)]), 3);
}

#[test]
fn three_stacked_glass_panes_raise_the_overlap_warning() {
    let (interp, frame) = rendered_frame(
        "View V() { ZStack #[width: 200, height: 200] { \
             Box #[blur: 8, width: 180, height: 180] {} \
             Box #[blur: 8, width: 160, height: 160] {} \
             Box #[blur: 8, width: 140, height: 140] {} } }",
    );
    assert_eq!(frame.backdrops().len(), 3);
    assert_eq!(
        interp.perf_warnings(),
        &[PerfWarning::OverlappingBlurs { count: 3 }]
    );

    // Two panes are fine, stacked glass only warns at three.
    let (interp, _frame) = rendered_frame(
        "View V() { ZStack #[width: 200, height: 200] { \
             Box #[blur: 8, width: 180, height: 180] {} \
             Box #[blur: 8, width: 160, height: 160] {} } }",
    );
    assert!(
        interp.perf_warnings().is_empty(),
        "{:?}",
        interp.perf_warnings()
    );
}
