//! User-view instantiation: argument binding, body expansion, recursion limits,
//! hot reload, slots, defaults and the first render trees.

use super::*;
use crate::interp::eval::instance::MAX_INSTANCE_DEPTH;

// ── argument → parameter binding ────────────────────────────────

/// Parses `callee_src` (a single view) and a call element from `call_src`'s
/// first view body, returning `(callee, call_element)`.
fn callee_and_call(callee_src: &str, call_src: &str) -> (ViewDecl, ElementNode) {
    let callee = parse(callee_src).views.into_iter().next().unwrap();
    let host = parse(call_src).views.into_iter().next().unwrap();
    let Member::Element(call) = host.body.into_iter().next().unwrap() else {
        panic!("expected element")
    };
    (callee, call)
}

#[test]
fn named_positional_and_mixed_binding() {
    let (callee, _) = callee_and_call(
        "View Avatar(url, size) { Text(url) }",
        "View H() { Text(\"x\") }",
    );
    // Named.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call(
        "View Avatar(url, size) { Text(url) }",
        "View H() { Avatar(url: \"a.png\", size: 40) }",
    );
    let b = interp.bind_args(&callee, &call);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(b.bindings.len(), 2);
    assert_eq!(b.bindings[0].0.as_str(), "url");
    assert_eq!(b.bindings[1].0.as_str(), "size");

    // Positional.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call(
        "View Avatar(url, size) { Text(url) }",
        "View H() { Avatar(\"a.png\", 40) }",
    );
    let b = interp.bind_args(&callee, &call);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(b.bindings.len(), 2);

    // Mixed: positional then named.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call(
        "View Avatar(url, size) { Text(url) }",
        "View H() { Avatar(\"a.png\") #[size: 40] }",
    );
    let b = interp.bind_args(&callee, &call);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(b.bindings.len(), 2);
}

#[test]
fn arity_unknown_duplicate_and_missing_diagnostics() {
    let (callee, _) = callee_and_call("View A(x, y) { Text(x) }", "View H() { Text(\"_\") }");

    // Over-arity: 3 positional for 2 params.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View A(x, y) { Text(x) }", "View H() { A(1, 2, 3) }");
    interp.bind_args(&callee, &call);
    assert!(interp.errors().iter().any(|e| matches!(
        e,
        CompileError::ViewArityMismatch {
            expected: 2,
            found: 3,
            ..
        }
    )));

    // Unknown named param.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View A(x, y) { Text(x) }", "View H() { A(z: 1) }");
    interp.bind_args(&callee, &call);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::UnknownParam { .. }))
    );

    // Duplicate: positional + named bind the same param.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View A(x, y) { Text(x) }", "View H() { A(1) #[x: 2] }");
    interp.bind_args(&callee, &call);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::DuplicateParam { .. }))
    );

    // Missing required.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View A(x, y) { Text(x) }", "View H() { A(1) }");
    interp.bind_args(&callee, &call);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::MissingParam { name, .. } if name == "y"))
    );
}

#[test]
fn parent_var_arg_is_a_live_memo_literal_is_constant() {
    // The parent declares `var n = 1`; the call passes `n` to a parameter.
    // The projecting memo tracks the parent signal: writing `n` and ticking
    // changes the memo's value (dirty edge preserved).
    let callee = parse("View Foo(v) { Text(\"{v}\") }")
        .views
        .into_iter()
        .next()
        .unwrap();
    let mut interp = Interpreter::new();
    let init = Expr::IntLit(1, crate::diagnostics::Span::new(0, 1));
    let n = interp.define_var(Symbol::intern("n"), &init);

    let (_, call) = callee_and_call("View Foo(v) { Text(\"{v}\") }", "View H() { Foo(n) }");
    let b = interp.bind_args(&callee, &call);
    let memo = b.bindings[0].1;
    interp.tick();
    assert_eq!(interp.read_memo(memo), Value::Int(1));
    interp.write_var(n, Value::Int(7));
    interp.tick();
    assert_eq!(
        interp.read_memo(memo),
        Value::Int(7),
        "memo tracks the parent var"
    );

    // A literal argument is a constant memo: it never changes.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View Foo(v) { Text(\"{v}\") }", "View H() { Foo(5) }");
    let b = interp.bind_args(&callee, &call);
    let memo = b.bindings[0].1;
    interp.tick();
    assert_eq!(interp.read_memo(memo), Value::Int(5));
}

// ── body expansion & per-instance scope ─────────────────────────

/// The string value of a `Text` node's content scope, after a tick.
fn text_value(interp: &mut Interpreter, node: &RenderNode) -> String {
    let RenderNode::Text { content, .. } = node else {
        panic!("expected Text node, got {node:?}");
    };
    interp.tick();
    match interp.binding_value(*content) {
        Some(Value::Str(s)) => s,
        other => panic!("expected Str binding, got {other:?}"),
    }
}

#[test]
fn user_view_expands_body_and_binds_a_parameter() {
    // `App` calls `Greet("Ada")`; the call expands to the callee body with
    // `name` bound, projecting "Hi Ada".
    let (mut interp, tree) = lower_named(
        "View Greet(name) { Text(\"Hi {name}\") }\nView App() { Greet(\"Ada\") }",
        "App",
    );
    assert_eq!(tree.len(), 1, "one spliced root");
    assert_eq!(text_value(&mut interp, &tree[0]), "Hi Ada");
}

#[test]
fn user_view_passes_value_to_an_intrinsic() {
    // `Avatar(url, size)` lowers `Image(url) #[width: size]`; the call's
    // arguments flow through to the intrinsic node.
    let (_interp, tree) = lower_named(
        "View Avatar(url, size) { Image(url) #[width: size, height: size] }\n\
             View App() { Avatar(\"ada.png\", 40) }",
        "App",
    );
    assert_eq!(tree.len(), 1);
    assert!(
        matches!(&tree[0], RenderNode::Image { .. }),
        "expected an Image node, got {:?}",
        tree[0]
    );
}

#[test]
fn a_call_yielding_multiple_roots_splices_as_siblings() {
    let (_interp, tree) = lower_named(
        "View Pair() { Text(\"a\")\n Text(\"b\") }\nView App() { Pair() }",
        "App",
    );
    assert_eq!(tree.len(), 2, "both callee roots spliced as siblings");
}

#[test]
fn nested_user_view_calls_expand() {
    // App → Outer → Inner → Text("x").
    let (mut interp, tree) = lower_named(
        "View Inner() { Text(\"x\") }\n\
             View Outer() { Inner() }\n\
             View App() { Outer() }",
        "App",
    );
    assert_eq!(tree.len(), 1);
    assert_eq!(text_value(&mut interp, &tree[0]), "x");
}

#[test]
fn two_instances_keep_independent_local_state() {
    // Two `Counter()` instances each lower their own `var n`; their content
    // scopes are distinct bindings (independent per-instance state).
    let (_interp, tree) = lower_named(
        "View Counter() { var n = 0\n Text(\"{n}\") }\n\
             View App() { Column { Counter()\n Counter() } }",
        "App",
    );
    // App → Column(Box) containing two expanded Counters.
    let RenderNode::Box { children, .. } = &tree[0] else {
        panic!("expected a Column box, got {:?}", tree[0]);
    };
    let texts: Vec<&RenderNode> = children
        .iter()
        .filter(|c| matches!(c, RenderNode::Text { .. }))
        .collect();
    assert_eq!(texts.len(), 2, "two independent Counter texts");
    let scopes: Vec<ScopeId> = texts
        .iter()
        .map(|t| match t {
            RenderNode::Text { content, .. } => *content,
            _ => unreachable!(),
        })
        .collect();
    assert_ne!(scopes[0], scopes[1], "each instance has its own binding");
}

#[test]
fn two_level_composition_golden_shape() {
    // UserRow composes Avatar + Text inside a Row; App stacks two UserRows.
    let (_interp, tree) = lower_named(
        "View Avatar(url) { Image(url) }\n\
             View UserRow(name, avatar) { Row { Avatar(avatar)\n Text(name) } }\n\
             View App() { Column { UserRow(\"Ada\", \"ada.png\")\n UserRow(\"Alan\", \"alan.png\") } }",
        "App",
    );
    // App → Column(Box) → [Row(Box)[Image, Text], Row(Box)[Image, Text]].
    let RenderNode::Box { children, .. } = &tree[0] else {
        panic!("expected Column");
    };
    assert_eq!(children.len(), 2, "two UserRow instances");
    for row in children {
        let RenderNode::Box { children: rc, .. } = row else {
            panic!("expected Row");
        };
        assert!(matches!(rc[0], RenderNode::Image { .. }));
        assert!(matches!(rc[1], RenderNode::Text { .. }));
    }
}

// ── recursion & cycle protection ────────────────────────────────

#[test]
fn unguarded_self_call_is_recursive_view_at_load() {
    let parsed = parse("View A() { A() }");
    let mut interp = Interpreter::new();
    let diags = interp.load_views(&parsed.views);
    assert!(
        diags
            .iter()
            .any(|d| matches!(d, CompileError::RecursiveView { .. })),
        "expected RecursiveView at load, got {diags:?}"
    );
}

#[test]
fn guarded_recursion_that_terminates_is_legal() {
    // RFC-0018: `Tree` recurses only in the `else` of a guard that is true, so
    // the recursive branch is never lowered (lazy `when`), it renders to a
    // finite depth with no diagnostic.
    let (mut interp, tree) = lower_named(
        "View Tree() { var leaf = true\n when leaf { Text(\"x\") } else { Tree() } }\n\
             View App() { Tree() }",
        "App",
    );
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        interp.errors().is_empty(),
        "guarded terminating recursion is legal: {:?}",
        interp.errors()
    );
    assert_eq!(frame.texts().len(), 1);
    assert_eq!(frame.texts()[0].text, "x");
}

#[test]
fn runaway_guarded_recursion_hits_depth_bound_without_panicking() {
    // `go` is always true, so the guard never terminates at lower time. The
    // static check does not flag it (the cycle is guarded), so the runtime
    // depth bound must stop it with a diagnostic, not a stack overflow.
    let parsed = parse("View Loop() { var go = true\n when go { Loop() } }\nView App() { Loop() }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let load_diags = interp.load_views(&parsed.views);
    assert!(
        !load_diags
            .iter()
            .any(|d| matches!(d, CompileError::RecursiveView { .. })),
        "a guarded cycle is not a static error"
    );
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let app = parsed
        .views
        .iter()
        .find(|v| v.name.as_str() == "App")
        .unwrap();
    let tree = interp.lower_view(app, &known); // lazy: no recursion at lower
    // RFC-0018: the recursion now unrolls at render (reconcile) time, one
    // level per frame (a freshly-lowered `go` reads false until the next
    // pull), so each render is finite (no stack overflow). Over enough frames
    // the reconcile depth bound stops it with a diagnostic.
    let mut hit = false;
    for _ in 0..(MAX_INSTANCE_DEPTH + 8) {
        interp.tick();
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0); // must not overflow
        if interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::RecursiveView { .. }))
        {
            hit = true;
            break;
        }
    }
    assert!(
        hit,
        "the reconcile depth bound must stop the runaway recursion"
    );
}

// ── hot-reload across instances ─────────────────────────────────

#[test]
fn reloading_a_leaf_view_updates_all_its_instances() {
    use crate::interp::reload::{affected_views, diff_view};

    let old = parse("View Leaf() { Text(\"old\") }\nView App() { Column { Leaf()\n Leaf() } }");
    let new = parse("View Leaf() { Text(\"new\") }\nView App() { Column { Leaf()\n Leaf() } }");

    let mut interp = Interpreter::new();
    interp.load_views(&old.views);
    let known_old: Vec<&str> = old.views.iter().map(|v| v.name.as_str()).collect();
    let app_old = old.views.iter().find(|v| v.name.as_str() == "App").unwrap();
    let tree = interp.lower_view(app_old, &known_old);
    let RenderNode::Box { children, .. } = &tree[0] else {
        panic!("expected Column");
    };
    assert_eq!(text_value(&mut interp, &children[0]), "old");

    // The edit to the leaf transitively affects App (RFC-0007 §5).
    let affected = affected_views(&old.views, &new.views);
    assert!(affected.contains(&Symbol::intern("App")));

    // Rebuild the registry and re-derive App; both Leaf instances update.
    interp.load_views(&new.views);
    let app_new = new.views.iter().find(|v| v.name.as_str() == "App").unwrap();
    interp.reload(app_new, diff_view(app_old, app_new));
    let known_new: Vec<&str> = new.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(app_new, &known_new);
    let RenderNode::Box { children, .. } = &tree[0] else {
        panic!("expected Column");
    };
    assert_eq!(text_value(&mut interp, &children[0]), "new");
    assert_eq!(text_value(&mut interp, &children[1]), "new");
}

// ── slots & parameter defaults ──────────────────────────────────

#[test]
fn omitted_defaulted_param_uses_its_default() {
    // `label` is omitted; the default "?" is evaluated in the callee scope.
    let (mut interp, tree) = lower_named(
        "View Tag(label = \"?\") { Text(label) }\nView App() { Tag() }",
        "App",
    );
    assert!(
        interp.errors().is_empty(),
        "a defaulted param is not required: {:?}",
        interp.errors()
    );
    assert_eq!(text_value(&mut interp, &tree[0]), "?");
}

#[test]
fn supplied_argument_overrides_the_default() {
    let (mut interp, tree) = lower_named(
        "View Tag(label = \"?\") { Text(label) }\nView App() { Tag(\"hi\") }",
        "App",
    );
    assert_eq!(text_value(&mut interp, &tree[0]), "hi");
}

#[test]
fn missing_param_only_fires_for_required_params() {
    // `a` is required, `b` is defaulted; omitting both reports only `a`.
    let (callee, _) = callee_and_call("View V(a, b = 1) { Text(a) }", "View H() { Text(\"_\") }");
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View V(a, b = 1) { Text(a) }", "View H() { V() }");
    interp.bind_args(&callee, &call);
    let missing: Vec<&str> = interp
        .errors()
        .iter()
        .filter_map(|e| match e {
            CompileError::MissingParam { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(missing, vec!["a"], "only the required param is missing");
}

#[test]
fn content_slot_renders_the_passed_block() {
    // `Card` declares a `content` slot; `App` passes a `Text` block, which is
    // spliced where `content` appears inside the card body.
    let (mut interp, tree) = lower_named(
        "View Card(content) { Column { content } }\n\
             View App() { Card { Text(\"inside\") } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let RenderNode::Box { children, .. } = &tree[0] else {
        panic!("expected the card's Column, got {:?}", tree[0]);
    };
    assert_eq!(children.len(), 1, "the passed block is spliced");
    assert_eq!(text_value(&mut interp, &children[0]), "inside");
}

#[test]
fn block_passed_to_a_slotless_view_is_unexpected_children() {
    let (interp, _tree) = lower_named(
        "View Plain() { Text(\"x\") }\nView App() { Plain { Text(\"no\") } }",
        "App",
    );
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::UnexpectedChildren { .. })),
        "expected UnexpectedChildren, got {:?}",
        interp.errors()
    );
}

#[test]
fn slot_block_captures_the_caller_scope() {
    // The block passed to `Card` reads the *caller's* `name` var, proving the
    // slot is lowered in the caller scope (not the callee's).
    let (mut interp, tree) = lower_named(
        "View Card(content) { content }\n\
             View App() { var name = \"Ada\"\n Card { Text(\"Hi {name}\") } }",
        "App",
    );
    assert_eq!(text_value(&mut interp, &tree[0]), "Hi Ada");
}

#[test]
fn var_text_binding_updates_after_mutation_and_tick() {
    let parsed =
        parse("View C() {\n var count = 0\n Text(\"{count}\")\n Button(\"+\") => count++\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];

    let mut interp = Interpreter::new();
    let Member::Var { name, init, .. } = &view.body[0] else {
        panic!("expected var");
    };
    interp.define_var(name.clone(), init);

    let text = element(&view.body[1]);
    let bind = interp.bind_value(&text.content[0].value);
    interp.tick();
    assert_eq!(
        interp.binding_value(bind),
        Some(Value::Str("0".to_string()))
    );

    // The Button's `=> count++` action.
    let action = element(&view.body[2]).action.as_ref().unwrap();
    interp.eval_action(action).unwrap();
    interp.tick();
    assert_eq!(
        interp.binding_value(bind),
        Some(Value::Str("1".to_string()))
    );
}

#[test]
fn let_memo_recomputes_when_its_source_changes() {
    let parsed = parse(
        "View C() {\n var count = 0\n let doubled = count\n Text(\"{doubled}\")\n Button(\"+\") => count++\n}",
    );
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();

    let Member::Var { name, init, .. } = &view.body[0] else {
        panic!()
    };
    interp.define_var(name.clone(), init);
    let Member::Let { name, init, .. } = &view.body[1] else {
        panic!()
    };
    let memo = interp.define_let(name.clone(), init);

    let text = element(&view.body[2]);
    let bind = interp.bind_value(&text.content[0].value);
    interp.tick();
    assert_eq!(
        interp.binding_value(bind),
        Some(Value::Str("0".to_string()))
    );
    let evals = interp.ctx().eval_count(memo);

    let action = element(&view.body[3]).action.as_ref().unwrap();
    interp.eval_action(action).unwrap();
    interp.tick();
    assert_eq!(
        interp.binding_value(bind),
        Some(Value::Str("1".to_string()))
    );
    assert!(interp.ctx().eval_count(memo) > evals, "memo recomputed");
}

#[test]
fn assignment_to_a_let_is_not_assignable() {
    let parsed = parse("View C() {\n var count = 0\n let y = count\n Button(\"x\") => y = 5\n}");
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();

    let Member::Var { name, init, .. } = &view.body[0] else {
        panic!()
    };
    interp.define_var(name.clone(), init);
    let Member::Let { name, init, .. } = &view.body[1] else {
        panic!()
    };
    interp.define_let(name.clone(), init);

    let action = element(&view.body[2]).action.as_ref().unwrap();
    let err = interp.eval_action(action).unwrap_err();
    assert!(matches!(err, CompileError::NotAssignable { .. }));
}

#[test]
fn lower_view_emits_expected_render_tree() {
    let parsed = parse(
        "View C() {\n var count = 0\n Column #[bg: 0x222222, radius: 16] {\n Text(\"Count: {count}\")\n }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];

    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    // One top-level Column box with the literal bg/radius and one Text child.
    assert_eq!(tree.len(), 1);
    let RenderNode::Box {
        attrs, children, ..
    } = &tree[0]
    else {
        panic!("expected a Box, got {:?}", tree[0]);
    };
    let bg = interp.eval_color_prop(attrs, "bg");
    let radius = interp.eval_int_prop(attrs, "radius");
    assert_eq!(bg, Some(0x0022_2222));
    assert_eq!(radius, Some(16));
    assert_eq!(children.len(), 1);
    let RenderNode::Text { content, .. } = &children[0] else {
        panic!("expected a Text child");
    };

    // The Text projects the reactive count.
    interp.tick();
    assert_eq!(
        interp.binding_value(*content),
        Some(Value::Str("Count: 0".to_string()))
    );
}

#[test]
fn lowering_an_unknown_element_records_unknown_view() {
    let parsed = parse("View C() { Colunm #[gap: 8] {} }");
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let _ = interp.lower_view(view, &[]);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::UnknownView { .. }))
    );
}

#[test]
fn mutation_on_an_undeclared_name_is_not_assignable() {
    let parsed = parse("View C() { Button(\"x\") => ghost++ }");
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let action = element(&view.body[0]).action.as_ref().unwrap();
    assert!(matches!(
        interp.eval_action(action).unwrap_err(),
        CompileError::NotAssignable { .. }
    ));
}

#[test]
fn spacing_convenience_parses_correctly() {
    use byard_core::atlas::layout::Spacing;

    let test_cases = vec![
        // 1-value positional
        ("View C() { Column #[p: (10)] {} }", Spacing::all(10.0)),
        // 2-value positional
        (
            "View C() { Column #[p: (2, 3)] {} }",
            Spacing::symmetric(2.0, 3.0),
        ),
        // 4-value positional: CSS order top, right, bottom, left
        (
            "View C() { Column #[p: (1, 2, 3, 4)] {} }",
            Spacing {
                top: 1.0,
                right: 2.0,
                bottom: 3.0,
                left: 4.0,
            },
        ),
        // Named top only, unspecified sides default to 0
        (
            "View C() { Column #[p: (top: 10)] {} }",
            Spacing {
                top: 10.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0,
            },
        ),
        // Named bottom only
        (
            "View C() { Column #[p: (bottom: 7)] {} }",
            Spacing {
                top: 0.0,
                right: 0.0,
                bottom: 7.0,
                left: 0.0,
            },
        ),
        // Named mixed sides
        (
            "View C() { Column #[p: (left: 5, bottom: 3)] {} }",
            Spacing {
                top: 0.0,
                right: 0.0,
                bottom: 3.0,
                left: 5.0,
            },
        ),
        // Verbose axis shorthands (the only accepted shorthands)
        (
            "View C() { Column #[p: (horizontal: 10, vertical: 5)] {} }",
            Spacing {
                top: 5.0,
                right: 10.0,
                bottom: 5.0,
                left: 10.0,
            },
        ),
    ];

    for (source, expected_spacing) in test_cases {
        let parsed = parse(source);
        assert!(
            parsed.errors.is_empty(),
            "Failed to parse: {}\nErrors: {:?}",
            source,
            parsed.errors
        );
        let view = &parsed.views[0];
        let mut interp = Interpreter::new();
        let tree = interp.lower_view(view, &[]);
        let RenderNode::Box { name, attrs, .. } = &tree[0] else {
            panic!("expected a Box");
        };
        let style = interp.eval_container_style(name.as_str(), attrs);
        assert_eq!(
            style.padding.top, expected_spacing.top,
            "top mismatch for source: {}",
            source
        );
        assert_eq!(
            style.padding.right, expected_spacing.right,
            "right mismatch for source: {}",
            source
        );
        assert_eq!(
            style.padding.bottom, expected_spacing.bottom,
            "bottom mismatch for source: {}",
            source
        );
        assert_eq!(
            style.padding.left, expected_spacing.left,
            "left mismatch for source: {}",
            source
        );
    }
}

#[test]
fn individual_margin_padding_properties_override() {
    use byard_core::atlas::layout::Spacing;

    let parsed = parse("View C() { Column #[p: (10), pt: 2, pb: 4, ml: 5, mt: 1] {} }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    let RenderNode::Box { name, attrs, .. } = &tree[0] else {
        panic!("expected a Box");
    };
    let style = interp.eval_container_style(name.as_str(), attrs);
    // padding.top overridden to 2, padding.bottom overridden to 4, others stay 10
    assert_eq!(
        style.padding,
        Spacing {
            top: 2.0,
            right: 10.0,
            bottom: 4.0,
            left: 10.0
        }
    );
    // margins
    assert_eq!(
        style.margin,
        Spacing {
            top: 1.0,
            right: 0.0,
            bottom: 0.0,
            left: 5.0
        }
    );
}
