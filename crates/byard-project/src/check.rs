//! Headless semantic validation of a resolved program (no `wgpu`/`winit`),
//! the check `byard check` runs and the editor's language server shares, so
//! the two cannot disagree about what is wrong with a project.

use byard_compiler::CompileError;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::ast::ViewDecl;
use byard_compiler::resolve::ResolvedProgram;

/// Validates `program` against `theme` (RFC-0022), the design-token set
/// `inject Theme as t` resolves to, so a custom manifest token is not flagged
/// `UnknownThemeToken`.
///
/// Resolve and parse errors short-circuit; otherwise every `View` is lowered
/// (every branch of it, shown or not) and rendered into a throwaway
/// [`RenderFrame`](byard_core::frame::RenderFrame), so attribute-contract and
/// `Len`-form validation checks, which run during lowering and render, are
/// exercised across the whole module graph.
#[must_use]
pub fn check_program(program: &ResolvedProgram, theme: Theme) -> Vec<CompileError> {
    check_views_where(program, theme, |_| true)
}

/// Like [`check_program`], but lowers and renders only the views `wanted`
/// accepts. Every view is still loaded, so the ones that are checked resolve
/// the ones that are not.
///
/// What an editor needs on a keystroke: the diagnostics of the file in front
/// of the person, in a program of many, without paying for the rest.
#[must_use]
pub fn check_views_where(
    program: &ResolvedProgram,
    theme: Theme,
    wanted: impl Fn(&ViewDecl) -> bool,
) -> Vec<CompileError> {
    if !program.errors.is_empty() {
        return program.errors.clone();
    }

    let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    interp.set_theme(theme);
    interp.set_checking(true);
    // RFC-0029 §7: the framework's own capability names are knowable
    // statically, so `inject Http as http` resolves here and its calls are
    // checked. An app's own controllers are not, and stay a warning
    // (`UncheckableInject`).
    //
    // The *provided* names, not the reserved ones: `Timer` is reserved because
    // `every`/`after` own the word and has no controller behind it, so
    // declaring it would call `inject Timer as t` checkable and let a runtime
    // failure through. Asking for the names rather than building a registry
    // also keeps the checker off the filesystem: constructing one resolves a
    // data directory and opens a store the app owns.
    let capabilities = byard_core::cap::provided_names();
    interp.declare_controllers(&capabilities);
    // Build the user-`View` registry once for the whole program so user-view
    // calls resolve and expand during lowering (RFC-0007 §1).
    interp.load_views(&program.views);
    let mut frame = byard_core::frame::RenderFrame::new();
    for view in program.views.iter().filter(|v| wanted(v)) {
        let tree = interp.lower_view(view, &known);
        interp.tick();
        interp.render(&tree, &mut frame, 1024.0, 768.0);
        frame.clear();
        // Lowering is lazy: a tab that is not the active one, a route the
        // initial path misses, a `when` that starts false and a `for` over a
        // list that has not loaded are not lowered by the render above, so
        // their diagnostics would wait for the first time somebody opens
        // them. Lower each once more, offline (nothing is mounted).
        interp.lower_every_branch();
    }
    let mut errors: Vec<CompileError> = Vec::new();
    for e in interp.errors() {
        // The same branch of a view reached through two callers reports the
        // same error twice.
        if !errors.contains(e) {
            errors.push(e.clone());
        }
    }
    errors
}
