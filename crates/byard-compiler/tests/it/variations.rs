//! `variations:` on text (RFC-0054): the axes written in the source reach the
//! line the renderer draws, sorted, animated like any paint value, and never
//! fight `weight:`.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::interp::theme::{DeclaredFont, Theme};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use std::sync::Arc;

fn icon_theme() -> Theme {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../byard-cli/examples/assets/fonts/ByardIcons.ttf");
    let bytes: Arc<[u8]> = Arc::from(std::fs::read(path).expect("the fixture font is in the tree"));
    let resolved = byard_core::text::family_name(&bytes).expect("it parses");
    let mut theme = Theme::byard_base();
    theme.add_font(
        "symbols",
        DeclaredFont {
            path: "ByardIcons.ttf".to_string(),
            resolved: Arc::from(resolved),
            bytes,
            icons: true,
            icons_include: Vec::new(),
        },
    );
    theme
}

struct Rig {
    interp: Interpreter,
    tree: Vec<RenderNode>,
}

fn rig(body: &str) -> (Rig, Vec<String>) {
    let src = format!("View Main() {{ var on = false\n Column #[width: 400] {{ {body} }} }}");
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(icon_theme());
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    let errors = interp.errors().iter().map(|e| format!("{e:?}")).collect();
    interp.tick();
    (Rig { interp, tree }, errors)
}

impl Rig {
    fn frame(&mut self, now: u32) -> RenderFrame {
        self.interp.set_now_ms(now);
        self.interp.tick();
        let mut f = RenderFrame::new();
        self.interp.render(&self.tree, &mut f, 400.0, 300.0);
        f
    }
    fn flip(&mut self) {
        let sig = self.interp.var_signal(&Symbol::intern("on")).expect("var");
        let now = self.interp.peek(sig).as_bool().unwrap_or(false);
        self.interp.write_var(sig, Value::Bool(!now));
    }
}

fn axes(f: &RenderFrame, line: usize) -> Vec<(String, f32)> {
    f.texts()[line]
        .variations
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|(t, v)| (String::from_utf8_lossy(t).into_owned(), *v))
        .collect()
}

#[test]
fn axes_reach_the_line_sorted_by_tag() {
    let (mut r, errors) = rig(
        r#"Text("favorite") #[font: symbols, size: 24, variations: (wght: 500, FILL: 1, GRAD: 0, opsz: 24)]
           Text("home") #[font: symbols]"#,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let f = r.frame(0);
    assert_eq!(
        axes(&f, 0),
        vec![
            ("FILL".to_string(), 1.0),
            ("GRAD".to_string(), 0.0),
            ("opsz".to_string(), 24.0),
            ("wght".to_string(), 500.0)
        ]
    );
    assert!(
        f.texts()[1].variations.is_none(),
        "a line that sets none has none"
    );
}

#[test]
fn a_wght_axis_is_the_weight_the_line_is_measured_at() {
    let (mut r, errors) = rig(r#"Text("favorite") #[font: symbols, variations: (wght: 700)]"#);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(r.frame(0).texts()[0].weight, 700);
}

#[test]
fn weight_and_a_wght_axis_together_are_an_error() {
    let (_, errors) =
        rig(r#"Text("favorite") #[font: symbols, weight: 500, variations: (wght: 700)]"#);
    assert!(
        errors.iter().any(|e| e.contains("VariationWeightConflict")),
        "{errors:?}"
    );
}

#[test]
fn an_axis_that_is_not_a_tag_is_a_check_error() {
    for bad in [
        "variations: (FIL: 1)",
        "variations: (1, 2)",
        "variations: 3",
    ] {
        let (_, errors) = rig(&format!(r#"Text("favorite") #[font: symbols, {bad}]"#));
        assert!(
            errors.iter().any(|e| e.contains("AttributeTypeMismatch")),
            "{bad}: {errors:?}"
        );
    }
}

/// An axis written with `with` moves like any animated value, and the line is
/// not a layout input while it does.
#[test]
fn an_axis_animates_with_a_spring() {
    let (mut r, errors) = rig(
        r#"Text("favorite") #[font: symbols, variations: (FILL: on ? 1 : 0 with anim.spring())]"#,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let first = r.frame(0);
    assert_eq!(axes(&first, 0)[0].1, 0.0);
    r.flip();
    let mut seen = Vec::new();
    for step in 1..=40 {
        let f = r.frame(step * 16);
        seen.push(axes(&f, 0)[0].1);
    }
    assert!(
        seen.iter().any(|v| *v > 0.05 && *v < 0.95),
        "the axis never passed through the middle: {seen:?}"
    );
    assert!(
        (seen.last().copied().unwrap() - 1.0).abs() < 0.01,
        "and settles on 1: {seen:?}"
    );
}

#[test]
fn a_text_field_takes_axes_too() {
    let (mut r, errors) =
        rig(r#"TextField #[font: symbols, placeholder: "x", variations: (FILL: 1)]"#);
    assert!(errors.is_empty(), "{errors:?}");
    let f = r.frame(0);
    assert!(
        f.texts().iter().any(|t| t.variations.is_some()),
        "the field's text carries the axes"
    );
}
