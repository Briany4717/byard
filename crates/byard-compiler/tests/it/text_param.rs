//! A `Text` that reads a view parameter (`size: dim`, `weight: w`, ...).
//!
//! The run is measured and painted outside the instance it was written in, so
//! it has to carry that instance's scope with it; without one, every attribute
//! that named a parameter resolved to nothing and silently fell back to the
//! default. The tests read the numbers off the frame, so they say what the run
//! was shaped at, with no GPU and no font.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::TextLine;
use byard_core::frame::RenderFrame;

struct Shot {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    frame: RenderFrame,
}

impl Shot {
    fn new(src: &str) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        interp.load_views(&parsed.views);
        let main = parsed.views.iter().find(|v| v.name.as_str() == "Main");
        let tree = interp.lower_view(main.expect("a Main view"), &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut shot = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
        };
        shot.render();
        shot
    }

    fn render(&mut self) {
        self.interp.tick();
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 600.0);
    }

    fn lines(&self) -> Vec<TextLine> {
        self.frame.texts().to_vec()
    }

    fn sizes(&self) -> Vec<f32> {
        self.lines().iter().map(|l| l.font_size).collect()
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

fn two(view: &str, a: &str, b: &str) -> Shot {
    Shot::new(&format!(
        "{view}\nView Main() {{ Column {{ K({a}) K({b}) }} }}"
    ))
}

#[test]
fn a_size_from_a_literal_argument() {
    let s = two(
        "View K(dim: Int) { Text(\"Aa\") #[size: dim] }",
        "dim: 40",
        "dim: 10",
    );
    assert_eq!(s.sizes(), [40.0, 10.0]);
}

#[test]
fn two_call_sites_are_measured_at_their_own_sizes() {
    let s = two(
        "View K(dim: Int) { Text(\"Aa\") #[size: dim] }",
        "dim: 40",
        "dim: 10",
    );
    let lines = s.lines();
    // The second run starts where the first, 40px tall, ends: it was laid out
    // at 40, not at the 17px default the first one used to get.
    assert!(lines[1].y - lines[0].y >= 40.0, "{:?}", s.sizes());
}

#[test]
fn a_size_from_a_default() {
    let s = two(
        "View K(dim: Int = 30) { Text(\"Aa\") #[size: dim] }",
        "",
        "dim: 12",
    );
    assert_eq!(s.sizes(), [30.0, 12.0]);
}

#[test]
fn a_size_from_a_derived_let() {
    let s = two(
        "View K(dim: Int) { let d = dim * 2\n Text(\"Aa\") #[size: d] }",
        "dim: 20",
        "dim: 6",
    );
    assert_eq!(s.sizes(), [40.0, 12.0]);
}

#[test]
fn a_size_from_a_view_var() {
    let s = two(
        "View K(dim: Int) { var s = dim\n Text(\"Aa\") #[size: s] }",
        "dim: 33",
        "dim: 11",
    );
    assert_eq!(s.sizes(), [33.0, 11.0]);
}

#[test]
fn a_size_from_a_ternary() {
    let s = two(
        "View K(dim: Int) { Text(\"Aa\") #[size: dim > 10 ? dim : 10] }",
        "dim: 40",
        "dim: 4",
    );
    assert_eq!(s.sizes(), [40.0, 10.0]);
}

#[test]
fn a_size_from_a_float() {
    let s = two(
        "View K(dim: Float) { Text(\"Aa\") #[size: dim] }",
        "dim: 24.0",
        "dim: 12.5",
    );
    assert_eq!(s.sizes(), [24.0, 12.5]);
}

#[test]
fn the_size_follows_the_parameter_when_it_changes() {
    let mut s = Shot::new(
        "View K(dim: Int) { Text(\"Aa\") #[size: dim] }\n\
         View Main() { var n = 20\n Column { K(dim: n) K(dim: 10) } }",
    );
    assert_eq!(s.sizes(), [20.0, 10.0]);
    let before = s.lines()[1].y;
    s.set("n", Value::Int(50));
    assert_eq!(s.sizes(), [50.0, 10.0]);
    // Remeasured, not only repainted: the run below moved down.
    assert!(
        s.lines()[1].y > before + 25.0,
        "{} -> {}",
        before,
        s.lines()[1].y
    );
    s.set("n", Value::Int(8));
    assert_eq!(s.sizes(), [8.0, 10.0]);
}

#[test]
fn a_weight_from_a_parameter() {
    let s = two(
        "View K(w: Int) { Text(\"Aa\") #[size: 20, weight: w] }",
        "w: 700",
        "w: 250",
    );
    let w: Vec<u16> = s.lines().iter().map(|l| l.weight).collect();
    assert_eq!(w, [700, 250]);
}

#[test]
fn a_weight_keyword_is_still_a_keyword() {
    let s = two(
        "View K(w: Int) { Text(\"Aa\") #[size: 20, weight: bold] }",
        "w: 1",
        "w: 2",
    );
    assert!(s.lines().iter().all(|l| l.weight == 700));
}

#[test]
fn a_color_from_a_parameter() {
    let s = two(
        "View K(c: Int) { Text(\"Aa\") #[size: 20, color: c] }",
        "c: 0xFF0000",
        "c: 0x0000FF",
    );
    let lines = s.lines();
    assert!(
        lines[0].color[0] > 0.9 && lines[0].color[2] < 0.1,
        "{:?}",
        lines[0].color
    );
    assert!(
        lines[1].color[2] > 0.9 && lines[1].color[0] < 0.1,
        "{:?}",
        lines[1].color
    );
}

#[test]
fn wrapping_from_a_parameter() {
    let s = Shot::new(
        "View K(on: Bool) { Text(\"alpha beta gamma delta\") #[size: 20, width: 60, wrap: on] }\n\
         View Main() { Column { K(on: true) K(on: false) } }",
    );
    let lines = s.lines();
    // The wrapping run is tall, so the run after it starts well below.
    assert!(
        lines[1].y - lines[0].y > 60.0,
        "{} {}",
        lines[0].y,
        lines[1].y
    );
}

#[test]
fn a_typo_token_from_a_parameter() {
    let s = two(
        "View K(t: Str) { Text(\"Aa\") #[typo: t] }",
        "t: \"headlineLarge\"",
        "t: \"labelSmall\"",
    );
    let sizes = s.sizes();
    assert!(sizes[0] > sizes[1], "{sizes:?}");
}
