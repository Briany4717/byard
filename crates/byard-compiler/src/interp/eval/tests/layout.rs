//! Layout: `Grid`, `ZStack`, default text wrap, structural `for` and `when`,
//! and `Spacer` flex.

use super::controls::checkbox_frame;
use super::paint::rendered_frame;
use super::*;

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
    let src = include_str!("../../../../examples/reactive_demo.byd");
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

// ── `Text` in a `Row` or `Column`: layout attributes, fixed-width cells ──

/// Lowers `src`, renders it at 400×300 and hands back the frame, requiring the
/// program to compile clean.
fn text_frame(src: &str) -> byard_core::frame::RenderFrame {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame
}

fn text_x(frame: &byard_core::frame::RenderFrame, text: &str) -> f32 {
    frame
        .texts()
        .iter()
        .find(|t| t.text == text)
        .unwrap_or_else(|| panic!("no run {text:?}"))
        .x
}

fn text_y(frame: &byard_core::frame::RenderFrame, text: &str) -> f32 {
    frame.texts().iter().find(|t| t.text == text).unwrap().y
}

#[test]
fn a_text_with_grow_takes_the_free_space_of_its_row() {
    // `a` grows, so `b` is pushed to the far edge of the 300px row.
    let frame = text_frame(
        "View C() {\n Row #[width: 300] {\n Text(\"a\") #[grow: 1]\n Text(\"b\")\n }\n}",
    );
    let b = text_x(&frame, "b");
    assert!(b > 280.0, "b sits at the right edge of the row, got {b}");
}

#[test]
fn texts_with_grow_and_a_zero_basis_split_the_row_evenly() {
    // `basis: 0` says "start from nothing", so two `grow: 1` cells are one half
    // each and the second starts at the same x in every row, whatever the
    // first says. A table's columns line up.
    let src = |a: &str| {
        format!(
            "View C() {{\n Row #[width: 400, gap: 10] {{\n Text(\"{a}\") #[grow: 1, basis: 0, wrap: false]\n Text(\"note\") #[grow: 1, basis: 0, wrap: false]\n }}\n}}"
        )
    };
    let short = text_x(&text_frame(&src("ab")), "note");
    let long = text_x(&text_frame(&src("a much longer product name")), "note");
    assert!(
        (short - 205.0).abs() < 1.0,
        "second half starts at 205, got {short}"
    );
    assert!(
        (long - short).abs() < 0.5,
        "same x in every row: {short} vs {long}"
    );
}

#[test]
fn texts_with_only_grow_start_from_their_content_as_boxes_do() {
    // Without `basis` each starts from its own size and shares what is left,
    // so a longer first cell pushes the second one right.
    let src = |a: &str| {
        format!(
            "View C() {{\n Row #[width: 400, gap: 10] {{\n Text(\"{a}\") #[grow: 1, wrap: false]\n Text(\"note\") #[grow: 1, wrap: false]\n }}\n}}"
        )
    };
    let short = text_x(&text_frame(&src("ab")), "note");
    let long = text_x(&text_frame(&src("a much longer product name")), "note");
    assert!(long > short + 20.0, "{short} vs {long}");
}

#[test]
fn a_text_with_height_padding_and_margin_lays_out_as_a_box_would() {
    // `height` is the room the next sibling is placed after.
    let frame =
        text_frame("View C() {\n Column {\n Text(\"a\") #[height: 50]\n Text(\"b\")\n }\n}");
    assert!(text_y(&frame, "b") >= 50.0, "b under a's 50px");

    // `p` insets the glyphs, `m` the whole leaf: both move the run.
    let frame = text_frame("View C() {\n Column {\n Text(\"a\") #[p: 10]\n }\n}");
    assert!((text_x(&frame, "a") - 10.0).abs() < 1.0, "padded run");
    assert!((text_y(&frame, "a") - 10.0).abs() < 1.0, "padded run");
    let frame = text_frame("View C() {\n Column {\n Text(\"a\") #[ml: 7, mt: 3]\n }\n}");
    assert!(
        (text_x(&frame, "a") - 7.0).abs() < 1.0,
        "margin moves the run"
    );
    assert!(
        (text_y(&frame, "a") - 3.0).abs() < 1.0,
        "margin moves the run"
    );
}

#[test]
fn padding_widens_a_text_by_its_sides() {
    // `a` is padded 10 a side, so `b` starts 20px later than it would plain.
    let plain = text_frame("View C() {\n Row {\n Text(\"a\")\n Text(\"b\")\n }\n}");
    let padded = text_frame("View C() {\n Row {\n Text(\"a\") #[p: 10]\n Text(\"b\")\n }\n}");
    let grew = text_x(&padded, "b") - text_x(&plain, "b");
    assert!((grew - 20.0).abs() < 1.5, "b moved right by 20, got {grew}");
}

#[test]
fn a_nonwrapping_text_with_a_width_is_that_wide() {
    // The cell is 40 wide though its text is much wider, so `b` starts at 40.
    let frame = text_frame(
        "View C() {\n Row {\n Text(\"a rather long cell\") #[width: 40, wrap: false]\n \
           Text(\"b\")\n }\n}",
    );
    let b = text_x(&frame, "b");
    assert!(
        (b - 40.0).abs() < 1.0,
        "b starts after the 40px cell, got {b}"
    );
}

#[test]
fn a_nonwrapping_text_wider_than_its_width_is_clipped_to_it() {
    let frame = text_frame(
        "View C() {\n Row {\n Text(\"a rather long cell\") #[width: 40, wrap: false]\n }\n}",
    );
    assert!(frame.text_wraps()[0].is_none(), "still one line");
    let clip = frame.text_clips()[0].expect("clipped to its cell");
    let rect = frame.clips()[usize::from(clip)];
    assert!(
        (rect.rect.width - 40.0).abs() < 1.0,
        "the clip is the cell's 40px, got {rect:?}"
    );
}

#[test]
fn a_nonwrapping_text_without_a_width_is_not_clipped() {
    let frame = text_frame(
        "View C() {\n Column #[width: 60] {\n Text(\"a rather long line\") #[wrap: false]\n }\n}",
    );
    assert!(frame.text_clips()[0].is_none(), "it overflows on purpose");
}

#[test]
fn leading_spaces_in_a_text_are_kept() {
    // Spaces are text like any other: they are drawn and they take room.
    let frame = text_frame(
        "View C() {\n var e = \"oops\"\n Row {\n Text(\"ab\")\n Text(\"   {e}\")\n \
           Text(\"   z\") #[wrap: false]\n }\n}",
    );
    assert!(frame.texts().iter().any(|t| t.text == "   oops"));
    assert!(frame.texts().iter().any(|t| t.text == "   z"));
    let plain = text_frame("View C() {\n Row {\n Text(\"ab\")\n Text(\"oops\")\n }\n}");
    let wrap_spaced = frame.text_wraps()[1].unwrap();
    let wrap_plain = plain.text_wraps()[1].unwrap();
    assert!(
        wrap_spaced > wrap_plain + 4.0,
        "three spaces widen the run: {wrap_spaced} vs {wrap_plain}"
    );
}
