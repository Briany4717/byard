//! Hot-reload boundary (RFC-0002 §"Hot-reload boundary"; D10/D11; RFC-0003 E5;
//! RFC-0004 §11).
//!
//! On a `.byd` change the new `ViewDecl` is structurally diffed against the
//! running one by the **shape** of its `param`/`var`/`let`/`inject` lists
//! (names + positions). Two outcomes:
//!
//! - **Reactive-compatible** (shape unchanged; only expressions/elements/control
//!   flow/style differ): keep the live `Signal`s, rebuild the scopes from the
//!   new AST, and read-tracking re-derives the dependency graph (§11). No
//!   flicker, state preserved.
//! - **Structure-incompatible** (a `var`/`param`/`inject` added, removed,
//!   reordered, or retyped): tear down and remount; state resets.
//!
//! Multiple `View`s per file are diffed **per `ViewDecl` name** (D11). A
//! structure-incompatible patch is **held** while a pointer gesture is in flight
//! (E5). The file watcher coalesces rapid saves latest-wins over a `bounded(1)`
//! channel (D10).

use std::collections::HashSet;

use super::views::{CallGraph, ViewTable};
use crate::parser::ast::{Member, ViewDecl};
use crate::symbol::Symbol;

/// The classification of a single-View patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReloadKind {
    /// Shape unchanged, keep `Signal`s, rebuild scopes (RFC-0002 case 1).
    ReactiveCompatible,
    /// Shape changed, teardown and remount (RFC-0002 case 2).
    StructureIncompatible,
}

/// A tag identifying which declaration list a shape entry came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tag {
    Param,
    Var,
    Let,
    Inject,
}

/// The reload-relevant shape of a View: the ordered `(tag, name)` of its
/// `param`/`var`/`let`/`inject` declarations. Expressions, elements, control
/// flow, and the style block are deliberately excluded, they may change freely
/// in a reactive-compatible patch.
fn shape(view: &ViewDecl) -> Vec<(Tag, Symbol)> {
    let mut s: Vec<(Tag, Symbol)> = view
        .params
        .iter()
        .map(|p| (Tag::Param, p.name.clone()))
        .collect();
    for member in &view.body {
        match member {
            Member::Var { name, .. } => s.push((Tag::Var, name.clone())),
            Member::Let { name, .. } => s.push((Tag::Let, name.clone())),
            Member::Inject { name, .. } => s.push((Tag::Inject, name.clone())),
            _ => {}
        }
    }
    s
}

/// Classifies a patch by diffing the two Views' shapes (RFC-0002 boundary).
#[must_use]
pub fn diff_view(old: &ViewDecl, new: &ViewDecl) -> ReloadKind {
    if shape(old) == shape(new) {
        ReloadKind::ReactiveCompatible
    } else {
        ReloadKind::StructureIncompatible
    }
}

/// What to do with one View when reloading a whole file (D11: per-`ViewDecl`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewReload {
    /// A newly added View, mount it.
    Added,
    /// A removed View, unmount it.
    Removed,
    /// A patch to an existing View.
    Patch(ReloadKind),
}

/// Diffs a whole file's Views by name (D11), so editing one View leaves the
/// others untouched.
#[must_use]
pub fn diff_program(old: &[ViewDecl], new: &[ViewDecl]) -> Vec<(Symbol, ViewReload)> {
    let mut out = Vec::new();
    for nv in new {
        if let Some(ov) = old.iter().find(|v| v.name == nv.name) {
            out.push((nv.name.clone(), ViewReload::Patch(diff_view(ov, nv))));
        } else {
            out.push((nv.name.clone(), ViewReload::Added));
        }
    }
    for ov in old {
        if !new.iter().any(|v| v.name == ov.name) {
            out.push((ov.name.clone(), ViewReload::Removed));
        }
    }
    out
}

/// The set of view names whose instances must be re-derived after an edit
/// (RFC-0007 §5): every **changed** view (body or shape differs, or added)
/// unioned with its **transitive callers**, because a caller embeds the callee's
/// expanded body. Instantiation widens per-`ViewDecl` reload (D11) to chase
/// callers through the call graph, the single source of truth for both
/// cycle detection and reload blast-radius.
///
/// A *removed* view's own name is included so the caller can drop it; its
/// callers are caught as changed in any well-formed edit that also updated them.
#[must_use]
pub fn affected_views(old: &[ViewDecl], new: &[ViewDecl]) -> HashSet<Symbol> {
    let (table, _) = ViewTable::build(new);
    let graph = CallGraph::build(&table);
    let mut affected: HashSet<Symbol> = HashSet::new();

    for nv in new {
        let modified = match old.iter().find(|v| v.name == nv.name) {
            Some(ov) => ov != nv, // body or shape differs
            None => true,         // newly added
        };
        if modified {
            affected.insert(nv.name.clone());
            // Union the changed view with its transitive callers (the view set
            // `affected_by` returns includes the view itself).
            if let Some(id) = table.resolve(&nv.name) {
                for aff in graph.affected_by(id) {
                    affected.insert(table.decl(aff).name.clone());
                }
            }
        }
    }
    // Removed views: include their name so the runtime can unmount them.
    for ov in old {
        if !new.iter().any(|v| v.name == ov.name) {
            affected.insert(ov.name.clone());
        }
    }
    affected
}

/// Whether a patch is applied now or held (E5 gesture gate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gated {
    /// Apply the patch on this tick.
    Apply,
    /// Hold the patch until the in-flight gesture releases.
    Defer,
}

/// Gates a patch on the gesture state (E5): reactive-compatible patches apply
/// even mid-gesture; structure-incompatible patches are deferred while a pointer
/// is pressed.
#[must_use]
pub fn gate(kind: ReloadKind, pointer_pressed: bool) -> Gated {
    match kind {
        ReloadKind::ReactiveCompatible => Gated::Apply,
        ReloadKind::StructureIncompatible => {
            if pointer_pressed {
                Gated::Defer
            } else {
                Gated::Apply
            }
        }
    }
}

/// A latest-wins, single-slot delivery channel (D10): the file watcher publishes
/// parsed results here; rapid saves coalesce so only the most recent survives,
/// and the logic thread takes it at tick step 1.
pub struct LatestWins<T> {
    tx: crossbeam_channel::Sender<T>,
    rx: crossbeam_channel::Receiver<T>,
}

impl<T> Default for LatestWins<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> LatestWins<T> {
    /// Creates an empty channel.
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::bounded(1);
        Self { tx, rx }
    }

    /// Publishes `value`, discarding any previous unconsumed value (latest-wins).
    /// Never blocks.
    pub fn publish(&self, value: T) {
        let _ = self.rx.try_recv(); // drop the stale pending value, if any
        let _ = self.tx.try_send(value);
    }

    /// Takes the most recent value, if any (non-blocking).
    pub fn take(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

// ── Notify file watcher ──────────────────────────────────────────────────

/// Parsed file result delivered by the file-watcher thread.
pub struct ParsedFile {
    /// The parsed views (may be empty on a parse error, preserving last good).
    pub views: Vec<crate::parser::ast::ViewDecl>,
    /// Diagnostics from the last parse attempt.
    pub errors: Vec<crate::diagnostics::CompileError>,
    /// The theme the manifest describes now, when the reparse read one: a
    /// `[theme]` edit, or a new seed image, applies without a restart.
    /// `None` leaves the running theme as it is.
    pub theme: Option<crate::interp::theme::Theme>,
    /// Files outside the watched directories the project is built from (a
    /// seed image elsewhere), as the manifest names them now. The watcher
    /// starts watching any it did not know, so one the manifest moves to is
    /// followed from that save on.
    pub watch_files: Vec<std::path::PathBuf>,
}

/// Spawns a background OS thread that watches `paths` with `notify` and
/// publishes the result of `reparse()` to `channel` on every relevant change
/// (generalized to the module graph by RFC-0008 Pillar E).
///
/// `reparse` re-derives the *whole program*, for a single-file project that
/// is one `parse`, for a multi-file/package project the CLI passes a closure
/// that re-runs the module resolver. Directories are watched recursively
/// (project sources and cooperative-dev `path` dependencies, D-J). Fetched,
/// lock-pinned cache packages are immutable and must not be in `paths`.
///
/// `files` are the files the project is built from besides its sources (the
/// manifest, a seed image, RFC-0022 §5). A change to one re-reads the project
/// whatever its extension, and one outside every directory in `paths` is
/// watched through its own directory, so an editor that saves by replacing
/// the file is still seen. After each re-read, the files the manifest names
/// then replace them, so a seed image it moves to is followed from that save.
///
/// Two event classes are routed: a source, the manifest, a raster image or
/// a watched file being written, created or removed triggers `reparse()` into
/// `channel`; an image or `.svg` changing sends its path to `assets` so the
/// runtime decodes or regenerates it in place (RFC-0009 §3).
///
/// A parse error keeps `views` empty so the caller retains the last-good view.
/// Returns the watcher handle, drop it to stop watching.
///
/// # Errors
///
/// Returns an error if `notify` fails to initialize the watcher or to register
/// a path (e.g. file does not exist).
pub fn start_watcher<F>(
    paths: &[std::path::PathBuf],
    files: &[std::path::PathBuf],
    channel: std::sync::Arc<LatestWins<ParsedFile>>,
    assets: crossbeam_channel::Sender<std::path::PathBuf>,
    reparse: F,
) -> Result<Watching, notify::Error>
where
    F: Fn() -> ParsedFile + Send + 'static,
{
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    let dirs: Vec<std::path::PathBuf> = paths.iter().map(|d| normal(d)).collect();
    let outside_of = |dirs: &[std::path::PathBuf], file: &std::path::Path| {
        !dirs.iter().any(|d| file.starts_with(d))
    };
    let outside = {
        let dirs = dirs.clone();
        move |file: &std::path::Path| outside_of(&dirs, file)
    };
    let initial: Vec<std::path::PathBuf> = files.iter().map(|f| normal(f)).collect();
    // Matched by path, whatever their extension.
    let files: Arc<Mutex<HashSet<std::path::PathBuf>>> =
        Arc::new(Mutex::new(initial.iter().cloned().collect()));
    // Directories to start watching, registered from a thread of their own:
    // `notify` must not be asked to watch from inside its own event handler.
    let (rewatch_tx, rewatch_rx) = crossbeam_channel::unbounded::<std::path::PathBuf>();
    let watched_files = Arc::clone(&files);

    let watcher = notify::RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| {
            let Ok(event) = result else { return };
            if !matches!(
                event.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            ) {
                return;
            }
            let mut source_changed = false;
            for p in event.paths.iter().filter(|p| !is_generated(p)) {
                let named = watched_files
                    .lock()
                    .is_ok_and(|f| f.contains(p) || f.contains(&normal(p)));
                if named || reparses_on(p) {
                    source_changed = true;
                }
                if is_raster(p) || p.extension().is_some_and(|e| e == "svg") {
                    // An image or vector asset changed: hand its path to
                    // the runtime, which decodes or regenerates it in
                    // place. A disconnected receiver (runtime torn down)
                    // is not the watcher's problem.
                    let _ = assets.send(p.clone());
                }
            }
            if source_changed {
                let parsed = reparse();
                if let Ok(mut known) = watched_files.lock() {
                    // The manifest's files now, not added to the old: a seed
                    // image it no longer names is not the project's any more.
                    // A failed re-read names none, so the set is kept.
                    if !parsed.watch_files.is_empty() {
                        let now: HashSet<_> =
                            parsed.watch_files.iter().map(|f| normal(f)).collect();
                        for file in now.difference(&known) {
                            if let Some(parent) = file.parent().filter(|_| outside(file)) {
                                let _ = rewatch_tx.send(parent.to_path_buf());
                            }
                        }
                        *known = now;
                    }
                }
                channel.publish(parsed);
            }
        },
        notify::Config::default(),
    )?;
    let watcher = Arc::new(Mutex::new(watcher));
    {
        let mut w = watcher
            .lock()
            .map_err(|_| notify::Error::generic("watcher lock"))?;
        for path in paths {
            w.watch(path, RecursiveMode::Recursive)?;
        }
        // A file's directory may not exist (a seed image not written yet):
        // that is the manifest's error to report, not the watcher's.
        for file in initial.iter().filter(|f| outside_of(&dirs, f)) {
            if let Some(parent) = file.parent() {
                let _ = w.watch(parent, RecursiveMode::NonRecursive);
            }
        }
    }
    // Ends when the watcher (which owns the sender) is dropped.
    let weak = Arc::downgrade(&watcher);
    std::thread::spawn(move || {
        while let Ok(dir) = rewatch_rx.recv() {
            let Some(watcher) = weak.upgrade() else { break };
            if let Ok(mut w) = watcher.lock() {
                let _ = w.watch(&dir, RecursiveMode::NonRecursive);
            }
        }
    });
    Ok(Watching { _watcher: watcher })
}

/// A running file watcher; dropping it stops watching.
pub struct Watching {
    _watcher: std::sync::Arc<std::sync::Mutex<notify::RecommendedWatcher>>,
}

/// `path` as the watcher reports it: canonical, or for a file that is gone,
/// its directory canonical and its name kept.
fn normal(path: &std::path::Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        match (
            path.parent().and_then(|d| d.canonicalize().ok()),
            path.file_name(),
        ) {
            (Some(dir), Some(name)) => dir.join(name),
            _ => path.to_path_buf(),
        }
    })
}

/// Whether a change to `path` re-reads the project: a source file, the
/// manifest, or a raster image, since the manifest can derive its theme from
/// one (`[theme] seed = { image = "…" }`, RFC-0022 §5) and replacing that
/// picture has to move the palette the way editing a hex seed does. An image
/// the theme does not use costs one resolve that finds nothing changed.
fn reparses_on(path: &std::path::Path) -> bool {
    path.file_name().is_some_and(|f| f == "byard.toml")
        || path.extension().is_some_and(|e| e == "byd")
        || is_raster(path)
}

/// Whether `path` lies in a directory tools write into rather than the
/// author: build output, caches, version control. The project root is
/// watched whole, so a build or a cache write there must not look like an
/// edit.
fn is_generated(path: &std::path::Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some(".byard" | "target" | ".git" | "node_modules")
        )
    })
}

/// Whether `path` is a raster image an `Image` can draw, which the runtime
/// decodes again when it changes (sent on the asset channel, like `.svg`).
#[must_use]
pub fn is_raster(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|e| {
        ["png", "jpg", "jpeg"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_image_edit_reparses_and_a_vector_does_not() {
        use std::path::Path;
        for yes in [
            "src/main.byd",
            "byard.toml",
            "brand.png",
            "photo.JPG",
            "a.jpeg",
        ] {
            assert!(reparses_on(Path::new(yes)), "{yes}");
        }
        // An `.svg` is regenerated in place by the runtime instead.
        for no in ["icon.svg", "notes.md", "byard.lock"] {
            assert!(!reparses_on(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn build_output_and_caches_are_not_edits() {
        use std::path::Path;
        for no in [
            "app/.byard/cache/vectors/a.png",
            "app/target/debug/x.png",
            "app/.git/index",
        ] {
            assert!(is_generated(Path::new(no)), "{no}");
        }
        for yes in ["app/assets/logo.png", "app/src/main.byd", "app/byard.toml"] {
            assert!(!is_generated(Path::new(yes)), "{yes}");
        }
        assert!(is_raster(Path::new("a/logo.PNG")) && !is_raster(Path::new("icon.svg")));
    }
    use crate::interp::env::Value;
    use crate::interp::eval::Interpreter;
    use crate::parser::parse;

    fn view(src: &str) -> ViewDecl {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        parsed.views.into_iter().next().unwrap()
    }

    #[test]
    fn same_shape_different_body_is_reactive_compatible() {
        let old = view("View C() { var count = 0\n Text(\"{count}\") }");
        let new = view("View C() { var count = 0\n Text(\"Count: {count}!\") }");
        assert_eq!(diff_view(&old, &new), ReloadKind::ReactiveCompatible);
    }

    #[test]
    fn added_var_is_structure_incompatible() {
        let old = view("View C() { var count = 0 }");
        let new = view("View C() { var count = 0\n var other = 1 }");
        assert_eq!(diff_view(&old, &new), ReloadKind::StructureIncompatible);
        // Reordering is also incompatible.
        let a = view("View C() { var a = 0\n var b = 1 }");
        let b = view("View C() { var b = 1\n var a = 0 }");
        assert_eq!(diff_view(&a, &b), ReloadKind::StructureIncompatible);
    }

    #[test]
    fn reactive_compatible_reload_preserves_signal_state() {
        let old = view("View C() { var count = 0\n Text(\"{count}\") }");
        let new = view("View C() { var count = 0\n Text(\"Count: {count}\") }");

        let mut interp = Interpreter::new();
        let Member::Var { name, init, .. } = &old.body[0] else {
            panic!()
        };
        interp.define_var(name.clone(), init);
        let sig = interp.var_signal(name).unwrap();
        interp.write_var(sig, Value::Int(5)); // user incremented to 5

        interp.reload(&new, diff_view(&old, &new));
        let new_sig = interp.var_signal(name).unwrap();
        assert_eq!(
            interp.peek(new_sig),
            Value::Int(5),
            "case-1 keeps the counter"
        );
    }

    #[test]
    fn structure_incompatible_reload_resets_state() {
        let old = view("View C() { var count = 0 }");
        let new = view("View C() { var count = 0\n var added = 9 }");

        let mut interp = Interpreter::new();
        let Member::Var { name, init, .. } = &old.body[0] else {
            panic!()
        };
        interp.define_var(name.clone(), init);
        let sig = interp.var_signal(name).unwrap();
        interp.write_var(sig, Value::Int(5));

        interp.reload(&new, diff_view(&old, &new));
        let new_sig = interp.var_signal(name).unwrap();
        assert_eq!(
            interp.peek(new_sig),
            Value::Int(0),
            "case-2 resets the counter"
        );
    }

    #[test]
    fn gesture_gate_holds_structural_patches_while_pressed() {
        assert_eq!(gate(ReloadKind::ReactiveCompatible, true), Gated::Apply);
        assert_eq!(gate(ReloadKind::StructureIncompatible, true), Gated::Defer);
        assert_eq!(gate(ReloadKind::StructureIncompatible, false), Gated::Apply);
    }

    #[test]
    fn rapid_saves_coalesce_latest_wins() {
        let chan: LatestWins<i32> = LatestWins::new();
        chan.publish(1);
        chan.publish(2);
        chan.publish(3);
        assert_eq!(chan.take(), Some(3), "only the most recent save applies");
        assert_eq!(chan.take(), None);
    }

    fn views_of(src: &str) -> Vec<ViewDecl> {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        parsed.views
    }

    #[test]
    fn editing_a_leaf_view_affects_its_transitive_callers() {
        // App → UserRow → Avatar (leaf). Editing Avatar's body re-derives
        // Avatar, UserRow, and App.
        let old = views_of(
            "View Avatar() { Image(\"a.png\") }\n\
             View UserRow() { Avatar() }\n\
             View App() { UserRow() }",
        );
        let new = views_of(
            "View Avatar() { Image(\"b.png\") }\n\
             View UserRow() { Avatar() }\n\
             View App() { UserRow() }",
        );
        let affected = affected_views(&old, &new);
        assert!(affected.contains(&Symbol::intern("Avatar")));
        assert!(affected.contains(&Symbol::intern("UserRow")));
        assert!(affected.contains(&Symbol::intern("App")));
    }

    #[test]
    fn editing_a_shared_view_affects_both_callers() {
        let old = views_of(
            "View Chip() { Text(\"x\") }\n\
             View A() { Chip() }\n\
             View B() { Chip() }",
        );
        let new = views_of(
            "View Chip() { Text(\"y\") }\n\
             View A() { Chip() }\n\
             View B() { Chip() }",
        );
        let affected = affected_views(&old, &new);
        assert!(affected.contains(&Symbol::intern("Chip")));
        assert!(affected.contains(&Symbol::intern("A")));
        assert!(affected.contains(&Symbol::intern("B")));
    }

    #[test]
    fn an_untouched_independent_view_is_not_affected() {
        // `Lonely` is unrelated to the edited `Chip`; it stays out of the set.
        let old = views_of("View Chip() { Text(\"x\") }\nView Lonely() { Text(\"z\") }");
        let new = views_of("View Chip() { Text(\"y\") }\nView Lonely() { Text(\"z\") }");
        let affected = affected_views(&old, &new);
        assert!(affected.contains(&Symbol::intern("Chip")));
        assert!(!affected.contains(&Symbol::intern("Lonely")));
    }

    #[test]
    fn editing_one_view_leaves_others_in_a_multi_view_file() {
        let old = [
            view("View A() { var a = 0 }"),
            view("View B() { var b = 0 }"),
        ];
        let new = [
            view("View A() { var a = 0 }"),                 // unchanged
            view("View B() { var b = 0\n Text(\"{b}\") }"), // body-only edit
        ];
        let diffs = diff_program(&old, &new);
        let a = diffs
            .iter()
            .find(|(n, _)| *n == Symbol::intern("A"))
            .unwrap()
            .1;
        let b = diffs
            .iter()
            .find(|(n, _)| *n == Symbol::intern("B"))
            .unwrap()
            .1;
        assert_eq!(a, ViewReload::Patch(ReloadKind::ReactiveCompatible));
        assert_eq!(b, ViewReload::Patch(ReloadKind::ReactiveCompatible));
    }
}
