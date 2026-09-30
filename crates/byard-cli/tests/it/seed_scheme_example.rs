//! Guards the committed seed-scheme example (RFC-0022 §5): it checks clean,
//! and every label it draws is readable on the surface under it, in both
//! schemes. "A palette was derived" would pass for one nobody can read; this
//! measures what the example actually paints.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/seed_scheme")
}

#[test]
fn seed_scheme_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
}

/// The theme the example's `byard.toml` describes: its seed, applied to base.
fn theme() -> Theme {
    let manifest: toml::Table = std::fs::read_to_string(example_dir().join("byard.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let seed = manifest["theme"]["seed"].as_str().expect("a seed");
    let mut theme = Theme::byard_base();
    theme.apply_seed(i64::from_str_radix(seed.trim_start_matches('#'), 16).unwrap());
    theme
}

/// WCAG relative luminance of a linear colour, which is what the frame holds.
fn luminance(c: [f32; 4]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

#[test]
fn every_label_is_readable_on_what_it_sits_on_in_both_schemes() {
    let src = std::fs::read_to_string(example_dir().join("src/main.byd")).unwrap();
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(theme());
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    for dark in [false, true] {
        interp.set_theme_dark(dark);
        // Past the 300 ms cross-fade, so the colours are the scheme's own.
        interp.set_now_ms(if dark { 10_000 } else { 0 });
        interp.tick();
        let mut f = RenderFrame::new();
        interp.render(&tree, &mut f, 560.0, 480.0);

        let fills: Vec<([f32; 4], [f32; 4])> = f
            .instances()
            .iter()
            .map(|b| (b.rect, b.color))
            .chain(f.decorated().iter().map(|d| (d.base.rect, d.base.color)))
            .collect();
        assert!(!f.texts().is_empty(), "the example draws labels");
        for line in f.texts() {
            let (px, py) = (line.x + 1.0, line.y + 1.0);
            let under = fills
                .iter()
                .filter(|(r, _)| px >= r[0] && py >= r[1] && px <= r[0] + r[2] && py <= r[1] + r[3])
                .min_by(|a, b| (a.0[2] * a.0[3]).total_cmp(&(b.0[2] * b.0[3])))
                .unwrap_or_else(|| panic!("`{}` sits on no fill", line.text));
            let (a, b) = (luminance(line.color), luminance(under.1));
            let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
            assert!(
                ratio >= 4.5,
                "dark={dark}: `{}` is {ratio:.2}:1 on its surface",
                line.text
            );
        }
    }
}
