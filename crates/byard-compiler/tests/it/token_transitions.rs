//! A scheme flip cross-fades the colour tokens (RFC-0016 animated token
//! transitions), read off the frame at points through the flip.
//!
//! What is asserted is continuity and arrival, never a particular shade: the
//! first frame after a flip is still the old colour (no flash), the middle is
//! strictly between the two, the end is exactly the new one, and a reversal
//! half way picks up from where the picture was.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

const SRC: &str = "View Main() {
    inject Theme as t
    Column { Box #[bg: t.primary, width: 60, height: 60] {} }
}";

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
}

impl App {
    fn new(transition_ms: u32) -> Self {
        let mut theme = Theme::byard_base();
        theme.transition_ms = transition_ms;
        let parsed = parse(SRC);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.set_theme(theme);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut app = Self { interp, tree };
        let _ = app.colour_at(0);
        app
    }

    /// The box's colour on the frame rendered at `ms`.
    fn colour_at(&mut self, ms: u32) -> [f32; 3] {
        self.interp.set_now_ms(ms);
        self.interp.tick();
        let mut f = RenderFrame::new();
        self.interp.render(&self.tree, &mut f, 400.0, 300.0);
        let c = f
            .instances()
            .iter()
            .map(|b| (b.rect, b.color))
            .chain(f.decorated().iter().map(|d| (d.base.rect, d.base.color)))
            .find(|(r, _)| (r[2] - 60.0).abs() < 0.5)
            .map(|(_, c)| c)
            .expect("the box is emitted");
        [c[0], c[1], c[2]]
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
}

/// The two scheme colours, measured through the same frame path.
fn endpoints() -> ([f32; 3], [f32; 3]) {
    let mut light = App::new(0);
    let l = light.colour_at(10);
    light.interp.set_theme_dark(true);
    let d = light.colour_at(20);
    assert!(dist(l, d) > 0.05, "the test needs two different schemes");
    (l, d)
}

#[test]
fn a_flip_crossfades_from_the_old_colour_to_the_new_one() {
    let (light, dark) = endpoints();
    let mut app = App::new(300);
    assert!(dist(app.colour_at(1000), light) < 1e-3);

    app.interp.set_theme_dark(true);
    let first = app.colour_at(1000);
    assert!(
        dist(first, light) < 0.02,
        "the frame the flip lands on is still the old colour, no flash: {first:?}"
    );
    let a = app.colour_at(1075);
    let b = app.colour_at(1150);
    let c = app.colour_at(1225);
    for (name, m) in [("a", a), ("b", b), ("c", c)] {
        assert!(
            dist(m, light) > 1e-3 && dist(m, dark) > 1e-3,
            "{name} must be strictly between the schemes: {m:?}"
        );
    }
    assert!(
        dist(a, dark) > dist(b, dark) && dist(b, dark) > dist(c, dark),
        "and heading towards the new one the whole way"
    );
    assert!(
        app.interp.has_active_animations(),
        "a flip in flight keeps frames coming"
    );
    let done = app.colour_at(1400);
    assert!(
        dist(done, dark) < 1e-6,
        "at the end it is exactly the new scheme's colour: {done:?} vs {dark:?}"
    );
    assert!(
        !app.interp.has_active_animations(),
        "and the runner may idle again"
    );
}

/// A second flip half way through continues from the picture, rather than
/// jumping to either end.
#[test]
fn a_reversal_half_way_picks_up_where_the_picture_was() {
    let mut app = App::new(300);
    let _ = app.colour_at(1000);
    app.interp.set_theme_dark(true);
    let _ = app.colour_at(1000);
    let before = app.colour_at(1150);
    app.interp.set_theme_dark(false);
    let after = app.colour_at(1150);
    assert!(
        dist(before, after) < 0.02,
        "reversing must not jump: {before:?} → {after:?}"
    );
}

/// With no transition configured, a flip is the cut it always was.
#[test]
fn a_theme_without_a_transition_cuts() {
    let (_, dark) = endpoints();
    let mut app = App::new(0);
    let _ = app.colour_at(1000);
    app.interp.set_theme_dark(true);
    let now = app.colour_at(1000);
    assert!(
        dist(now, dark) < 1e-6,
        "no transition: straight to the new colour"
    );
    assert!(!app.interp.has_active_animations());
}

/// An interpreter with no theme at all must still be able to idle. The mix's
/// fields default to zero, which reads as "part way through a flip" unless the
/// transition is asked whether it exists first.
#[test]
fn an_app_with_no_theme_is_not_kept_awake() {
    let parsed = parse("View Main() { Column { Box #[bg: 0x123456, width: 10, height: 10] {} } }");
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.set_now_ms(10);
    interp.tick();
    let mut f = RenderFrame::new();
    interp.render(&tree, &mut f, 100.0, 100.0);
    assert!(!interp.has_active_animations());
}

/// The frames between a flip's first and last take the retained layout path:
/// a colour token is paint, so a cross-fade must never rebuild layout.
#[test]
fn the_frames_of_a_crossfade_do_not_rebuild_layout() {
    use byard_core::atlas::layout::path_counters;
    let mut app = App::new(300);
    let _ = app.colour_at(1000);
    app.interp.set_theme_dark(true);
    let _ = app.colour_at(1000); // the flip frame rebuilds, by design
    path_counters::reset();
    let _ = app.colour_at(1100);
    let _ = app.colour_at(1200);
    let counts = path_counters::snapshot();
    assert_eq!(
        counts.full_computes, 0,
        "mid-crossfade frames must not rebuild layout: {counts:?}"
    );
}
