//! Hit testing under an ancestor's transform (RFC-0011 hierarchical transforms).
//!
//! Paint has composed ancestor transforms for a while: a button inside a
//! rotated card is *drawn* rotated with it. Hit testing had not been told, so
//! the same button was pressable at the spot the card would have put it
//! unrotated, which is invisible in any screenshot and obvious the first time
//! anyone tries to press it. These tests come before any pixels for exactly
//! that reason.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent};

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
}

impl App {
    fn new(src: &str) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut app = Self { interp, tree };
        app.render();
        app
    }

    fn render(&mut self) {
        self.interp.tick();
        let mut f = RenderFrame::new();
        self.interp.render(&self.tree, &mut f, 400.0, 400.0);
    }

    fn tap(&mut self, pos: (f32, f32)) {
        let ev = |kind, t| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: t,
        };
        self.interp
            .dispatch_events(&[ev(EventKind::PointerDown, 0), ev(EventKind::PointerUp, 8)]);
        self.render();
    }

    fn count(&self, name: &str) -> i64 {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(name))
            .unwrap_or_else(|| panic!("`{name}` is declared"));
        self.interp.peek(sig).as_int().unwrap_or(-1)
    }
}

/// A 200×100 card at (100, 100), rotated a quarter turn about its centre
/// (200, 150), holding a 60×40 button at its top-left.
///
/// Unrotated, the button's centre is (130, 120). Rotated with the card it is
/// drawn centred on (230, 80).
const ROTATED: &str = r"
View Main() {
    var hits = 0
    Column #[width: 400, height: 400] {
        Box #[m: (left: 100, top: 100), width: 200, height: 100, bg: 0x333333,
              rotate: 90deg, origin: center] {
            Box #[width: 60, height: 40, bg: 0xFF0000] => hits++
        }
    }
}
";

#[test]
fn a_button_in_a_rotated_card_is_pressable_where_it_is_drawn() {
    let mut app = App::new(ROTATED);
    app.tap((230.0, 80.0));
    assert_eq!(
        app.count("hits"),
        1,
        "a press on the button where it is drawn must reach it"
    );
}

/// And *not* where the card would have put it unrotated. Without this half,
/// a hit region that simply grew to cover both would pass the test above.
#[test]
fn a_button_in_a_rotated_card_is_not_pressable_where_it_used_to_be() {
    let mut app = App::new(ROTATED);
    app.tap((130.0, 120.0));
    assert_eq!(
        app.count("hits"),
        0,
        "the unrotated position is empty space on screen and must not press it"
    );
}

/// A scaled container carries its children's targets with it too.
#[test]
fn a_button_in_a_scaled_card_is_pressable_where_it_is_drawn() {
    // A 100×100 card at the origin scaled 2× about its top-left: the 50×50
    // button at its top-left is drawn over (0..100, 0..100), so (80, 80) is on
    // it on screen and outside its unscaled 50×50 rect.
    let src = r"
View Main() {
    var hits = 0
    Column #[width: 400, height: 400] {
        Box #[width: 100, height: 100, scale: 2.0, origin: (0.0, 0.0)] {
            Box #[width: 50, height: 50, bg: 0xFF0000] => hits++
        }
    }
}
";
    let mut app = App::new(src);
    app.tap((80.0, 80.0));
    assert_eq!(app.count("hits"), 1);
}

/// An element's *own* transform still does not move its own hit target.
///
/// Kept deliberately and asserted so it stays kept: a hover-scale that grew
/// the element's own target would push the pointer at its edge in and out of
/// hover as it animated. Only *ancestors'* transforms move a target.
#[test]
fn an_elements_own_transform_still_leaves_its_own_target_alone() {
    let src = r"
View Main() {
    var hits = 0
    Column #[width: 400, height: 400] {
        Box #[width: 100, height: 100, translate: (200, 0), bg: 0xFF0000] => hits++
    }
}
";
    let mut app = App::new(src);
    app.tap((50.0, 50.0));
    assert_eq!(
        app.count("hits"),
        1,
        "the element's own target is its laid-out rect, as it always was"
    );
}

/// With no transforms anywhere, no region carries a frame at all: the fast
/// path is not merely equivalent, it is the only path taken. This is the
/// assertion that fails if identity ever starts being stored as a frame.
#[test]
fn an_untransformed_tree_registers_no_framed_regions() {
    let src = r"
View Main() {
    var hits = 0
    Column #[width: 400, height: 400] {
        Box #[width: 100, height: 100] {
            Box #[width: 50, height: 50, bg: 0xFF0000] => hits++
        }
    }
}
";
    let app = App::new(src);
    assert_eq!(app.interp.router.framed_region_count(), 0);
    let rotated = App::new(ROTATED);
    assert!(
        rotated.interp.router.framed_region_count() > 0,
        "the control: a rotated ancestor must produce a framed region"
    );
}
