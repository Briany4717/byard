//! The `font:` prop selects a declared family (RFC-0034).
//!
//! Deterministic first, and on purpose. A pixel test can only say that two
//! renderings differ; it can never say *which* family a line was shaped in,
//! and "which" is the entire question a font selector has to answer. These
//! read the resolved family off the frame, so `font: display` means Space
//! Grotesk on every machine, whatever fonts it happens to have installed.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::{DeclaredFont, Theme, TypoToken};
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use std::sync::Arc;

/// The shipped example faces, read from the tree so the suite and the examples
/// prove the same files work.
fn face(file: &str) -> DeclaredFont {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../byard-cli/examples/assets/fonts")
        .join(file);
    let bytes: Arc<[u8]> =
        Arc::from(std::fs::read(&path).expect("the shipped font is in the tree"));
    let resolved = byard_core::text::family_name(&bytes).expect("it parses");
    DeclaredFont {
        path: file.to_string(),
        resolved: Arc::from(resolved),
        bytes,
    }
}

/// A theme declaring both faces and nothing else.
fn two_families() -> Theme {
    let mut theme = Theme::byard_base();
    theme.add_font("display", face("SpaceGrotesk-Variable.ttf"));
    theme.add_font("body", face("Manrope-Variable.ttf"));
    theme
}

/// Renders one frame and returns `(text, resolved family)` for every line.
fn families_of(src: &str, theme: Theme) -> Vec<(String, Option<String>)> {
    let (lines, errors) = render(src, theme);
    assert!(errors.is_empty(), "{errors:?}");
    lines
}

fn render(src: &str, theme: Theme) -> (Vec<(String, Option<String>)>, Vec<String>) {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(theme);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    let errors: Vec<String> = interp.errors().iter().map(|e| format!("{e:?}")).collect();
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let lines = frame
        .texts()
        .iter()
        .map(|t| (t.text.clone(), t.family.as_deref().map(str::to_string)))
        .collect();
    (lines, errors)
}

fn view(body: &str) -> String {
    format!("View Main() {{ Column #[width: 400] {{ {body} }} }}")
}

/// The family read off the frame is the family written in the source.
#[test]
fn an_explicit_font_selects_the_declared_family() {
    let lines = families_of(
        &view(r#"Text("a") #[size: 20, font: display] Text("b") #[size: 20, font: body]"#),
        two_families(),
    );
    assert_eq!(
        lines,
        vec![
            ("a".to_string(), Some("Space Grotesk".to_string())),
            ("b".to_string(), Some("Manrope".to_string())),
        ]
    );
}

/// A quoted family name reads the same as a bare one. Neither spelling is more
/// correct and rejecting one only makes the language harder to guess at.
#[test]
fn a_quoted_family_name_reads_the_same_as_a_bare_one() {
    let lines = families_of(
        &view(r#"Text("a") #[size: 20, font: "display"]"#),
        two_families(),
    );
    assert_eq!(lines[0].1.as_deref(), Some("Space Grotesk"));
}

/// A `typo:` token brings its own family, exactly as it brings its own weight.
/// The token has carried a family since the theme runtime landed and it was
/// dropped on the way through the whole time.
#[test]
fn a_typo_token_brings_its_family() {
    let mut theme = two_families();
    theme.set_typo(
        "hero",
        TypoToken {
            family: Some("display".to_string()),
            ..TypoToken::plain(40.0)
        },
    );
    let lines = families_of(&view(r#"Text("a") #[typo: hero]"#), theme);
    assert_eq!(lines[0].1.as_deref(), Some("Space Grotesk"));
}

/// An explicit `font:` outranks the token's family. The narrower statement
/// wins, which is the same rule `weight:` follows.
#[test]
fn an_explicit_font_outranks_the_token() {
    let mut theme = two_families();
    theme.set_typo(
        "hero",
        TypoToken {
            family: Some("display".to_string()),
            ..TypoToken::plain(40.0)
        },
    );
    let lines = families_of(&view(r#"Text("a") #[typo: hero, font: body]"#), theme);
    assert_eq!(lines[0].1.as_deref(), Some("Manrope"));
}

/// With neither, the theme's body family applies, so a project that declares a
/// UI face gets it everywhere without repeating itself.
#[test]
fn text_with_no_font_falls_back_to_the_themes_body_family() {
    let mut theme = two_families();
    theme.set_typo(
        "body",
        TypoToken {
            family: Some("body".to_string()),
            ..TypoToken::plain(14.0)
        },
    );
    let lines = families_of(&view(r#"Text("a") #[size: 16]"#), theme);
    assert_eq!(lines[0].1.as_deref(), Some("Manrope"));
}

/// And with no body family either, the system font, silently. A project that
/// declares no fonts is not making a mistake.
#[test]
fn a_theme_with_no_families_leaves_text_on_the_system_font() {
    let lines = families_of(&view(r#"Text("a") #[size: 16]"#), Theme::byard_base());
    assert_eq!(lines[0].1, None);
}

/// A family nobody declared is a compile error naming the nearest one. The
/// alternative is text that renders perfectly in the wrong face, which reads
/// as a design decision rather than a typo.
#[test]
fn an_undeclared_family_is_a_diagnostic_with_a_hint() {
    let (_, errors) = render(
        &view(r#"Text("a") #[size: 20, font: dsiplay]"#),
        two_families(),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("UnknownFontFamily"), "{errors:?}");
    assert!(errors[0].contains("dsiplay"), "{errors:?}");
    assert!(
        errors[0].contains("display"),
        "the nearest declared name must be offered: {errors:?}"
    );
}

/// The check runs once, when the view is lowered, not on every frame: a
/// diagnostic re-reported sixty times a second is a diagnostic nobody reads.
#[test]
fn the_undeclared_family_is_reported_once_not_per_frame() {
    let src = view(r#"Text("a") #[size: 20, font: nope]"#);
    let parsed = parse(&src);
    let mut interp = Interpreter::new();
    interp.set_theme(two_families());
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    for _ in 0..5 {
        let mut frame = RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    assert_eq!(interp.errors().len(), 1, "{:?}", interp.errors());
}

/// A `Button`'s type properties reach its label. The button's own box cannot
/// use `size`, `weight` or `font` for anything, so leaving them on it made all
/// three accepted, type-checked and inert.
#[test]
fn a_buttons_type_properties_reach_its_label() {
    let lines = families_of(
        &view(r#"Button("go") #[size: 20, font: display]"#),
        two_families(),
    );
    assert_eq!(lines[0].1.as_deref(), Some("Space Grotesk"));
}

/// So do a `TextField`'s.
#[test]
fn a_text_fields_type_properties_reach_its_text() {
    let src = r#"View Main() {
        var typed = "hello"
        Column #[width: 400] {
            TextField #[bind: typed, size: 20, font: display, width: 200]
        }
    }"#;
    let (lines, errors) = render(src, two_families());
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(lines[0].1.as_deref(), Some("Space Grotesk"));
}

/// A name the view binds is skipped by the checker rather than reported as an
/// unknown family.
///
/// `View Specimen(family: Str)` writing `font: family` means the parameter,
/// and a checker that read it as the family named "family" would make a
/// reusable type-specimen view impossible to write. (Whether a *parameter*
/// then reaches the attribute at render time is a separate, older limitation
/// of view instantiation: an attribute value is evaluated on the render walk,
/// where the view's parameters are no longer in scope. `size: sz` has the same
/// hole. Not this change's to fix, and not this change's to paper over: the
/// example uses literals.)
#[test]
fn a_bound_name_is_not_reported_as_an_unknown_family() {
    let src = r#"View Specimen(family: Str) {
        Text("Handgloves") #[font: family, size: 20]
    }
    View Main() {
        var chosen = "display"
        Column #[width: 400] {
            Text("a") #[font: chosen, size: 20]
        }
    }"#;
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(two_families());
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    // Both views are checked, exactly as `byard check` does it, so the one
    // that binds the name is the one that would report a spurious error.
    for v in &parsed.views {
        let _ = interp.lower_view(v, &known);
    }
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
}

/// A `var` in the same view *does* reach the attribute, so a family chosen at
/// runtime resolves to that family and not to the variable's own name.
#[test]
fn a_family_read_from_a_var_resolves_to_that_family() {
    let src = r#"View Main() {
        var chosen = "body"
        Column #[width: 400] { Text("a") #[font: chosen, size: 20] }
    }"#;
    let lines = families_of(src, two_families());
    assert_eq!(lines[0].1.as_deref(), Some("Manrope"));
}
