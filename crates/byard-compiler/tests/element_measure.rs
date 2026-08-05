//! Element self-measurement, end to end (RFC-0038).
//!
//! `on measure => size = it` hands an element its own laid-out rect as a
//! reactive value. The tests that matter here are not "the number is right",
//! they are the two properties that make the event safe to leave switched on in
//! a real screen: it fires **only when the rect changed**, and a size fed back
//! into the layout that produced it is caught or bounded rather than allowed to
//! oscillate.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, PerfWarning, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;

struct Harness {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    viewport: (f32, f32),
}

impl Harness {
    fn new(source: &str) -> Self {
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let tree = interp.lower_view(&parsed.views[0], &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        interp.tick();
        let mut harness = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
            viewport: (800.0, 600.0),
        };
        harness.render();
        harness
    }

    /// One frame: settle the reactive graph, then lay out and paint.
    fn render(&mut self) {
        self.interp.tick();
        self.frame.clear();
        let (w, h) = self.viewport;
        self.interp.render(&self.tree, &mut self.frame, w, h);
    }

    fn resize(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
        self.render();
    }

    fn var(&self, name: &str) -> Value {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(name))
            .expect("a declared var");
        self.interp.peek(sig)
    }

    fn int(&self, name: &str) -> i64 {
        match self.var(name) {
            Value::Int(n) => n,
            other => panic!("`{name}` is {other:?}, not an Int"),
        }
    }

    fn float(&self, name: &str) -> f64 {
        match self.var(name) {
            Value::Float(f) => f,
            Value::Int(n) => f64::from(i32::try_from(n).expect("a pixel extent")),
            other => panic!("`{name}` is {other:?}, not a number"),
        }
    }

    fn set(&mut self, name: &str, value: Value) {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(name))
            .expect("a declared var");
        self.interp.write_var(sig, value);
        self.render();
    }
}

/// A column that fills its viewport and reports its own rect.
const FILLING_COLUMN: &str = r"
View Main() {
    var w: Float = 0.0
    var h: Float = 0.0
    var fires: Int = 0

    Column #[grow: 1] {
        on measure => {
            w = it.w
            h = it.h
            fires = fires + 1
        }
    }
}
";

#[test]
fn an_element_is_told_its_own_resolved_rect() {
    let h = Harness::new(FILLING_COLUMN);
    assert!(
        (h.float("w") - 800.0).abs() < 0.5,
        "expected the viewport width, got {}",
        h.float("w")
    );
    assert!(
        (h.float("h") - 600.0).abs() < 0.5,
        "expected the viewport height, got {}",
        h.float("h")
    );
}

#[test]
fn the_measured_size_is_the_rect_the_frame_paints() {
    // RFC-0038 asks for the space paint and hit-testing use rather than a
    // logical value every consumer re-converts. In this engine that space is
    // the frame's own: primitives and hit rects are emitted in it, and the DPI
    // scale is applied once, at the encoder boundary, to all of them together.
    // So the assertion is the one that has content: the number handed to the
    // element is the number the frame carries for it, in whatever unit the
    // frame is in.
    let source = r"
View Main() {
    var w: Float = 0.0
    var h: Float = 0.0

    Column #[bg: 0x223344, width: 320, height: 180] {
        on measure => {
            w = it.w
            h = it.h
        }
    }
}
";
    let h = Harness::new(source);
    let painted = h
        .frame
        .instances()
        .iter()
        .find(|b| (b.rect[2] - 320.0).abs() < 0.5)
        .expect("the column was painted");
    assert!((h.float("w") - f64::from(painted.rect[2])).abs() < f64::EPSILON);
    assert!((h.float("h") - f64::from(painted.rect[3])).abs() < f64::EPSILON);
}

#[test]
fn an_unchanged_rect_fires_nothing() {
    // The incremental-path assertion (INV-18/INV-29): if the fire step ever
    // stops consulting the last delivered size, a static screen starts writing
    // a `var` every frame and this fails.
    let mut h = Harness::new(FILLING_COLUMN);
    assert_eq!(h.int("fires"), 1, "the first layout is a change");
    for _ in 0..8 {
        h.render();
    }
    assert_eq!(h.int("fires"), 1, "a static layout fires exactly once");
}

#[test]
fn a_resize_fires_again_and_a_sibling_reflow_does_too() {
    let mut h = Harness::new(FILLING_COLUMN);
    h.resize(640.0, 480.0);
    assert_eq!(h.int("fires"), 2);
    assert!((h.float("w") - 640.0).abs() < 0.5);

    // A sibling that grows pushes this element's rect, which is a change the
    // element itself did nothing to cause.
    let source = r"
View Main() {
    var pad: Int = 0
    var w: Float = 0.0
    var fires: Int = 0

    Row #[width: 400] {
        Box #[width: pad, height: 10]
        Box #[grow: 1, height: 10] {
            on measure => {
                w = it.w
                fires = fires + 1
            }
        }
    }
}
";
    let mut h = Harness::new(source);
    assert_eq!(h.int("fires"), 1);
    assert!((h.float("w") - 400.0).abs() < 0.5);
    h.set("pad", Value::Int(120));
    assert_eq!(h.int("fires"), 2);
    assert!((h.float("w") - 280.0).abs() < 0.5);
}

#[test]
fn a_child_sizes_itself_to_its_measured_parent() {
    // The pattern the RFC exists for: a widget that maps something onto its own
    // pixel extent, inside a parent whose size is only known after layout.
    //
    // The parent is deliberately *narrower* than the viewport and the child is
    // sized to half of it, so a child that ignored the measurement and fell
    // back to filling its parent would produce a different number. A test where
    // "fills the parent" and "used the measured value" agree proves nothing.
    let source = r"
View Main() {
    var w: Float = 0.0

    Column #[width: 480, height: 120] {
        on measure => w = it.w

        Box #[width: w / 2.0, height: 20, bg: 0x88AACC]
    }
}
";
    let mut h = Harness::new(source);
    // Frame 1 measures; the write is an ordinary reactive write, so frame 2 is
    // the one that draws the child at the measured width.
    h.render();
    let child = h
        .frame
        .instances()
        .iter()
        .find(|b| (b.rect[3] - 20.0).abs() < 0.5)
        .expect("the child was painted");
    assert!(
        (child.rect[2] - 240.0).abs() < 0.5,
        "the child should be half its measured parent, got {}",
        child.rect[2]
    );
}

#[test]
fn a_fractional_extent_is_a_size_and_not_a_fallback() {
    // A measured rect is fractional, and layout is `f32` throughout, so a
    // `Float` width has to mean what it says. Read as an integer it resolved to
    // nothing, and the element silently took its default size, which is exactly
    // the silent failure INV-4 forbids, and would have made every consumer of a
    // measured size wrong in a way that still looked plausible on screen.
    let source = r"
View Main() {
    Column {
        Box #[width: 300.5, height: 20.25, bg: 0x5B8DEF]
    }
}
";
    //
    // Layout snaps the resolved rect to whole pixels, so the assertion is that
    // the extent came from the number written, within that snap, rather than
    // from the fallback (which is the parent's width and `0` respectively).
    let h = Harness::new(source);
    let painted = &h.frame.instances()[0];
    assert!((painted.rect[2] - 300.5).abs() <= 0.5, "{painted:?}");
    assert!((painted.rect[3] - 20.25).abs() <= 0.5, "{painted:?}");
}

#[test]
fn an_element_without_on_measure_allocates_no_slot() {
    let source = r#"
View Main() {
    Column #[grow: 1] {
        Text("nothing to measure here")
    }
}
"#;
    let h = Harness::new(source);
    assert_eq!(h.interp.measure_slots(), 0);
}

#[test]
fn every_row_of_a_for_is_measured_on_its_own() {
    // A pooled body is lowered once per slot, so each row owns its own
    // declaration and its own last-size, exactly as an animation does.
    let source = r"
View Main() {
    var widths = [100, 260]
    var last: Float = 0.0
    var fires: Int = 0

    Column {
        for wd in widths {
            Box #[width: wd, height: 8] {
                on measure => {
                    last = it.w
                    fires = fires + 1
                }
            }
        }
    }
}
";
    let mut h = Harness::new(source);
    assert_eq!(h.interp.measure_slots(), 2, "one slot per pooled row");
    assert_eq!(h.int("fires"), 2, "both rows reported");
    for _ in 0..4 {
        h.render();
    }
    assert_eq!(h.int("fires"), 2, "and neither repeats itself");
}

#[test]
fn a_size_fed_back_into_its_own_layout_is_a_compile_error() {
    let source = r"
View Main() {
    var size = { w: 0.0, h: 0.0 }

    Column #[width: size.w, height: 100] {
        on measure => size = it
    }
}
";
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let _ = interp.lower_view(&parsed.views[0], &known);
    let found = interp.errors().iter().any(|e| {
        matches!(
            e,
            byard_compiler::diagnostics::CompileError::MeasureFeedback { prop, binding, .. }
                if prop == "width" && binding == "size"
        )
    });
    assert!(
        found,
        "expected a feedback diagnostic: {:?}",
        interp.errors()
    );
}

#[test]
fn two_measure_declarations_on_one_element_are_refused() {
    let source = r"
View Main() {
    var a: Float = 0.0
    var b: Float = 0.0

    Column #[width: 100, height: 100] {
        on measure => a = it.w
        on measure => b = it.h
    }
}
";
    let parsed = parse(source);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let _ = interp.lower_view(&parsed.views[0], &known);
    assert!(
        interp.errors().iter().any(|e| matches!(
            e,
            byard_compiler::diagnostics::CompileError::DuplicateMeasure { .. }
        )),
        "{:?}",
        interp.errors()
    );
}

#[test]
fn a_runtime_feedback_loop_is_clamped_to_one_fire_a_frame_and_named() {
    // The half the compiler cannot see: the measured size reaches this
    // element's own width through a `let`, so nothing in the element's own
    // attribute list mentions the written binding. It alternates, which is what
    // a cycle does; what it must not do is fire twice in a frame or run away.
    let source = r"
View Main() {
    var w: Float = 0.0
    var fires: Int = 0
    let chosen = w > 150.0 ? 100 : 200

    Column #[width: chosen, height: 40] {
        on measure => {
            w = it.w
            fires = fires + 1
        }
    }
}
";
    let mut h = Harness::new(source);
    let mut warned = false;
    for frame in 1..=20 {
        h.render();
        assert!(
            h.int("fires") <= frame + 1,
            "an oscillating measure fired more than once in a frame"
        );
        warned |= h
            .interp
            .perf_warnings()
            .iter()
            .any(|w| matches!(w, PerfWarning::MeasureFeedback { .. }));
    }
    assert!(
        warned,
        "an alternating rect should be reported once it is unmistakable"
    );
}
