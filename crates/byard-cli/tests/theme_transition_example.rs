//! Guards the committed theme example's cross-fade (RFC-0016): it checks
//! clean, and flipping its scheme fades the background *and* the text
//! together, which the per-element springs it used to carry did not do.
//!
//! The theme is built from the example's own `byard.toml`, colours and
//! `transition` both, so the test measures the file a reader runs.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/theme")
}

#[test]
fn theme_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
}

fn theme() -> Theme {
    let manifest: toml::Table = std::fs::read_to_string(example_dir().join("byard.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let mut theme = Theme::byard_base();
    let t = manifest["theme"].as_table().unwrap();
    for (scheme, tokens) in t["color"].as_table().unwrap() {
        for (token, hex) in tokens.as_table().unwrap() {
            let hex = hex.as_str().unwrap().trim_start_matches('#');
            theme.set_color(scheme, token, i64::from_str_radix(hex, 16).unwrap());
        }
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        theme.transition_ms = t["transition"].as_integer().expect("a transition") as u32;
    }
    theme
}

/// Every text colour and every fill colour on the frame at `ms`.
fn colours_at(
    interp: &mut Interpreter,
    tree: &[byard_compiler::interp::eval::RenderNode],
    ms: u32,
) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
    interp.set_now_ms(ms);
    interp.tick();
    let mut f = RenderFrame::new();
    interp.render(tree, &mut f, 520.0, 400.0);
    let texts = f.texts().iter().map(|t| t.color).collect();
    let fills = f
        .instances()
        .iter()
        .map(|b| b.color)
        .chain(f.decorated().iter().map(|d| d.base.color))
        .collect();
    (texts, fills)
}

#[test]
fn a_flip_fades_text_and_surfaces_together() {
    let src = std::fs::read_to_string(example_dir().join("src/main.byd")).unwrap();
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(theme());
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let (text0, fill0) = colours_at(&mut interp, &tree, 1000);
    interp.set_theme_dark(true);
    let _ = colours_at(&mut interp, &tree, 1000);
    let (text_mid, fill_mid) = colours_at(&mut interp, &tree, 1150);
    let (text_end, fill_end) = colours_at(&mut interp, &tree, 2000);

    // Half way, *both* kinds are between their two ends: the text is not
    // snapping while the surfaces ease, which is what the example used to do.
    let between = |a: [f32; 4], m: [f32; 4], b: [f32; 4]| {
        let d = |x: [f32; 4], y: [f32; 4]| -> f32 {
            x.iter().zip(y).take(3).map(|(p, q)| (p - q).abs()).sum()
        };
        d(a, b) > 0.05 && d(m, a) > 1e-3 && d(m, b) > 1e-3
    };
    assert!(
        text0
            .iter()
            .zip(&text_mid)
            .zip(&text_end)
            .any(|((a, m), b)| between(*a, *m, *b)),
        "some text must be mid-fade half way through: {text0:?} {text_mid:?} {text_end:?}"
    );
    assert!(
        fill0
            .iter()
            .zip(&fill_mid)
            .zip(&fill_end)
            .any(|((a, m), b)| between(*a, *m, *b)),
        "some surface must be mid-fade half way through"
    );
}
