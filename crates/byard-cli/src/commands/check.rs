//! `byard check [file]` — parse and validate without opening a window
//! (RFC-0006 §5, decision C7; RFC-0008: checks the *whole module graph*,
//! project siblings and packages included).

use crate::deps::resolve_project;
use crate::manifest::Manifest;
use crate::style;
use byard_compiler::CompileError;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::resolve::{ResolvedProgram, SourceMap};
use std::fmt::Write as _;
use std::path::Path;

pub fn run(file: Option<&Path>, short: bool) -> Result<(), String> {
    let started = std::time::Instant::now();
    let manifest = Manifest::discover(file)?;

    style::action(&format!("checking {}", manifest.entry.display()));

    let (program, _provider) = resolve_project(&manifest)?;
    let n_files = program.source_map.files().count();
    let n_pkgs = program.packages.len().saturating_sub(1);
    if n_files > 1 || n_pkgs > 0 {
        style::info(&format!("{n_files} file(s), {n_pkgs} package(s)"));
    }

    let errors = check_program_with_theme(&program, manifest.theme);

    if errors.is_empty() {
        style::ok("0 errors", Some(started.elapsed()));
        Ok(())
    } else {
        print_diagnostics(&errors, &program.source_map, short);
        let n = errors.len();
        style::err(&format!("{n} error{}", if n == 1 { "" } else { "s" }));
        // Signal failure to the caller (main.rs maps Err → exit 1).
        Err(String::new())
    }
}

/// Headless semantic validation of a resolved program (no `wgpu`/`winit`).
///
/// Resolve/parse errors short-circuit; otherwise every `View` is lowered and
/// rendered into a throwaway [`RenderFrame`] so attribute-contract and
/// `Len`-form validation checks — which run during lowering and render — are
/// exercised across the whole module graph.
#[must_use]
pub fn check_program(program: &ResolvedProgram) -> Vec<CompileError> {
    check_program_with_theme(program, byard_compiler::interp::theme::Theme::byard_base())
}

/// Like [`check_program`], but validates against a specific design-token theme
/// (RFC-0022) so `inject Theme as t` resolves and `t.token` references are
/// checked against the project's *actual* declared tokens — a custom manifest
/// token must not be flagged `UnknownThemeToken`.
#[must_use]
pub fn check_program_with_theme(
    program: &ResolvedProgram,
    theme: byard_compiler::interp::theme::Theme,
) -> Vec<CompileError> {
    if !program.errors.is_empty() {
        return program.errors.clone();
    }

    let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    interp.set_theme(theme);
    // Build the user-`View` registry once for the whole program so user-view
    // calls resolve and expand during lowering (RFC-0007 §1).
    interp.load_views(&program.views);
    let mut frame = byard_core::frame::RenderFrame::new();
    for view in &program.views {
        let tree = interp.lower_view(view, &known);
        interp.tick();
        interp.render(&tree, &mut frame, 1024.0, 768.0);
        frame.clear();
    }
    interp.errors().to_vec()
}

/// Headless parse + semantic validation of one `.byd` source — the
/// single-file path (bare `byard check file.byd`, unit tests). Any `use` in a
/// bare file is an `UnknownPackage` error: dependencies need a manifest.
// Exercised by the unit tests below; the `run` entry point always goes through
// the manifest/module-graph path, so the bin build sees this as unused.
#[allow(dead_code)]
#[must_use]
pub fn check_source(src: &str) -> Vec<CompileError> {
    struct NoPackages;
    impl byard_compiler::resolve::PackageProvider for NoPackages {
        fn package_files(
            &mut self,
            _dependent: &str,
            _package: &str,
        ) -> Result<Vec<byard_compiler::resolve::SourceFile>, String> {
            Err("a bare `.byd` file has no `[dependencies]`; create a byard.toml".to_string())
        }
    }
    let program = byard_compiler::resolve::resolve_program(
        vec![byard_compiler::resolve::SourceFile {
            name: "main.byd".to_string(),
            source: src.to_string(),
        }],
        &mut NoPackages,
    );
    check_program(&program)
}

/// Prints every diagnostic: the machine-readable first line, then — unless
/// `short` — a caret-anchored block beneath it (RFC-0006 **C3**).
///
/// # The first line is not negotiable
///
/// `file:line:col: error[kind]: message` is RFC-0006 **C7** and is parsed by
/// editor problem matchers. It is emitted **unstyled and byte-identical** to
/// what `byard check` has always printed. Colouring or reformatting it would
/// break integrations that then go *quiet* rather than failing, which nobody
/// notices for a long time.
///
/// The caret block goes **beneath** it, and that is where the palette applies:
///
/// ```text
/// main.byd:7:14: error[UnknownAttribute]: unknown attribute `colour` on `Column`
///   |
/// 7 |     Column #[colour: 0xFF0000] {
///   |              ^^^^^^
/// ```
///
/// `--short` suppresses the block entirely, for scripts that want one line per
/// error and nothing else.
pub fn print_diagnostics(errors: &[CompileError], map: &SourceMap, short: bool) {
    let p = style::palette();
    for err in errors {
        // Unstyled, on stderr, exactly as before.
        crate::statusline::log_stderr(&map.render_line(err));
        if short {
            continue;
        }
        let Some(caret) = map.caret(err) else {
            // A project-level diagnostic has a headline and no source to point
            // at. Printing an empty gutter for it would suggest the context was
            // lost rather than never existing.
            continue;
        };
        // Built line by line rather than as one continued literal: a `\`
        // continuation in a formatted string is exactly the kind of thing
        // `cargo fmt` rewrites into baked-in indentation, and the gutter's
        // alignment is the only thing holding the block together.
        let gutter = caret.line.to_string();
        let pad = " ".repeat(gutter.len());
        let mut block = String::with_capacity(caret.text.len() + 96);
        let (dim, err_c, reset) = (p.dim, p.err, p.reset);
        let _ = writeln!(block, "{dim}{pad} |{reset}");
        let _ = writeln!(block, "{dim}{gutter} |{reset} {}", caret.text);
        let _ = write!(block, "{dim}{pad} |{reset} ");
        for _ in 0..caret.caret_start {
            block.push(' ');
        }
        let _ = write!(block, "{err_c}");
        for _ in 0..caret.caret_len {
            block.push('^');
        }
        let _ = write!(block, "{reset}");
        crate::statusline::log_stderr(&block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC-0006 **C7**. Editor problem matchers parse this line; colouring or
    /// reformatting it breaks integrations that then go *quiet* rather than
    /// failing, which nobody notices for a long time.
    #[test]
    fn the_machine_readable_first_line_is_never_styled() {
        let program = program_with_a_bad_attribute();
        let err = &program.errors[0];
        let line = program.source_map.render_line(err);
        assert!(
            !line.contains('\x1b'),
            "the first line must carry no escapes even with colour forced on: {line:?}"
        );
        assert!(
            line.starts_with("main.byd:"),
            "and must keep the file:line:col shape: {line:?}"
        );
        assert!(line.contains("error[UnknownAttribute]:"), "{line:?}");
    }

    #[test]
    fn the_caret_points_at_the_span_in_display_columns() {
        let program = program_with_a_bad_attribute();
        let caret = program
            .source_map
            .caret(&program.errors[0])
            .expect("a diagnostic in a real file has source context");
        assert_eq!(caret.line, 2);
        assert!(caret.text.contains("colour"), "{:?}", caret.text);
        // The caret must land under `colour`, and the column the first line
        // reports must agree with it — a caret and a `line:col` that disagree
        // are worse than either alone.
        let at: String = caret.text.chars().skip(caret.caret_start).take(6).collect();
        assert_eq!(at, "colour");
        assert_eq!(caret.column, caret.caret_start + 1);
        assert!(caret.caret_len >= 1);
    }

    #[test]
    fn a_caret_column_counts_characters_not_bytes() {
        // A multi-byte character before the span would push the caret right by
        // its byte length if this counted bytes — the classic way a caret block
        // ends up pointing at nothing in particular.
        let src = "View Main() {\n    Text(\"héllo wörld\") #[colour: 1]\n}\n";
        let program = resolve_one(src);
        let caret = program
            .source_map
            .caret(&program.errors[0])
            .expect("context");
        let at: String = caret.text.chars().skip(caret.caret_start).take(6).collect();
        assert_eq!(at, "colour", "line was: {:?}", caret.text);
    }

    #[test]
    fn a_diagnostic_with_no_source_has_no_caret_block() {
        // A project-level error has a headline and nothing to point at.
        // Printing an empty gutter for it would suggest the context was lost
        // rather than never existing.
        let map = SourceMap::default();
        let err = CompileError::Project {
            span: byard_compiler::Span::new(0, 0),
            message: "no byard.toml".to_string(),
        };
        assert!(map.caret(&err).is_none());
    }

    fn resolve_one(src: &str) -> ResolvedProgram {
        struct NoPackages;
        impl byard_compiler::resolve::PackageProvider for NoPackages {
            fn package_files(
                &mut self,
                _dependent: &str,
                _package: &str,
            ) -> Result<Vec<byard_compiler::resolve::SourceFile>, String> {
                Ok(Vec::new())
            }
        }
        let program = byard_compiler::resolve::resolve_program(
            vec![byard_compiler::resolve::SourceFile {
                name: "main.byd".to_string(),
                source: src.to_string(),
            }],
            &mut NoPackages,
        );
        let errors = check_program(&program);
        ResolvedProgram { errors, ..program }
    }

    fn program_with_a_bad_attribute() -> ResolvedProgram {
        resolve_one(
            "View Main() {
    Box #[colour: 0xFF0000] {}
}
",
        )
    }

    #[test]
    fn clean_source_has_no_errors() {
        let errs =
            check_source("View Main() { Column #[gap: 8, p: (horizontal: 8)] { Text(\"hi\") } }");
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn bad_attr_is_reported() {
        let errs = check_source("View Main() { Column #[bogus: 1] {} }");
        assert!(
            errs.iter()
                .any(|e| matches!(e, CompileError::UnknownAttribute { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn removed_px_attr_is_reported() {
        // px/py are no longer accepted attributes.
        let errs = check_source("View Main() { Column #[px: 4] {} }");
        assert!(
            errs.iter().any(|e| matches!(
                e,
                CompileError::UnknownAttribute { name, .. } if name == "px"
            )),
            "{errs:?}"
        );
    }

    #[test]
    fn conflicting_spacing_is_reported() {
        let errs = check_source("View Main() { Column #[p: (horizontal: 4, left: 2)] {} }");
        assert!(
            errs.iter()
                .any(|e| matches!(e, CompileError::ConflictingSpacingField { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn parse_error_short_circuits() {
        let errs = check_source("View Main() { Column #[gap: ");
        assert!(!errs.is_empty());
    }

    #[test]
    fn use_in_a_bare_file_explains_it_needs_a_manifest() {
        let errs = check_source("use material\nView Main() { Text(\"x\") }");
        assert!(
            errs.iter().any(|e| matches!(
                e,
                CompileError::UnknownPackage { detail, .. } if detail.contains("byard.toml")
            )),
            "{errs:?}"
        );
    }
}
