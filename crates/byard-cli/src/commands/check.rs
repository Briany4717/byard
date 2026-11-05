//! `byard check [file]`, parse and validate without opening a window
//! (RFC-0006 §5, decision C7; RFC-0008: checks the *whole module graph*,
//! project siblings and packages included).

use crate::deps::resolve_project;
use crate::manifest::Manifest;
use crate::style;
use byard_compiler::CompileError;
use byard_compiler::diagnostics::{Style, render, render_short};
use byard_compiler::resolve::{ResolvedProgram, SourceMap};
use std::path::Path;

pub fn run(file: Option<&Path>, short: bool) -> Result<(), String> {
    let started = std::time::Instant::now();
    let manifest = Manifest::discover(file).map_err(|e| e.to_string())?;

    if manifest.library {
        style::action(&format!(
            "checking package `{}` ({})",
            manifest.name,
            manifest.project_root.display()
        ));
    } else {
        style::action(&format!("checking {}", manifest.entry.display()));
    }

    let (program, _provider) = resolve_project(&manifest).map_err(|e| e.to_string())?;
    let n_files = program.source_map.files().count();
    let n_pkgs = program.packages.len().saturating_sub(1);
    if n_files > 1 || n_pkgs > 0 {
        style::info(&format!("{n_files} file(s), {n_pkgs} package(s)"));
    }

    let diagnostics = check_program_with_theme(&program, manifest.theme);
    // A warning is printed and does not fail the check (RFC-0031 §Q5: failing a
    // build over an inert attribute is disproportionate). It is still printed
    // in full, with its caret block, because a diagnostic nobody sees is one
    // that was not worth raising.
    let n = diagnostics.iter().filter(|d| !d.is_warning()).count();
    let warnings = diagnostics.len() - n;

    if !diagnostics.is_empty() {
        print_diagnostics(&diagnostics, &program.source_map, short);
    }
    if n == 0 {
        let summary = if warnings == 0 {
            "0 errors".to_string()
        } else {
            format!(
                "0 errors, {warnings} warning{}",
                if warnings == 1 { "" } else { "s" }
            )
        };
        style::ok(&summary, Some(started.elapsed()));
        Ok(())
    } else {
        style::err(&format!("{n} error{}", if n == 1 { "" } else { "s" }));
        // Signal failure to the caller (main.rs maps Err → exit 1).
        Err(String::new())
    }
}

/// Headless semantic validation of a resolved program (no `wgpu`/`winit`),
/// see [`byard_project::check`], which the editor's language server runs too.
#[must_use]
pub fn check_program(program: &ResolvedProgram) -> Vec<CompileError> {
    check_program_with_theme(program, byard_compiler::interp::theme::Theme::byard_base())
}

/// Like [`check_program`], but validates against a specific design-token theme
/// (RFC-0022) so `inject Theme as t` resolves and `t.token` references are
/// checked against the project's *actual* declared tokens, a custom manifest
/// token must not be flagged `UnknownThemeToken`.
#[must_use]
pub fn check_program_with_theme(
    program: &ResolvedProgram,
    theme: byard_compiler::interp::theme::Theme,
) -> Vec<CompileError> {
    byard_project::check::check_program(program, theme)
}

/// Headless parse + semantic validation of one `.byd` source, the
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

/// Prints every diagnostic: the machine-readable first line, then, unless
/// `short`, a caret-anchored block beneath it (RFC-0006 **C3**).
///
/// The text is [`byard_compiler::diagnostics::render`]'s, so the first line
/// stays unstyled and byte-identical to what `byard check` has always
/// printed (RFC-0006 **C7**, see there) and an app that does not compile
/// prints the same block. What stays here is the part that is the CLI's own:
/// the palette it resolved, and writing through the statusline so the
/// readout is erased around the text.
pub fn print_diagnostics(errors: &[CompileError], map: &SourceMap, short: bool) {
    if errors.is_empty() {
        return;
    }
    let text = if short {
        render_short(errors, map)
    } else {
        let p = style::palette();
        render(
            errors,
            map,
            Style {
                dim: p.dim,
                err: p.err,
                reset: p.reset,
            },
        )
    };
    crate::statusline::log_stderr(&text);
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
        // reports must agree with it, a caret and a `line:col` that disagree
        // are worse than either alone.
        let at: String = caret.text.chars().skip(caret.caret_start).take(6).collect();
        assert_eq!(at, "colour");
        assert_eq!(caret.column, caret.caret_start + 1);
        assert!(caret.caret_len >= 1);
    }

    #[test]
    fn a_caret_column_counts_characters_not_bytes() {
        // A multi-byte character before the span would push the caret right by
        // its byte length if this counted bytes, the classic way a caret block
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
        assert!(!errs.is_empty(), "{errs:?}");
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

    // The cases below are code that is not on screen when the program starts.
    // Lowering is lazy for it, so `check` used to say `0 errors` for all of it.

    fn unknown_views(src: &str) -> Vec<String> {
        check_source(src)
            .iter()
            .filter_map(|e| match e {
                CompileError::UnknownView { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_tab_that_is_not_active_is_checked() {
        let found = unknown_views(
            "View Main() { var tab = \"a\"
                NavHost(active: tab) { tab \"a\" { Text(\"x\") }  tab \"b\" { Bogus() } } }",
        );
        assert_eq!(found, ["Bogus"]);
    }

    #[test]
    fn a_route_the_initial_path_misses_is_checked() {
        let found = unknown_views(
            "View Main() { var p = \"/\"
                NavStack(path: p) { route \"/\" { Text(\"x\") }
                    route \"/x/:id\" {|params| Bogus2() } } }",
        );
        assert_eq!(found, ["Bogus2"]);
    }

    #[test]
    fn both_arms_of_a_when_that_starts_false_are_checked() {
        let mut found = unknown_views(
            "View Main() { var on = false
                when on { Bogus3() } else { Text(\"x\") }
                when on { Text(\"y\") } else { Bogus4() } }",
        );
        found.sort();
        assert_eq!(found, ["Bogus3", "Bogus4"]);
    }

    #[test]
    fn a_when_inside_a_when_is_checked() {
        let found = unknown_views(
            "View Main() { var on = false
                when on { when on { Text(\"a\") } else { when on { Bogus5() } } } }",
        );
        assert_eq!(found, ["Bogus5"]);
    }

    #[test]
    fn the_body_of_a_for_over_a_list_that_has_not_loaded_is_checked() {
        let errs = check_source(
            "View Main() { var xs: List<Int> = []
                for x in xs { Text(x) #[bogus: 1] } }",
        );
        assert!(
            errs.iter()
                .any(|e| matches!(e, CompileError::UnknownAttribute { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn a_view_used_only_inside_an_inactive_tab_is_checked_with_its_arguments() {
        let errs = check_source(
            "View Detail(id: Str) { Text(id) }
             View Main() { var tab = \"a\"
                NavHost(active: tab) { tab \"a\" { Text(\"x\") }
                    tab \"b\" { Detail(id: \"1\", extra: 2) } } }",
        );
        assert!(
            errs.iter()
                .any(|e| matches!(e, CompileError::UnknownParam { .. })),
            "{errs:?}"
        );
        let errs = check_source(
            "View Detail(id: Str) { Text(id) }
             View Main() { var on = false
                when on { Detail() } }",
        );
        assert!(
            errs.iter()
                .any(|e| matches!(e, CompileError::MissingParam { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn a_routes_params_and_path_are_in_scope_in_every_route() {
        let errs = check_source(
            "View Detail(id: Str) { Text(id) }
             View Main() { var p = \"/\"
                NavStack(path: p) { route \"/\" { Text(\"x\") }
                    route \"/x/:id\" {|params| Detail(id: params.id)
                        Detail(id: route.params.id) Text(route.path) } } }",
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn a_for_body_and_a_recursive_view_are_checked_once_and_end() {
        // The offline pass visits each distinct body once, so a view that
        // contains itself under a `when` neither loops nor reports twice.
        let found = unknown_views(
            "View Tree(n: Int) { when n > 0 { Tree(n: n - 1) Bogus6() } }
             View Main() { Tree(n: 0) }",
        );
        assert_eq!(found, ["Bogus6"]);
    }
}
