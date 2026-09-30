//! Guards the committed RFC-0016 responsive example: it checks clean, and at
//! a phone, a tablet and a desktop width it lays out one, two and three
//! columns of cards.
//!
//! The breakpoints are read from the example's own `byard.toml` rather than
//! written again here, so the test cannot pass against widths the example does
//! not declare.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/responsive")
}

#[test]
fn responsive_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
    assert!(stdout.contains("0 errors"), "{stdout}");
}

/// The example's theme: `byard-base` plus the breakpoints its manifest
/// declares.
fn theme() -> Theme {
    let manifest: toml::Table = std::fs::read_to_string(example_dir().join("byard.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let mut theme = Theme::byard_base();
    let bps = manifest["theme"]["breakpoints"]
        .as_table()
        .expect("declared");
    for (name, px) in bps {
        #[allow(clippy::cast_precision_loss)]
        let px = px.as_integer().map(|n| n as f32).expect("a width");
        theme.set_breakpoint(name, px);
    }
    theme
}

/// How many distinct columns the example's cards occupy at `width`.
fn columns_at(width: f32) -> usize {
    let src = std::fs::read_to_string(example_dir().join("src/main.byd")).unwrap();
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(theme());
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, width, 900.0);
    // The cards are the 110px-tall boxes; their distinct left edges are the
    // columns.
    let mut xs: Vec<i32> = frame
        .instances()
        .iter()
        .map(|b| b.rect)
        .chain(frame.decorated().iter().map(|d| d.base.rect))
        .filter(|r| (r[3] - 110.0).abs() < 0.5)
        .map(|r| {
            #[allow(clippy::cast_possible_truncation)]
            let x = r[0].round() as i32;
            x
        })
        .collect();
    xs.sort_unstable();
    xs.dedup();
    xs.len()
}

#[test]
fn the_card_grid_has_one_two_and_three_columns_at_three_widths() {
    assert_eq!(columns_at(400.0), 1, "a phone width: one column");
    assert_eq!(columns_at(800.0), 2, "a tablet width: two columns");
    assert_eq!(columns_at(1200.0), 3, "a desktop width: three columns");
}
