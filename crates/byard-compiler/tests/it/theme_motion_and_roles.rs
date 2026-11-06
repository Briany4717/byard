//! RFC-0053: the rest of Material's colour roles in `byard-base`, the modal
//! scrim following the theme, and motion tokens in `with`.

use byard_compiler::CompileError;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::interp::seed::{self, TEXT_PAIRS};
use byard_compiler::interp::theme::{Theme, spring_damping};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;

const NEW_ROLES: &[&str] = &[
    "surfaceContainerLowest",
    "surfaceContainerHighest",
    "surfaceDim",
    "surfaceBright",
    "inverseSurface",
    "inverseOnSurface",
    "inversePrimary",
    "scrim",
    "shadow",
    "primaryFixed",
    "primaryFixedDim",
    "onPrimaryFixed",
    "onPrimaryFixedVariant",
    "secondaryFixed",
    "secondaryFixedDim",
    "onSecondaryFixed",
    "onSecondaryFixedVariant",
    "tertiaryFixed",
    "tertiaryFixedDim",
    "onTertiaryFixed",
    "onTertiaryFixedVariant",
    // The roles the base lacked that the pairs and the fixed families need.
    "tertiary",
    "onTertiary",
    "tertiaryContainer",
    "onTertiaryContainer",
    "surfaceVariant",
    "errorContainer",
    "onErrorContainer",
];

fn role(table: &[(&str, i64)], name: &str) -> Option<i64> {
    table.iter().find(|(r, _)| *r == name).map(|(_, c)| *c)
}

#[test]
fn every_new_role_is_in_the_base_in_both_schemes() {
    let t = Theme::byard_base();
    for name in NEW_ROLES {
        for dark in [false, true] {
            assert!(
                t.color(name, dark).is_some(),
                "byard-base has no `{name}` in the {} scheme",
                if dark { "dark" } else { "light" }
            );
        }
    }
}

#[test]
fn every_new_role_is_derived_from_a_seed_in_both_schemes() {
    for seed_hex in [0x67_50A4, 0x1E_8E3E, 0x80_8080] {
        let (light, dark) = seed::derive(seed_hex);
        for name in NEW_ROLES {
            assert!(role(&light, name).is_some(), "{seed_hex:06X} light {name}");
            assert!(role(&dark, name).is_some(), "{seed_hex:06X} dark {name}");
        }
        // Black, whatever the seed.
        for name in ["scrim", "shadow"] {
            assert_eq!(role(&light, name), Some(0));
            assert_eq!(role(&dark, name), Some(0));
        }
        // Fixed means the same in both schemes.
        for p in ["primary", "secondary", "tertiary"] {
            for suffix in ["Fixed", "FixedDim"] {
                let n = format!("{p}{suffix}");
                assert_eq!(role(&light, &n), role(&dark, &n), "{n}");
            }
        }
        // The surfaces run from the lowest container to the highest.
        let lum = |t: &[(&str, i64)], n: &str| seed::luminance(role(t, n).unwrap());
        assert!(lum(&light, "surfaceContainerLowest") > lum(&light, "surfaceContainerHighest"));
        assert!(lum(&dark, "surfaceContainerLowest") < lum(&dark, "surfaceContainerHighest"));
    }
}

/// A seed recolours the extended roles too, the gap the Material spike found:
/// the filled field's container followed the package, not the seed.
#[test]
fn a_seed_moves_the_new_roles() {
    let mut a = Theme::byard_base();
    a.apply_seed(0x1E_8E3E);
    let mut b = Theme::byard_base();
    b.apply_seed(0x0B_57D0);
    for name in ["surfaceContainerHighest", "primaryFixed", "inversePrimary"] {
        assert_ne!(a.color(name, false), b.color(name, false), "{name}");
    }
}

#[test]
fn the_base_theme_meets_the_contrast_pairs_the_seed_is_held_to() {
    let t = Theme::byard_base();
    for dark in [false, true] {
        for (bg, fg) in TEXT_PAIRS {
            let ratio = seed::contrast(t.color(bg, dark).unwrap(), t.color(fg, dark).unwrap());
            assert!(
                ratio >= 4.5,
                "byard-base {}: {fg} on {bg} is {ratio:.2}:1",
                if dark { "dark" } else { "light" }
            );
        }
    }
}

#[test]
fn the_expressive_shapes_and_emphasized_type_are_in_the_base() {
    let t = Theme::byard_base();
    assert_eq!(t.shape("cornerLgIncreased"), Some(20.0));
    assert_eq!(t.shape("cornerXlIncreased"), Some(32.0));
    assert_eq!(t.shape("cornerXxl"), Some(48.0));
    for style in [
        "displayLarge",
        "headlineSmall",
        "titleLarge",
        "titleMedium",
        "bodyLarge",
        "bodySmall",
        "labelLarge",
        "labelSmall",
    ] {
        let plain = t.typo(style).unwrap();
        let strong = t.typo(&format!("{style}Emphasized")).unwrap();
        assert_eq!(plain.size.to_bits(), strong.size.to_bits(), "{style} size");
        assert_eq!(plain.tracking.to_bits(), strong.tracking.to_bits());
        assert!(
            strong.weight.axis() > plain.weight.axis(),
            "{style}Emphasized is heavier"
        );
    }
    // A short alias has no emphasized twin.
    assert!(t.typo("bodyEmphasized").is_none());
}

// ── the scrim follows the theme ──────────────────────────────────────

const W: f32 = 600.0;
const H: f32 = 300.0;

const MODAL: &str = r#"
View Main() {
    var open = true
    Column #[grow: 1, bg: 0xFFFFFF] {
        Text("page")
        when open { Overlay {
            Column #[anchor: center, bg: 0x333344, p: 20, width: 300] { Text("A dialog") } } }
    }
}
"#;

fn scrim_of(theme: Theme, dark: bool) -> [f32; 4] {
    let parsed = parse(MODAL);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(theme);
    interp.set_theme_dark(dark);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, W, H);
    frame
        .instances()
        .iter()
        .find(|b| b.rect == [0.0, 0.0, W, H] && b.color[3] > 0.0 && b.color[3] < 1.0)
        .expect("a translucent box covers the viewport")
        .color
}

#[test]
fn the_modal_scrim_is_the_themes_scrim_at_32_percent() {
    let black = scrim_of(Theme::byard_base(), false);
    assert!(black[..3].iter().all(|c| *c < 0.01), "{black:?}");
    assert!((black[3] - 0.32).abs() < 0.01);

    let mut tinted = Theme::byard_base();
    tinted.set_color("light", "scrim", 0x00FF_0000);
    tinted.set_color("dark", "scrim", 0x0000_00FF);
    let red = scrim_of(tinted.clone(), false);
    assert!(red[0] > 0.9 && red[1] < 0.1 && red[2] < 0.1, "{red:?}");
    assert!((red[3] - 0.32).abs() < 0.01, "{red:?}");
    let blue = scrim_of(tinted, true);
    assert!(
        blue[2] > 0.9 && blue[0] < 0.1,
        "the dark scheme's scrim: {blue:?}"
    );
}

// ── motion tokens ────────────────────────────────────────────────────

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
}

impl App {
    fn new(src: &str, theme: Theme) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.set_theme(theme);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &known);
        Self { interp, tree }
    }

    fn errors(&self) -> &[CompileError] {
        self.interp.errors()
    }

    fn scale_at(&mut self, ms: u32) -> f32 {
        self.interp.set_now_ms(ms);
        self.interp.tick();
        let mut frame = RenderFrame::new();
        self.interp.render(&self.tree, &mut frame, 400.0, 300.0);
        frame.instances()[0].transform.scale[0]
    }

    /// Renders at rest, flips `on` at t = 0 and samples the scale at `times`.
    fn samples(&mut self, times: &[u32]) -> Vec<f32> {
        let _ = self.scale_at(0);
        let sig = self.interp.var_signal(&Symbol::intern("on")).unwrap();
        self.interp.write_var(sig, Value::Bool(true));
        self.interp.tick();
        times.iter().map(|t| self.scale_at(*t)).collect()
    }
}

fn view_with(curve: &str) -> String {
    format!(
        "View V() {{ inject Theme as t\n var on: Bool = false\n \
         Box #[bg: 0x808080, width: 10, height: 10, scale: on ? 2.0 : 1.0 with {curve}] }}"
    )
}

const TIMES: [u32; 8] = [0, 16, 40, 80, 120, 200, 400, 900];

#[test]
fn a_spring_token_animates_as_the_literal_spring_does() {
    // 800 and a ratio of 0.6 is Material's spatial-fast spring; the damping
    // coefficient the engine takes is 2 * 0.6 * sqrt(800).
    let c = spring_damping(800.0, 0.6);
    let mut token = App::new(&view_with("t.springSpatialFast"), Theme::byard_base());
    assert!(token.errors().is_empty(), "{:?}", token.errors());
    let mut literal = App::new(
        &view_with(&format!("anim.spring(stiffness: 800, damping: {c})")),
        Theme::byard_base(),
    );
    assert!(literal.errors().is_empty(), "{:?}", literal.errors());
    let (a, b) = (token.samples(&TIMES), literal.samples(&TIMES));
    assert_eq!(a, b, "a token is the curve its literal is");
    // And it is a spring: it overshoots 2.0 on the way (ratio 0.6) and settles.
    assert!(a.iter().any(|v| *v > 2.0), "{a:?}");
    assert!((a[7] - 2.0).abs() < 1e-3, "{a:?}");
}

#[test]
fn every_base_spring_matches_its_literal() {
    for (name, k, ratio) in [
        ("springSpatialFast", 800.0_f32, 0.6_f32),
        ("springSpatialDefault", 380.0, 0.8),
        ("springSpatialSlow", 200.0, 0.8),
        ("springEffectsFast", 3800.0, 1.0),
        ("springEffectsDefault", 1600.0, 1.0),
        ("springEffectsSlow", 800.0, 1.0),
    ] {
        let c = spring_damping(k, ratio);
        let mut token = App::new(&view_with(&format!("t.{name}")), Theme::byard_base());
        let mut literal = App::new(
            &view_with(&format!("anim.spring(stiffness: {k}, damping: {c})")),
            Theme::byard_base(),
        );
        assert!(token.errors().is_empty(), "{name}: {:?}", token.errors());
        assert_eq!(token.samples(&TIMES), literal.samples(&TIMES), "{name}");
    }
}

#[test]
fn a_duration_token_is_a_duration_literal() {
    let mut token = App::new(
        &view_with("anim.linear(t.durationMedium2)"),
        Theme::byard_base(),
    );
    assert!(token.errors().is_empty(), "{:?}", token.errors());
    let mut literal = App::new(&view_with("anim.linear(300)"), Theme::byard_base());
    assert_eq!(token.samples(&TIMES), literal.samples(&TIMES));
    // The `duration:` modifier takes one too.
    let mut modifier = App::new(
        &view_with("anim.ease(duration: t.durationLong1)"),
        Theme::byard_base(),
    );
    let mut lit = App::new(&view_with("anim.ease(450)"), Theme::byard_base());
    assert!(modifier.errors().is_empty(), "{:?}", modifier.errors());
    assert_eq!(modifier.samples(&TIMES), lit.samples(&TIMES));
}

#[test]
fn a_theme_can_declare_its_own_spring_and_retune_the_base() {
    let mut theme = Theme::byard_base();
    theme.set_spring("bouncy", 300.0, 12.0);
    theme.set_spring_ratio("spatial_fast", 100.0, 1.0);
    let mut own = App::new(&view_with("t.springBouncy"), theme.clone());
    assert!(own.errors().is_empty(), "{:?}", own.errors());
    let mut lit = App::new(
        &view_with("anim.spring(stiffness: 300, damping: 12)"),
        Theme::byard_base(),
    );
    assert_eq!(own.samples(&TIMES), lit.samples(&TIMES));
    // Redeclaring a base spring changes what every use of it does.
    let mut retuned = App::new(&view_with("t.springSpatialFast"), theme);
    let mut slow = App::new(
        &view_with("anim.spring(stiffness: 100, damping: 20)"),
        Theme::byard_base(),
    );
    assert_eq!(retuned.samples(&TIMES), slow.samples(&TIMES));
}

#[test]
fn a_token_works_in_a_state_block_and_a_stagger() {
    let src = "View V() { inject Theme as t\n \
               for i in [0, 1] { Box #[width: 10, height: 10, \
               scale: 1.0 with anim.stagger(t.springEffectsFast, 50ms, i)] } }";
    let app = App::new(src, Theme::byard_base());
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    // In a state block, where a `with` is validated like any other.
    let src = "View V() { inject Theme as t\n \
               let card = style { bg: 0x336699, on hover { scale: 1.1 with t.springSpatialFast } }\n \
               Box #[width: 40, height: 40, ..card] }";
    let app = App::new(src, Theme::byard_base());
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let bad = src.replace("springSpatialFast", "springSpatialFat");
    let app = App::new(&bad, Theme::byard_base());
    assert!(
        app.errors()
            .iter()
            .any(|e| matches!(e, CompileError::UnknownThemeToken { .. })),
        "{:?}",
        app.errors()
    );
}

// ── the errors ───────────────────────────────────────────────────────

fn errors_of(curve: &str) -> Vec<CompileError> {
    App::new(&view_with(curve), Theme::byard_base())
        .errors()
        .to_vec()
}

#[test]
fn a_misspelt_motion_token_is_an_unknown_theme_token() {
    let errs = errors_of("t.springSpatialFat");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            CompileError::UnknownThemeToken { field, .. } if field == "springSpatialFat"
        )),
        "{errs:?}"
    );
}

#[test]
fn a_token_of_the_wrong_kind_says_what_it_is() {
    let message = |curve: &str| -> String {
        errors_of(curve)
            .into_iter()
            .find_map(|e| match e {
                CompileError::InvalidAnimation { message, .. } => Some(message),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no InvalidAnimation for {curve}"))
    };
    assert!(message("t.primary").contains("a colour"));
    assert!(message("t.cornerLg").contains("a shape token"));
    assert!(message("t.titleLarge").contains("a typography token"));
    assert!(message("t.durationShort2").contains("anim.ease(t.durationShort2)"));
    assert!(message("anim.ease(t.springSpatialFast)").contains("a spring, not a duration"));
}

#[test]
fn a_spring_token_outside_with_is_an_error_and_a_duration_reads_as_ms() {
    let src = "View V() { inject Theme as t\n Box #[bg: 0x336699, width: t.springSpatialFast] }";
    let app = App::new(src, Theme::byard_base());
    let mut interp = app.interp;
    let mut frame = RenderFrame::new();
    interp.tick();
    interp.render(&app.tree, &mut frame, 400.0, 300.0);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::InvalidAnimation { message, .. } if message.contains("a spring"))),
        "{:?}",
        interp.errors()
    );
    // A duration is a number of milliseconds, wherever a number goes.
    let src = "View V() { inject Theme as t\n Box #[bg: 0x336699, width: t.durationMedium2, height: 10] }";
    let app = App::new(src, Theme::byard_base());
    let mut interp = app.interp;
    let mut frame = RenderFrame::new();
    interp.tick();
    interp.render(&app.tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert!((frame.instances()[0].rect[2] - 300.0).abs() < 0.5);
}
