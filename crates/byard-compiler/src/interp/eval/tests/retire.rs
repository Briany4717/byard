//! Scopes, pools and rows given back when a screen, a branch or a row goes away
//! (RFC-0018, RFC-0026): the arena stays the size of what is on screen, and
//! giving a slot back never lets a stale id reach the scope that took it.

use super::*;
use crate::interp::env::Value;
use byard_core::platform::EventKind as K;

const W: f32 = 400.0;
const H: f32 = 400.0;

fn render(interp: &mut Interpreter, tree: &[RenderNode]) -> Vec<String> {
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(tree, &mut frame, W, H);
    frame.texts().iter().map(|t| t.text.clone()).collect()
}

fn set(interp: &mut Interpreter, name: &str, value: Value) {
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern(name))
        .unwrap_or_else(|| panic!("no `var {name}`"));
    interp.write_var(sig, value);
    interp.tick();
}

fn get(interp: &Interpreter, name: &str) -> Value {
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern(name))
        .unwrap_or_else(|| panic!("no `var {name}`"));
    interp.peek(sig)
}

fn nav(interp: &mut Interpreter, tree: &[RenderNode], path: &str) -> Vec<String> {
    set(interp, "navPath", Value::Str(path.to_string()));
    render(interp, tree)
}

fn strs(items: &[&str]) -> Value {
    Value::List(items.iter().map(|s| Value::Str((*s).to_string())).collect())
}

/// A home screen and a detail screen that opens a scope of every kind a route
/// can: a `let`, an argument memo, a `when` and a `for` with their branches and
/// rows, and a lifecycle effect.
const APP: &str = r#"
View Detail(id: Str) {
    let label = "item {id}"
    var on = true
    var parts = ["a", "b"]
    on mount => { seen = label }
    on unmount => { gone = label }
    Column {
        Text(label)
        when on { Text("on {id}") } else { Text("off {id}") }
        for p in parts { Text("{p}{id}") }
    }
}
View App() {
    var navPath = "/"
    var seen = ""
    var gone = ""
    NavStack(path: navPath) {
        route "/" { Text("home") }
        route "/detail/:id" {|p| Detail(id: p.id) }
    }
}
"#;

fn app() -> (Interpreter, Vec<RenderNode>) {
    let (mut interp, tree) = lower_named(APP, "App");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    render(&mut interp, &tree);
    (interp, tree)
}

#[test]
fn popping_a_route_gives_every_scope_it_opened_back() {
    let (mut interp, tree) = app();
    let at_home = interp.live_scope_count();

    nav(&mut interp, &tree, "/detail/1");
    let on_detail = interp.live_scope_count();
    assert!(
        on_detail > at_home,
        "the detail screen opens scopes of its own"
    );
    nav(&mut interp, &tree, "/");
    assert_eq!(interp.live_scope_count(), at_home, "popped: all given back");
    let slots = interp.scope_slot_count();

    for i in 0..10 {
        let texts = nav(&mut interp, &tree, "/detail/2");
        assert_eq!(texts[0], "item 2", "cycle {i}");
        assert_eq!(interp.live_scope_count(), on_detail, "cycle {i}: same size");
        let texts = nav(&mut interp, &tree, "/");
        assert_eq!(texts, ["home"], "cycle {i}");
        assert_eq!(interp.live_scope_count(), at_home, "cycle {i}: back home");
    }
    assert_eq!(
        interp.scope_slot_count(),
        slots,
        "ten more visits reused the slots the first one freed"
    );
}

#[test]
fn the_pools_inside_a_popped_route_are_reused_too() {
    let (mut interp, tree) = app();
    nav(&mut interp, &tree, "/detail/1");
    nav(&mut interp, &tree, "/");
    let (whens, fors) = (interp.when_pools.len(), interp.for_pools.len());
    assert!(
        whens > 0 && fors > 0,
        "the detail screen has a when and a for"
    );
    for _ in 0..10 {
        nav(&mut interp, &tree, "/detail/3");
        nav(&mut interp, &tree, "/");
    }
    assert_eq!(
        interp.when_pools.len(),
        whens,
        "when pools do not accumulate"
    );
    assert_eq!(interp.for_pools.len(), fors, "for pools do not accumulate");
}

#[test]
fn a_popped_screens_unmount_still_reads_its_own_lets() {
    // The scopes are released after the effects settle, not before: this reads
    // `label` (a `let`) from the screen that is being popped.
    let (mut interp, tree) = app();
    nav(&mut interp, &tree, "/detail/7");
    assert_eq!(get(&interp, "seen"), Value::Str("item 7".into()));
    nav(&mut interp, &tree, "/");
    assert_eq!(get(&interp, "gone"), Value::Str("item 7".into()));
}

#[test]
fn a_screen_visited_again_shows_its_own_values_not_a_previous_visits() {
    let (mut interp, tree) = app();
    for id in ["1", "22", "333"] {
        let texts = nav(&mut interp, &tree, &format!("/detail/{id}"));
        assert_eq!(
            texts,
            [
                format!("item {id}"),
                format!("on {id}"),
                format!("a{id}"),
                format!("b{id}")
            ]
        );
        nav(&mut interp, &tree, "/");
    }
}

#[test]
fn a_for_that_shrinks_a_long_way_releases_its_rows() {
    let (mut interp, tree) = lower_named(
        "View App() {
            var items = []
            Column { for it in items { Text(\"row {it}\") } }
        }",
        "App",
    );
    interp.tick();
    let many: Vec<Value> = (0..1000)
        .map(|i| Value::Str(format!("long item number {i}")))
        .collect();
    set(&mut interp, "items", Value::List(many));
    let texts = render(&mut interp, &tree);
    assert_eq!(texts.len(), 1000);
    let peak = interp.live_scope_count();
    let slots = interp.for_pools[0].item_slots.clone();
    assert_eq!(slots.len(), 1000);

    set(&mut interp, "items", strs(&["only"]));
    assert_eq!(render(&mut interp, &tree), ["row only"]);

    // P1-03: the element values are let go the moment the rows leave...
    for slot in &slots[1..] {
        assert_eq!(
            interp.peek(*slot),
            Value::Unit,
            "a dead row's value is released"
        );
    }
    // ...and the bodies beyond a small bound are not kept lowered.
    assert!(
        interp.for_pools[0].bodies.len() <= 32,
        "kept {} bodies for a list of 1",
        interp.for_pools[0].bodies.len()
    );
    assert_eq!(
        interp.for_pools[0].slot_scopes.len(),
        interp.for_pools[0].bodies.len()
    );
    assert!(
        interp.live_scope_count() < peak / 10,
        "the trimmed rows gave their scopes back: {} of {peak}",
        interp.live_scope_count()
    );

    // Growing back lowers the rows again, with the same signals and the right text.
    let many: Vec<Value> = (0..1000)
        .map(|i| Value::Str(format!("again {i}")))
        .collect();
    set(&mut interp, "items", Value::List(many));
    let texts = render(&mut interp, &tree);
    assert_eq!(texts.len(), 1000);
    assert_eq!(texts[0], "row again 0");
    assert_eq!(texts[999], "row again 999");
    assert_eq!(interp.for_pools[0].item_slots, slots, "no new slot signals");
    assert_eq!(interp.live_scope_count(), peak);
}

#[test]
fn a_for_that_wobbles_keeps_its_rows_lowered() {
    let (mut interp, tree) = lower_named(
        "View App() {
            var items = []
            Column { for it in items { Text(\"row {it}\") } }
        }",
        "App",
    );
    interp.tick();
    for n in [90, 100, 90, 60, 95] {
        let items: Vec<Value> = (0..n).map(|i| Value::Str(format!("{i}"))).collect();
        set(&mut interp, "items", Value::List(items));
        assert_eq!(render(&mut interp, &tree).len(), n);
    }
    assert_eq!(
        interp.for_pools[0].bodies.len(),
        100,
        "grew once to the high-water mark and was never trimmed"
    );
}

/// What a retired slot can do to a screen that is not done with it: `when`,
/// `for` shrinking and growing, and a route pushed and popped, in a fixed
/// pseudo-random order, with the rendered text checked against a model after
/// every step. A stale id firing a recycled scope would show up here as a row
/// or a branch showing another's text.
#[test]
fn a_mix_of_when_for_and_routes_keeps_every_screen_right() {
    let src = r#"
View Item(id: Str) {
    let tag = "item-{id}"
    when show { Text("{tag}!") } else { Text("{tag}?") }
    for n in tags { Text("{tag}:{n}") }
}
View App() {
    var show = true
    var tags = ["x"]
    var rows = ["a", "b", "c"]
    var navPath = "/"
    Column {
        for r in rows { Text("row {r}") }
        NavStack(path: navPath) {
            route "/" { Text("home") }
            route "/detail/:id" {|p| Item(id: p.id) }
        }
    }
}
"#;
    let (mut interp, tree) = lower_named(src, "App");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();

    let mut show = true;
    let mut tags: Vec<String> = vec!["x".into()];
    let mut rows: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
    let mut path = "/".to_string();

    let mut seed = 0x2545_f491_u32;
    let mut next = move |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as usize % n
    };

    let mut settled_slots = 0;
    for step in 0..400 {
        match next(5) {
            0 => {
                show = !show;
                set(&mut interp, "show", Value::Bool(show));
            }
            1 => {
                let n = [0, 1, 3, 40, 2][next(5)];
                tags = (0..n).map(|i| format!("t{i}")).collect();
                set(
                    &mut interp,
                    "tags",
                    Value::List(tags.iter().cloned().map(Value::Str).collect()),
                );
            }
            2 => {
                let n = [0, 1, 5, 200, 2, 3][next(6)];
                rows = (0..n).map(|i| format!("r{i}")).collect();
                set(
                    &mut interp,
                    "rows",
                    Value::List(rows.iter().cloned().map(Value::Str).collect()),
                );
            }
            _ => {
                path = if next(3) == 0 {
                    "/".into()
                } else {
                    format!("/detail/{}", next(4))
                };
                set(&mut interp, "navPath", Value::Str(path.clone()));
            }
        }
        let mut want: Vec<String> = rows.iter().map(|r| format!("row {r}")).collect();
        if let Some(id) = path.strip_prefix("/detail/") {
            want.push(format!("item-{id}{}", if show { "!" } else { "?" }));
            want.extend(tags.iter().map(|t| format!("item-{id}:{t}")));
        } else {
            want.push("home".into());
        }
        assert_eq!(
            render(&mut interp, &tree),
            want,
            "step {step}: {path} show={show}"
        );
        if step == 200 {
            settled_slots = interp.scope_slot_count();
        }
    }
    // The arena settled: the last two hundred steps visited the same screens.
    assert!(
        interp.scope_slot_count() <= settled_slots + 40,
        "the arena kept growing: {settled_slots} -> {}",
        interp.scope_slot_count()
    );
}

#[test]
fn the_arguments_of_a_called_fn_do_not_add_a_scope_per_frame() {
    // A handler that calls a `fn` is lowered again every frame, and lowering the
    // call opens a scope for its argument.
    let (mut interp, tree) = lower_named(
        "View App() {
            var n = 0
            fn plus(k) => n + k
            Column {
                Button(\"Add\") #[width: 100, height: 40] => { n = plus(1) }
                Text(\"{n}\")
            }
        }",
        "App",
    );
    interp.tick();
    render(&mut interp, &tree);
    render(&mut interp, &tree);
    let (slots, live) = (interp.scope_slot_count(), interp.live_scope_count());
    for _ in 0..200 {
        render(&mut interp, &tree);
    }
    assert_eq!(interp.scope_slot_count(), slots, "the arena did not grow");
    assert_eq!(interp.live_scope_count(), live, "nor did the live set");

    // And the handler still works, frame after frame.
    for i in 1..=5 {
        interp.dispatch_events(&[
            pointer(K::PointerDown, 50.0, 20.0, 0),
            pointer(K::PointerUp, 50.0, 20.0, 10),
        ]);
        interp.tick();
        assert_eq!(render(&mut interp, &tree)[1], format!("{i}"));
    }
    assert_eq!(interp.scope_slot_count(), slots);
}
