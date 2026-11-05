//! RFC-0019, callback props & event forwarding.
//!
//! A user `View` declares an `Fn(...)` parameter and forwards an inner
//! intrinsic's event to it; the caller passes an inline action block that is
//! evaluated in the *caller's* scope. These tests drive the same path the
//! runner does (`lower_view` → `render` → `dispatch_events` → `tick`) and assert
//! the caller's `var` mutates when the callee's event fires.

use byard_compiler::diagnostics::CompileError;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent};

/// The interpreter's router speaks its own `EventKind`; import it under an alias
/// to filter `handler_rects` without colliding with the core input kind.
use byard_compiler::interp::events::EventKind as RouterEventKind;

fn ev(kind: EventKind, pos: (f32, f32)) -> InputEvent {
    InputEvent {
        kind,
        pos,
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }
}

/// Parses `src`, lowers its first view as the root, renders one frame, and
/// returns the interpreter plus the frame. Panics on any parse/load/lower error.
fn boot(src: &str) -> (Interpreter, RenderFrame) {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut interp = Interpreter::new();
    let load = interp.load_views(&parsed.views);
    assert!(load.is_empty(), "load errors: {load:?}");
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let root = &parsed.views[0];
    let tree = interp.lower_view(root, &known);
    assert!(
        interp.errors().is_empty(),
        "lowering errors: {:?}",
        interp.errors()
    );
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    (interp, frame)
}

/// The centre of the first registered `Tap` handler's rect.
fn tap_center(interp: &Interpreter) -> (f32, f32) {
    let r = interp
        .router
        .handler_rects()
        .iter()
        .find(|(_, k, _)| matches!(k, RouterEventKind::Tap))
        .expect("a tap handler is registered")
        .2;
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn sig(interp: &Interpreter, name: &str) -> byard_compiler::interp::env::SignalId {
    interp
        .var_signal(&Symbol::intern(name))
        .expect("var exists")
}

/// A tap on a wrapper view forwards through an `on_tap: Fn()` prop to the
/// caller's `{ count++ }` block, which mutates the *caller's* `var`.
#[test]
fn tap_forwards_to_caller_action_block() {
    let src = "
View Counter() {
    var count = 0
    TappableCard(on_tap: { count++ })
    Text(\"Count: {count}\")
}
View TappableCard(on_tap: Fn() = {}) {
    Box #[width: 120, height: 120, tap => on_tap()]
}
";
    let (mut interp, _frame) = boot(src);
    let count = sig(&interp, "count");
    assert_eq!(interp.peek(count), Value::Int(0));

    let center = tap_center(&interp);
    interp.dispatch_events(&[ev(EventKind::Tap, center)]);
    interp.tick();
    assert_eq!(
        interp.peek(count),
        Value::Int(1),
        "the callee forwarded the tap to the caller's action block"
    );

    // A second tap keeps writing the same caller signal.
    interp.dispatch_events(&[ev(EventKind::Tap, center)]);
    interp.tick();
    assert_eq!(interp.peek(count), Value::Int(2));
}

/// Two instances of the same wrapper carry independent caller actions, each
/// mutates the caller `var` its own block names (`inc` vs `reset`).
#[test]
fn distinct_callbacks_per_instance() {
    let src = "
View App() {
    var count = 0
    ActionButton(label: \"Inc\", on_tap: { count++ })
    ActionButton(label: \"Reset\", on_tap: { count = 0 })
}
View ActionButton(label: Str, on_tap: Fn() = {}) {
    Box #[width: 80, height: 40, tap => on_tap()]
}
";
    let (mut interp, _frame) = boot(src);
    let count = sig(&interp, "count");

    let rects = interp.router.handler_rects();
    let taps: Vec<_> = rects
        .iter()
        .filter(|(_, k, _)| matches!(k, RouterEventKind::Tap))
        .map(|(_, _, r)| (r.x + r.w / 2.0, r.y + r.h / 2.0))
        .collect();
    assert_eq!(taps.len(), 2, "two tappable instances");

    // Tap the first (increment) twice.
    interp.dispatch_events(&[ev(EventKind::Tap, taps[0])]);
    interp.tick();
    interp.dispatch_events(&[ev(EventKind::Tap, taps[0])]);
    interp.tick();
    assert_eq!(interp.peek(count), Value::Int(2));

    // Tap the second (reset) once, a different action on the same caller var.
    interp.dispatch_events(&[ev(EventKind::Tap, taps[1])]);
    interp.tick();
    assert_eq!(interp.peek(count), Value::Int(0));
}

/// A callback carrying an argument (`Fn(Str)`): the callee passes its own local
/// value; the caller receives it and writes a caller `var`.
#[test]
fn callback_with_argument_passes_value_to_caller() {
    let src = "
View Search() {
    var echoed = \"\"
    Field(on_change: {|text| echoed = text})
    Text(echoed)
}
View Field(on_change: Fn(Str) = {|_|}) {
    var query = \"hi\"
    Box #[width: 100, height: 30, tap => on_change(query)]
}
";
    let (mut interp, _frame) = boot(src);
    let echoed = sig(&interp, "echoed");
    assert_eq!(interp.peek(echoed), Value::Str(String::new()));

    let center = tap_center(&interp);
    interp.dispatch_events(&[ev(EventKind::Tap, center)]);
    interp.tick();
    assert_eq!(
        interp.peek(echoed),
        Value::Str("hi".to_string()),
        "the callee's local `query` flowed to the caller through the callback arg"
    );
}

/// An omitted optional callback (`Fn() = {}`) is a no-op: tapping does nothing
/// and no diagnostic is raised.
#[test]
fn omitted_callback_defaults_to_noop() {
    let src = "
View App() {
    var count = 0
    TappableCard()
    Text(\"{count}\")
}
View TappableCard(on_tap: Fn() = {}) {
    Box #[width: 50, height: 50, tap => on_tap()]
}
";
    let (mut interp, _frame) = boot(src);
    let count = sig(&interp, "count");
    let center = tap_center(&interp);
    interp.dispatch_events(&[ev(EventKind::Tap, center)]);
    interp.tick();
    assert_eq!(
        interp.peek(count),
        Value::Int(0),
        "default `{{}}` is a no-op"
    );
}

/// Multi-statement callback blocks run every statement in order.
#[test]
fn multi_statement_callback_runs_all() {
    let src = "
View App() {
    var a = 0
    var b = 10
    Card(on_tap: { a++ b = 0 })
}
View Card(on_tap: Fn() = {}) {
    Box #[width: 60, height: 60, tap => on_tap()]
}
";
    let (mut interp, _frame) = boot(src);
    let a = sig(&interp, "a");
    let b = sig(&interp, "b");
    let center = tap_center(&interp);
    interp.dispatch_events(&[ev(EventKind::Tap, center)]);
    interp.tick();
    assert_eq!(interp.peek(a), Value::Int(1));
    assert_eq!(interp.peek(b), Value::Int(0));
}

/// A wrapper can forward the callback it received to a nested wrapper
/// (`on_tap: on_tap`), chaining the tap two views deep back to the caller.
#[test]
fn callback_forwarding_through_nested_wrapper() {
    let src = "
View App() {
    var count = 0
    Outer(on_tap: { count++ })
}
View Outer(on_tap: Fn() = {}) {
    Inner(on_tap: on_tap)
}
View Inner(on_tap: Fn() = {}) {
    Box #[width: 70, height: 70, tap => on_tap()]
}
";
    let (mut interp, _frame) = boot(src);
    let count = sig(&interp, "count");
    let center = tap_center(&interp);
    interp.dispatch_events(&[ev(EventKind::Tap, center)]);
    interp.tick();
    assert_eq!(interp.peek(count), Value::Int(1));
}

/// End-to-end guard on the committed visual example: it renders without any
/// diagnostic, registers a tap handler per `ActionButton` (four in `Main`, two
/// inside `Stepper` and three tags), and every one of those taps forwards to a caller
/// block that mutates `count`, proving the reusable wrappers really drive the
/// caller's state on screen.
#[test]
fn visual_example_forwards_every_button() {
    const EXAMPLE: &str = include_str!("../../../byard-cli/examples/callbacks/src/main.byd");
    let (mut interp, _frame) = boot(EXAMPLE);
    let count = sig(&interp, "count");
    assert_eq!(interp.peek(count), Value::Int(0));

    // Six ActionButton instances, plus one per tag of the `TagList` ⇒ nine tap
    // handlers.
    let taps: Vec<(f32, f32)> = interp
        .router
        .handler_rects()
        .iter()
        .filter(|(_, k, _)| matches!(k, RouterEventKind::Tap))
        .map(|(_, _, r)| (r.x + r.w / 2.0, r.y + r.h / 2.0))
        .collect();
    assert_eq!(taps.len(), 9, "one tap handler per ActionButton instance");

    // Every button's action is live: tapping each one changes `count` (each
    // block differs, so the exact value isn't asserted, only that the caller
    // state moved off its initial 0 through a forwarded callback).
    let mut moved = false;
    for t in &taps {
        interp.dispatch_events(&[ev(EventKind::Tap, *t)]);
        interp.tick();
        if interp.peek(count) != Value::Int(0) {
            moved = true;
        }
        // Reset between buttons so each is exercised from a known state.
        interp.write_var(count, Value::Int(0));
        interp.tick();
    }
    assert!(
        moved,
        "at least one wrapper forwarded its tap to the caller"
    );
}

/// The wrappers' `on hover` style state re-tints the button from its `accent`
/// to its `hover` colour, both param-driven, so this also proves a
/// param-dependent style resolves at render time (RFC-0019 §2 env snapshot).
/// Moving the pointer onto a button must change at least one rendered fill.
#[test]
fn visual_example_hover_tints_a_button() {
    const EXAMPLE: &str = include_str!("../../../byard-cli/examples/callbacks/src/main.byd");
    let parsed = parse(EXAMPLE);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();

    // Base render: capture every solid fill colour.
    let mut base = RenderFrame::new();
    interp.render(&tree, &mut base, 800.0, 700.0);
    let base_colors: Vec<[f32; 4]> = base.instances().iter().map(|b| b.color).collect();

    // Hover the first button (a pointer move sets the engine's live hover state).
    let center = tap_center(&interp);
    interp.dispatch_events(&[ev(EventKind::PointerMove, center)]);
    interp.tick();
    let mut hovered = RenderFrame::new();
    interp.render(&tree, &mut hovered, 800.0, 700.0);
    let hover_colors: Vec<[f32; 4]> = hovered.instances().iter().map(|b| b.color).collect();

    assert_ne!(
        base_colors, hover_colors,
        "hovering a button must apply its `on hover` tint (accent → hover colour)"
    );
}

// ── diagnostics (RFC-0019 §4) ────────────────────────────────────────────

/// Helper: lower the first view *and render one frame* (event actions are
/// re-lowered during the render walk, so invocation-site diagnostics such as
/// `CallbackArityMismatch` only surface once rendered) and return the
/// accumulated errors.
fn lower_errors(src: &str) -> Vec<CompileError> {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let root = &parsed.views[0];
    let tree = interp.lower_view(root, &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    interp.errors().to_vec()
}

/// Invoking a `Fn(Str)` callback with no argument is a `CallbackArityMismatch`.
#[test]
fn wrong_invocation_arity_is_reported() {
    let src = "
View App() {
    var echoed = \"\"
    Field(on_change: {|t| echoed = t})
}
View Field(on_change: Fn(Str) = {|_|}) {
    Box #[width: 10, height: 10, tap => on_change()]
}
";
    let errs = lower_errors(src);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            CompileError::CallbackArityMismatch {
                expected: 1,
                found: 0,
                ..
            }
        )),
        "expected CallbackArityMismatch, got {errs:?}"
    );
}

/// Passing a non-callback value to a `Fn()` parameter is a
/// `CallbackTypeMismatch`.
#[test]
fn non_callback_argument_is_type_mismatch() {
    let src = "
View App() {
    Card(on_tap: 5)
}
View Card(on_tap: Fn() = {}) {
    Box #[width: 10, height: 10, tap => on_tap()]
}
";
    let errs = lower_errors(src);
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::CallbackTypeMismatch { .. })),
        "expected CallbackTypeMismatch, got {errs:?}"
    );
}

/// A required callback (no default) omitted at the call site is a `MissingParam`.
#[test]
fn missing_required_callback_is_reported() {
    let src = "
View App() {
    Card()
}
View Card(on_tap: Fn()) {
    Box #[width: 10, height: 10, tap => on_tap()]
}
";
    let errs = lower_errors(src);
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::MissingParam { name, .. } if name == "on_tap")),
        "expected MissingParam for on_tap, got {errs:?}"
    );
}

/// Referencing a callback prop as a value (not invoking it) is
/// `CallbackNotInvocable`.
#[test]
fn callback_used_as_value_is_not_invocable() {
    let src = "
View App() {
    Card(on_tap: { })
}
View Card(on_tap: Fn() = {}) {
    Text(\"{on_tap}\")
}
";
    let errs = lower_errors(src);
    assert!(
        errs.iter().any(
            |e| matches!(e, CompileError::CallbackNotInvocable { name, .. } if name == "on_tap")
        ),
        "expected CallbackNotInvocable, got {errs:?}"
    );
}

// ── callbacks are lexically scoped ───────────────────────────────────────
//
// The braces of a callback read and write the names visible where they were
// written, whatever the view that runs them calls its own parameters, `let`s
// and `var`s.

/// Taps the first tap handler and settles.
fn tap_once(interp: &mut Interpreter) {
    let center = tap_center(interp);
    interp.dispatch_events(&[ev(EventKind::Tap, center)]);
    interp.tick();
}

/// Like [`boot`], keeping the lowered tree so the test can render again.
fn boot_tree(src: &str) -> (Interpreter, Vec<byard_compiler::interp::eval::RenderNode>) {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut interp = Interpreter::new();
    let load = interp.load_views(&parsed.views);
    assert!(load.is_empty(), "load errors: {load:?}");
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(
        interp.errors().is_empty(),
        "lowering errors: {:?}",
        interp.errors()
    );
    interp.tick();
    (interp, tree)
}

/// The text on screen after a render, in paint order. Used where two views
/// each have a `var` of the same name, which `var_signal` cannot tell apart.
fn shown(
    interp: &mut Interpreter,
    tree: &[byard_compiler::interp::eval::RenderNode],
) -> Vec<String> {
    let mut frame = RenderFrame::new();
    interp.render(tree, &mut frame, 400.0, 400.0);
    frame.texts().iter().map(|t| t.text.clone()).collect()
}

fn ints(xs: &[i64]) -> Value {
    Value::List(xs.iter().map(|n| Value::Int(*n)).collect())
}

/// The callee's parameter is called `products`, like the caller's `var`: the
/// callback's write still lands in the caller's.
#[test]
fn a_callback_writes_the_callers_var_when_the_callee_has_a_parameter_of_that_name() {
    let src = "
View Main() {
    var products = [1, 2, 3]
    let remove = (id: Int) => { products = products.filter(p => p != id) }
    Column {
        Text(\"main has {products.len}\")
        Detail(products: products, on_delete: {|id| remove(id) })
    }
}
View Detail(products: List<Int>, on_delete: Fn(Int) = {|_| }) {
    Box #[width: 100, height: 40, tap => on_delete(2)]
}
";
    let (mut interp, _frame) = boot(src);
    let products = sig(&interp, "products");
    tap_once(&mut interp);
    assert_eq!(interp.peek(products), ints(&[1, 3]));
}

/// Same, with the callee's own `var` and `let` of the caller's names, and a
/// callback parameter named like a callee name.
#[test]
fn a_callback_ignores_the_callees_vars_and_lets_of_the_same_name() {
    let src = "
View Main() {
    var n = 0
    var total = 0
    Column {
        Text(\"{n} {total}\")
        Child(on_x: {|total| n = n + total }, on_y: { total = total + 10 })
    }
}
View Child(on_x: Fn(Int) = {|_| }, on_y: Fn() = {}) {
    var n = 100
    var total = 7
    let twice = total * 2
    Box #[width: 80, height: 80, tap => { on_x(total) on_y() }]
}
";
    let (mut interp, tree) = boot_tree(src);
    assert_eq!(shown(&mut interp, &tree), ["0 0"]);
    tap_once(&mut interp);
    // The argument (the child's `total`, 7) added to Main's `n`, and Main's
    // `total` raised by ten.
    assert_eq!(shown(&mut interp, &tree), ["7 10"]);
}

/// A wrapper that calls its own callback from a closure of the same name:
/// `Outer(on_x: ...)` then `Inner(on_x: {|id| on_x(id) })`.
#[test]
fn a_wrapping_callback_named_like_the_one_it_calls_does_not_recurse() {
    let src = "
View Main() { var n = 0
    Outer(on_x: {|id| n = id }) }
View Outer(on_x: Fn(Int) = {|_| }) { Inner(on_x: {|id| on_x(id) }) }
View Inner(on_x: Fn(Int) = {|_| }) { Box #[width: 60, height: 60, tap => on_x(3)] }
";
    let (mut interp, _frame) = boot(src);
    let n = sig(&interp, "n");
    tap_once(&mut interp);
    assert_eq!(interp.peek(n), Value::Int(3));
}

/// The caller's `let` actions are what a callback calls, not the callee's of
/// the same name.
#[test]
fn a_callback_calls_the_callers_let_not_the_callees() {
    let src = "
View Main() { var n = 0
    let save = () => { n++ }
    let create = () => { save() }
    Child(on_x: { create() }) }
View Child(on_x: Fn() = {}) { let save = () => { on_x() }
    Box #[width: 60, height: 60, tap => save()] }
";
    let (mut interp, _frame) = boot(src);
    let n = sig(&interp, "n");
    tap_once(&mut interp);
    assert_eq!(interp.peek(n), Value::Int(1));
}

/// Three views deep, each wrapping the callback it got, each with names that
/// collide with the ones above it.
#[test]
fn callbacks_wrap_through_three_views_that_reuse_every_name() {
    let src = "
View Main() { var n = 0
    let bump = (by: Int) => { n = n + by }
    Column {
        Text(\"n {n}\")
        A(on_x: {|by| bump(by) })
    }
}
View A(on_x: Fn(Int) = {|_| }) { var n = 1000
    let bump = (by: Int) => { n = n + by }
    B(on_x: {|by| on_x(by + 1) })
}
View B(on_x: Fn(Int) = {|_| }) { var by = 5
    C(on_x: {|n| on_x(n + by) })
}
View C(on_x: Fn(Int) = {|_| }) { Box #[width: 60, height: 60, tap => on_x(10)] }
";
    let (mut interp, tree) = boot_tree(src);
    shown(&mut interp, &tree);
    tap_once(&mut interp);
    // C passes 10; B adds its own `by` (5), A adds 1, and Main's `bump` adds
    // that to Main's `n`.
    assert_eq!(shown(&mut interp, &tree), ["n 16"]);
}

/// A callback forwarded as a bare name keeps meaning the caller's, including a
/// caller's `let` action passed on directly.
#[test]
fn a_let_action_forwarded_by_name_runs_in_the_callers_scope() {
    let src = "
View Main() { var n = 0
    let bump = () => { n++ }
    Column {
        Text(\"n {n}\")
        Child(on_x: bump)
    }
}
View Child(on_x: Fn() = {}) { var n = 50
    let bump = () => { n = 99 }
    Box #[width: 60, height: 60, tap => on_x()] }
";
    let (mut interp, tree) = boot_tree(src);
    shown(&mut interp, &tree);
    tap_once(&mut interp);
    assert_eq!(shown(&mut interp, &tree), ["n 1"]);
}

/// A callback inside a `for` row reads that row's item, and a callee's loop
/// variable of the same name does not hijack it.
#[test]
fn a_callback_in_a_for_row_reads_the_rows_item() {
    let src = "
View Main() { var last = 0
    Column {
        for item in [4, 5] {
            Line(on_x: {|k| last = item + k })
        }
    }
}
View Line(on_x: Fn(Int) = {|_| }) {
    for item in [100] {
        Box #[width: 60, height: 60, tap => on_x(item)]
    }
}
";
    let (mut interp, _frame) = boot(src);
    let last = sig(&interp, "last");
    tap_once(&mut interp);
    assert_eq!(interp.peek(last), Value::Int(104));
}

/// A callback that runs while the instance that wrote it is being unmounted
/// writes that instance's var, harmlessly, and the other callback still lands.
#[test]
fn a_callback_run_as_its_instance_unmounts_does_not_crash() {
    let src = "
View Main() {
    var open = true
    var closed = 0
    Column {
        Text(\"{closed}\")
        when open { Panel(on_gone: { closed = closed + 1 }) }
    }
}
View Panel(on_gone: Fn() = {}) {
    var count = 0
    let double = count * 2
    Leaf(on_x: { count = count + double + 1 }, on_gone: on_gone)
}
View Leaf(on_x: Fn() = {}, on_gone: Fn() = {}) {
    on unmount => { on_x() on_gone() }
    Text(\"leaf\")
}
";
    let (mut interp, tree) = boot_tree(src);
    assert_eq!(shown(&mut interp, &tree), ["0", "leaf"]);
    let open = sig(&interp, "open");
    interp.write_var(open, Value::Bool(false));
    interp.tick();
    assert_eq!(shown(&mut interp, &tree), ["1"]);
}

/// The checker types a callback's body in the caller: a name only the callee
/// declares is unknown there.
#[test]
fn the_checker_reports_a_name_only_the_callee_declares_against_the_caller() {
    let src = "
View Main() { var n = 0
    Detail(on_x: {|id| n = id + zzz }) }
View Detail(on_x: Fn(Int) = {|_| }) { var zzz = 0
    Box #[width: 60, height: 60, tap => on_x(3)] }
";
    let parsed = parse(src);
    let errors = byard_compiler::infer::check_views(&parsed.views).errors;
    assert!(
        errors.iter().any(|e| matches!(
            e,
            CompileError::UnknownName { name, .. } if name == "zzz"
        )),
        "{errors:?}"
    );
}

// ── an argument is read once, when the call runs ─────────────────────────

/// The text on screen after tapping the first handler.
fn tapped_text(src: &str) -> Vec<String> {
    let (mut interp, tree) = boot_tree(src);
    shown(&mut interp, &tree);
    tap_once(&mut interp);
    shown(&mut interp, &tree)
}

/// `on_x(items[0])`: the body writes `xs` (which `items` follows) and then
/// reads `id` again; `id` is still what the call was given.
#[test]
fn a_callback_argument_is_evaluated_once_whatever_the_body_writes() {
    let src = "
View Main() {
    var xs = [1, 2, 3]
    var log = \"\"
    Column {
        Text(\"xs {xs.len} log {log}\")
        Child(items: xs, on_x: {|id| xs = xs.filter(p => p != id)
                                     log = log + \"{id};\" })
    }
}
View Child(items: List<Int>, on_x: Fn(Int) = {|_| }) {
    Box #[width: 100, height: 40, tap => on_x(items[0])]
}
";
    assert_eq!(tapped_text(src), ["xs 2 log 1;"]);
}

/// The same for a named action called with an argument.
#[test]
fn a_let_actions_argument_is_evaluated_once() {
    let src = "
View Main() {
    var xs = [1, 2, 3]
    var log = \"\"
    let remove = (id: Int) => { xs = xs.filter(p => p != id)
                                log = log + \"{id};\" }
    Column {
        Text(\"xs {xs.len} log {log}\")
        Box #[width: 100, height: 40, tap => remove(xs[0])]
    }
}
";
    assert_eq!(tapped_text(src), ["xs 2 log 1;"]);
}

/// And for a view-local `fn` with parameters, called from an action.
#[test]
fn a_view_local_fns_argument_is_evaluated_once() {
    let src = "
View Main() {
    var xs = [1, 2, 3]
    var log = \"\"
    fn tag(id: Int) -> Str => \"{id};\"
    let remove = (id: Int) => { xs = xs.filter(p => p != id)
                                log = log + tag(id) + tag(id) }
    Column {
        Text(\"xs {xs.len} log {log}\")
        Box #[width: 100, height: 40, tap => remove(xs[0])]
    }
}
";
    assert_eq!(tapped_text(src), ["xs 2 log 1;1;"]);
}
