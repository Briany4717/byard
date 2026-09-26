//! Font families reach the frame from one source of truth (RFC-0034, INV-27).
//!
//! The engine-side halves of this live in `byard-core`: that the measurement
//! and paint `FontSystem`s agree, and that a face is loaded once. What is left
//! for this crate is the seam it owns, the theme, and the claim that the table
//! the render thread reads is the table the measurer was given, on **every**
//! frame rather than only the one after registration.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::{DeclaredFont, Theme};
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use std::sync::Arc;

/// The shipped example faces. The suite reads the same files the examples do,
/// so a test passing against a font nobody ships is not a thing that can
/// happen here.
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

fn theme_with_two_families() -> Theme {
    let mut theme = Theme::byard_base();
    theme.add_font("display", face("SpaceGrotesk-Variable.ttf"));
    theme.add_font("body", face("Manrope-Variable.ttf"));
    theme
}

/// Renders one frame of `src` under `theme` and returns it.
fn frame_of(src: &str, theme: Theme) -> RenderFrame {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(theme);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame
}

const SRC: &str = r#"View Main() { Column #[width: 400] { Text("hello") #[size: 20] } }"#;

#[test]
fn the_declared_families_reach_the_frame() {
    let frame = frame_of(SRC, theme_with_two_families());
    let names: Vec<&str> = frame
        .fonts()
        .faces()
        .iter()
        .map(|f| f.resolved.as_ref())
        .collect();
    assert!(names.contains(&"Space Grotesk"), "{names:?}");
    assert!(names.contains(&"Manrope"), "{names:?}");
}

/// The table rides *every* frame, not just the first.
///
/// The relay keeps only the newest frame and drops the rest, so a table handed
/// over once is a table a dropped frame loses for good. This is the assertion
/// that fails if anyone turns the delivery into a one-shot pool.
#[test]
fn every_frame_carries_the_table_not_just_the_first() {
    let parsed = parse(SRC);
    let mut interp = Interpreter::new();
    interp.set_theme(theme_with_two_families());
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    for tick in 1..=5 {
        let mut frame = RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        assert_eq!(
            frame.fonts().faces().len(),
            2,
            "frame {tick} carried no font table"
        );
    }
}

/// A project that declares no fonts carries no table and pays for nothing.
#[test]
fn a_project_with_no_declared_fonts_carries_an_empty_table() {
    let frame = frame_of(SRC, Theme::byard_base());
    assert!(frame.fonts().is_empty());
}
