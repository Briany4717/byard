//! RFC-0051, bindable parameters: a view declares `bind on: Bool`, the caller
//! passes a `var`, and the view reads and writes it as its own, with no copy
//! between the two.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::resolve::{PackageProvider, SourceFile, resolve_program};
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, InputPayload};

struct NoPackages;

impl PackageProvider for NoPackages {
    fn package_files(
        &mut self,
        _dependent: &str,
        _package: &str,
    ) -> Result<Vec<SourceFile>, String> {
        Err("no packages in this test".to_string())
    }
}

/// Everything `byard check` would say about `source`, as `kind: headline`.
fn check(source: &str) -> Vec<String> {
    resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: source.to_string(),
        }],
        &mut NoPackages,
    )
    .errors
    .iter()
    .map(|e| format!("{}: {}", e.kind(), e.headline()))
    .collect()
}

fn kinds(said: &[String]) -> Vec<&str> {
    said.iter()
        .map(|l| l.split(':').next().unwrap_or_default())
        .collect()
}

/// A program running headless: load it, tap it, read what it draws.
struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    t: u64,
}

impl App {
    fn start(source: &str) -> Self {
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let checked = byard_compiler::infer::check_views(&parsed.views).errors;
        assert!(checked.is_empty(), "the type checker: {checked:?}");
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let root = byard_compiler::parser::ast::root_view(&parsed.views).unwrap();
        let tree = interp.lower_view(root, &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut app = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
            t: 0,
        };
        app.send(&[]);
        app
    }

    fn send(&mut self, events: &[InputEvent]) {
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 600.0);
    }

    fn event(&mut self, kind: EventKind, pos: (f32, f32), payload: Option<InputPayload>) {
        self.t += 50;
        let e = InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload,
            time_ms: self.t,
            mods: byard_core::KeyModifiers::default(),
        };
        self.send(&[e]);
    }

    fn tap(&mut self, label: &str) {
        let line = self
            .frame
            .texts()
            .iter()
            .find(|t| t.text == label)
            .unwrap_or_else(|| panic!("a text {label}: {:?}", self.texts()));
        let pos = (line.x + 2.0, line.y + 4.0);
        self.event(EventKind::PointerDown, pos, None);
        self.event(EventKind::PointerUp, pos, None);
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.event(
                EventKind::TextInput,
                (0.0, 0.0),
                Some(InputPayload::Key(c.to_string())),
            );
        }
    }

    fn key(&mut self, key: &str) {
        self.event(
            EventKind::KeyDown,
            (0.0, 0.0),
            Some(InputPayload::Key(key.into())),
        );
    }

    fn texts(&self) -> Vec<String> {
        self.frame.texts().iter().map(|t| t.text.clone()).collect()
    }

    fn shows(&self, text: &str) -> bool {
        self.frame.texts().iter().any(|t| t.text == text)
    }
}

// ── the two-way case ────────────────────────────────────────────────────

const SWITCH: &str = r#"
View Switch(bind on: Bool, label: Str = "") {
    var seen = false
    Column {
        Button(label) => {
            on = !on
            seen = on
        }
        Text("child={on}")
        Text("seen={seen}")
    }
}
View Main() {
    var wifi = false
    Column {
        Switch(bind: wifi, label: "flip")
        Text("wifi={wifi}")
        Button("off") => wifi = false
        Button("on") => wifi = true
    }
}
"#;

#[test]
fn a_child_writes_the_callers_var_and_both_show_it_in_the_same_frame() {
    let mut app = App::start(SWITCH);
    assert!(
        app.shows("wifi=false") && app.shows("child=false"),
        "{:?}",
        app.texts()
    );

    app.tap("flip");
    // One frame after the tap: the caller's text, the child's, and a copy the
    // child made after writing, which a lagging inner copy would get wrong.
    assert!(app.shows("wifi=true"), "{:?}", app.texts());
    assert!(app.shows("child=true"), "{:?}", app.texts());
    assert!(app.shows("seen=true"), "{:?}", app.texts());

    app.tap("flip");
    assert!(
        app.shows("wifi=false") && app.shows("child=false"),
        "{:?}",
        app.texts()
    );
}

#[test]
fn the_caller_writing_its_var_changes_what_the_child_reads() {
    let mut app = App::start(SWITCH);
    app.tap("on");
    assert!(
        app.shows("wifi=true") && app.shows("child=true"),
        "{:?}",
        app.texts()
    );
    app.tap("off");
    assert!(
        app.shows("wifi=false") && app.shows("child=false"),
        "{:?}",
        app.texts()
    );
}

#[test]
fn two_calls_with_two_vars_are_two_cells_and_with_one_var_are_one() {
    let mut app = App::start(
        r#"
View Counter(bind n: Int, label: Str) {
    Button(label) => n++
}
View Main() {
    var a = 0
    var b = 0
    Column {
        Counter(bind: a, label: "A")
        Counter(bind: a, label: "A again")
        Counter(bind: b, label: "B")
        Text("a={a} b={b}")
    }
}
"#,
    );
    app.tap("A");
    app.tap("A again");
    app.tap("B");
    assert!(app.shows("a=2 b=1"), "{:?}", app.texts());
}

#[test]
fn a_view_with_the_same_name_as_the_callers_var_binds_the_callers() {
    // The argument is looked up where the call is written, not among the
    // callee's own parameters.
    let mut app = App::start(
        r#"
View Inner(bind n: Int) { Button("inc") => n++ }
View Main() {
    var n = 10
    Column { Inner(bind: n)  Text("n={n}") }
}
"#,
    );
    app.tap("inc");
    assert!(app.shows("n=11"), "{:?}", app.texts());
}

// ── several bindings ────────────────────────────────────────────────────

const TWO: &str = r#"
View Pair(bind first: Str, bind second: Int, label: Str) {
    Column {
        Button(label) => {
            first = first + "!"
            second += 2
        }
    }
}
View Main() {
    var s = "hi"
    var n = 0
    Column {
        Pair(label: "go", bind first: s, bind second: n)
        Text("{s} {n}")
    }
}
"#;

#[test]
fn two_bindings_are_named_and_each_writes_its_own_var() {
    let mut app = App::start(TWO);
    app.tap("go");
    assert!(app.shows("hi! 2"), "{:?}", app.texts());
    app.tap("go");
    assert!(app.shows("hi!! 4"), "{:?}", app.texts());
}

#[test]
fn the_bindings_may_be_given_in_any_order_and_the_long_form_is_for_one_too() {
    let mut app = App::start(
        r#"
View Pair(bind a: Int, bind b: Int) { Button("go") => { a = 1
    b = 2 } }
View One(bind a: Int) { Button("one") => a = 7 }
View Main() {
    var x = 0
    var y = 0
    var z = 0
    Column {
        Pair(bind b: x, bind a: y)
        One(bind a: z)
        Text("{x} {y} {z}")
    }
}
"#,
    );
    app.tap("go");
    app.tap("one");
    assert!(app.shows("2 1 7"), "{:?}", app.texts());
}

// ── forwarding ──────────────────────────────────────────────────────────

#[test]
fn a_binding_forwarded_through_three_views_writes_the_first_ones_var() {
    let mut app = App::start(
        r#"
View Outer() {
    var on = false
    Column {
        Middle(bind: on)
        Text("outer={on}")
    }
}
View Middle(bind flag: Bool) {
    Column {
        Inner(bind flag: flag)
        Text("middle={flag}")
    }
}
View Inner(bind flag: Bool) {
    Column {
        Button("flip") => flag = !flag
        Text("inner={flag}")
    }
}
View Main() { Outer() }
"#,
    );
    assert!(app.shows("outer=false"), "{:?}", app.texts());
    app.tap("flip");
    for line in ["outer=true", "middle=true", "inner=true"] {
        assert!(app.shows(line), "{line}: {:?}", app.texts());
    }
}

// ── a binding given to a control ────────────────────────────────────────

#[test]
fn a_binding_is_bound_further_to_a_control() {
    let mut app = App::start(
        r#"
View Field(bind text: Str) {
    TextField #[bind: text, width: 200, height: 36]
}
View Main() {
    var name = "ab"
    Column {
        Field(bind: name)
        Text("name={name}")
        Button("clear") => name = ""
    }
}
"#,
    );
    // The caller's text is in the field.
    assert!(app.shows("ab"), "{:?}", app.texts());
    app.tap("ab");
    app.key("End");
    app.type_text("c");
    assert!(app.shows("name=abc"), "{:?}", app.texts());
    // And the caller changing it changes the field.
    app.tap("clear");
    assert!(app.shows("name="), "{:?}", app.texts());
    assert!(!app.shows("abc"), "{:?}", app.texts());
}

#[test]
fn a_toggle_in_a_view_flips_the_callers_var_and_the_view_reads_it() {
    let mut app = App::start(
        r#"
View Row2(bind on: Bool) {
    Column {
        Toggle #[bind: on, width: 52, height: 30]
        Button("tap") => on = !on
        Text(on ? "ON" : "OFF")
    }
}
View Main() {
    var a = false
    Column { Row2(bind: a)  Text("a={a}") }
}
"#,
    );
    assert!(
        app.shows("OFF") && app.shows("a=false"),
        "{:?}",
        app.texts()
    );
    app.tap("tap");
    assert!(app.shows("ON") && app.shows("a=true"), "{:?}", app.texts());
}

// ── `for` ───────────────────────────────────────────────────────────────

#[test]
fn a_row_binds_a_var_of_the_caller_or_one_of_its_own_view() {
    let mut app = App::start(
        r#"
View Bump(bind n: Int, label: Str) { Button(label) => n++ }
View Item(label: Str) {
    var local = 0
    Column {
        Bump(bind: local, label: label)
        Text("{label}={local}")
    }
}
View Main() {
    var shared = 0
    var names = ["x", "y"]
    Column {
        for name in names {
            Bump(bind: shared, label: "s-{name}")
            Item(label: name)
        }
        Text("shared={shared}")
    }
}
"#,
    );
    app.tap("s-x");
    app.tap("s-y");
    assert!(app.shows("shared=2"), "{:?}", app.texts());
    // Each row's own var is its own cell.
    app.tap("x");
    assert!(app.shows("x=1") && app.shows("y=0"), "{:?}", app.texts());
}

#[test]
fn a_loop_variable_or_one_of_its_fields_is_not_a_place_to_bind() {
    let said = check(
        r#"
type Todo = { title: Str, done: Bool }
View Check(bind on: Bool) { Toggle #[bind: on] }
View Main() {
    var todos: List<Todo> = [{ title: "a", done: false }]
    var flags = [true, false]
    for todo in todos { Check(bind: todo.done) }
    for flag in flags { Check(bind: flag) }
}
"#,
    );
    assert_eq!(kinds(&said), ["BindNotAVar", "BindNotAVar"], "{said:?}");
    assert!(said[0].contains("`todo.done`"), "{said:?}");
    assert!(said[1].contains("`flag` is not a `var`"), "{said:?}");
}

// ── the errors ──────────────────────────────────────────────────────────

const SW: &str = "View Sw(bind on: Bool, label: Str = \"\") { Toggle #[bind: on] }\n";

#[test]
fn a_let_a_literal_an_expression_or_a_plain_parameter_is_not_a_var() {
    for (call, what) in [
        ("let k = true\n Sw(bind: k)", "`k` is a `let`"),
        ("Sw(bind: true)", "`true` cannot be written"),
        ("Sw(bind: 1)", "a literal cannot be written"),
        ("var a = true\n Sw(bind: !a)", "an expression"),
        ("var a = 1\n Sw(bind: a > 0)", "an expression"),
    ] {
        let said = check(&format!("{SW}View Main() {{ {call} }}"));
        assert_eq!(kinds(&said), ["BindNotAVar"], "{call}: {said:?}");
        assert!(said[0].contains(what), "{call}: {said:?}");
        assert!(said[0].contains("Sw(bind: "), "{call}: {said:?}");
    }
    let said = check(&format!(
        "{SW}View Inner(v: Bool) {{ Sw(bind: v) }}
         View Main() {{ var a = true
             Inner(v: a) }}"
    ));
    assert_eq!(kinds(&said), ["BindNotAVar"], "{said:?}");
    // It points at the feature as the fix, with the parameter's own type.
    assert!(said[0].contains("`bind v: Bool`"), "{said:?}");
    assert!(said[0].contains("a copy"), "{said:?}");
}

#[test]
fn the_long_form_names_the_parameter_in_the_error() {
    let said = check(
        "View P(bind a: Int, bind b: Int) { Text(\"{a}{b}\") }
         View Main() { var a = 1
             let k = 2
             P(bind a: a, bind b: k) }",
    );
    assert_eq!(kinds(&said), ["BindNotAVar"], "{said:?}");
    assert!(said[0].contains("P(bind b: k)"), "{said:?}");
}

#[test]
fn a_value_where_a_binding_is_declared_says_to_write_bind() {
    let said = check(&format!(
        "{SW}View Main() {{ var wifi = true
             Sw(on: wifi) }}"
    ));
    assert_eq!(kinds(&said), ["BindArgMismatch"], "{said:?}");
    assert!(said[0].contains("`on`"), "{said:?}");
    assert!(said[0].contains("is a binding"), "{said:?}");
    assert!(said[0].contains("`bind: wifi`"), "{said:?}");
    // And for a view with several, which one.
    let said = check(
        "View P(bind a: Int, bind b: Int) { Text(\"{a}{b}\") }
         View Main() { var x = 1
             P(bind a: x, b: x) }",
    );
    assert_eq!(kinds(&said), ["BindArgMismatch"], "{said:?}");
    assert!(said[0].contains("`bind b: x`"), "{said:?}");
}

#[test]
fn a_binding_where_a_value_is_declared_says_it_is_a_value() {
    let said = check(&format!(
        "{SW}View Main() {{ var wifi = true
             Sw(bind label: wifi) }}"
    ));
    assert_eq!(kinds(&said), ["BindArgMismatch"], "{said:?}");
    assert!(said[0].contains("`label`"), "{said:?}");
    assert!(said[0].contains("is a value, not a binding"), "{said:?}");
}

#[test]
fn a_binding_is_never_positional() {
    // The first positional argument is the first *value* parameter.
    let said = check(&format!(
        "{SW}View Main() {{ var wifi = true
             Sw(wifi) }}"
    ));
    assert_eq!(kinds(&said), ["ArgumentTypeMismatch"], "{said:?}");
    assert!(said[0].contains("`label`"), "{said:?}");
}

#[test]
fn bind_alone_needs_a_name_when_there_are_several() {
    let said = check(
        "View P(bind a: Int, bind b: Int) { Text(\"{a}{b}\") }
         View Main() { var x = 1
             P(bind: x) }",
    );
    assert_eq!(kinds(&said), ["BindNeedsName"], "{said:?}");
    assert!(
        said[0].contains("`bind a: …`") && said[0].contains("`bind b: …`"),
        "{said:?}"
    );
}

#[test]
fn a_missing_binding_and_a_view_with_none_are_told() {
    let said = check(&format!("{SW}View Main() {{ Sw(label: \"x\") }}"));
    assert!(said.is_empty(), "the checker alone is quiet: {said:?}");
    // The interpreter says the binding was not passed.
    let parsed = parse(&format!("{SW}View Main() {{ Sw(label: \"x\") }}"));
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known = ["Sw", "Main"];
    let main = byard_compiler::parser::ast::root_view(&parsed.views).unwrap();
    interp.lower_view(main, &known);
    assert!(
        interp.errors().iter().any(|e| e.kind() == "MissingParam"),
        "{:?}",
        interp.errors()
    );

    let parsed =
        parse("View Plain(v: Int) { Text(\"{v}\") }\nView Main() { var a = 1\n Plain(bind: a) }");
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let main = byard_compiler::parser::ast::root_view(&parsed.views).unwrap();
    interp.lower_view(main, &["Plain", "Main"]);
    assert!(
        interp.errors().iter().any(|e| e.kind() == "UnknownParam"),
        "{:?}",
        interp.errors()
    );
}

#[test]
fn a_var_of_another_type_is_a_type_mismatch_and_an_int_is_not_a_float() {
    let said = check(
        "View F(bind x: Float) { Text(\"{x}\") }
         View B(bind b: Bool) { Text(\"{b}\") }
         View Main() { var n = 1
             var f = 1.5
             var ok = true
             F(bind: n)
             F(bind: f)
             B(bind: n)
             B(bind: ok) }",
    );
    assert_eq!(
        kinds(&said),
        ["ArgumentTypeMismatch", "ArgumentTypeMismatch"],
        "{said:?}"
    );
    assert!(
        said[0].contains("`Float`") && said[0].contains("`Int`"),
        "{said:?}"
    );
    assert!(
        said[1].contains("`Bool`") && said[1].contains("`Int`"),
        "{said:?}"
    );
}

#[test]
fn a_binding_declaration_that_cannot_work_is_told_at_the_parameter() {
    for (src, needle) in [
        ("View V(bind on) { Text(\"x\") }", ""),
        (
            "View V(bind cb: Fn()) { Text(\"x\") }",
            "cannot be a callback",
        ),
        (
            "View V(bind: Int) { Text(\"x\") }",
            "cannot name a parameter",
        ),
        (
            "fn f(bind x: Int) -> Int => x\nView V() { Text(\"x\") }",
            "only a view's parameter",
        ),
    ] {
        let said = check(src);
        if needle.is_empty() {
            // No type: the existing annotation error says so.
            assert_eq!(kinds(&said), ["MissingAnnotation"], "{src}: {said:?}");
            continue;
        }
        assert_eq!(kinds(&said), ["BadBindingParam"], "{src}: {said:?}");
        assert!(said[0].contains(needle), "{src}: {said:?}");
    }
}

#[test]
fn bind_outside_a_view_call_is_told() {
    let said = check(
        "View Main() { var a = 1
             Text(\"x\") #[width: 10]
             Column(bind x: a) { Text(\"y\") } }",
    );
    assert_eq!(kinds(&said), ["BindOutsideViewCall"], "{said:?}");
    let said = check("View Main() { var a = 1\n let t = (bind x: a)\n Text(\"x\") }");
    assert_eq!(kinds(&said), ["BindOutsideViewCall"], "{said:?}");
}

#[test]
fn a_program_without_bindings_says_nothing_new() {
    let said = check(
        "View Card(label: Str, bind: Int = 0) { Text(label) }
         View Main() { Card(label: \"x\") }",
    );
    // A parameter named `bind` is the one thing the word takes away.
    assert_eq!(kinds(&said), ["BadBindingParam"], "{said:?}");
    let said = check(
        "View Card(label: Str, n: Int = 0) { Text(label) }
         View Main() { Card(label: \"x\") }",
    );
    assert!(said.is_empty(), "{said:?}");
}

#[test]
fn a_binding_is_a_var_for_every_name_that_takes_one() {
    let said = check(
        "View V(bind on: Bool, bind n: Int, bind s: Str, bind lv: Float, bind pick: Str) {
             Toggle #[bind: on]
             Checkbox #[bind: on]
             TextField #[bind: s]
             Slider #[bind: lv]
             RadioButton #[value: \"a\", bind: pick]
             Button(\"go\") => {
                 n += 1
                 n++
                 on = !on
             }
         }
         View Main() { var a = true
             var b = 0
             var c = \"\"
             var d = 0.5
             var e = \"a\"
             V(bind on: a, bind n: b, bind s: c, bind lv: d, bind pick: e) }",
    );
    assert!(said.is_empty(), "{said:?}");
}

// ── lifetimes ───────────────────────────────────────────────────────────

const OWNED: &str = r#"
View Counter(bind n: Int) {
    Column {
        Button("inc") => n++
        Text("n={n}")
    }
}
View Owner() {
    var x = 5
    Counter(bind: x)
}
View Main() {
    var show = true
    Column {
        when show { Owner() }
        Button("hide") => show = false
        Button("show") => show = true
    }
}
"#;

#[test]
fn unmounting_the_owner_of_the_var_and_mounting_it_again_does_not_break_the_binding() {
    let mut app = App::start(OWNED);
    assert!(app.shows("n=5"), "{:?}", app.texts());
    app.tap("inc");
    assert!(app.shows("n=6"), "{:?}", app.texts());

    app.tap("hide");
    assert!(!app.shows("inc"), "{:?}", app.texts());
    // Frames go by with nothing mounted; nothing is read or written.
    for _ in 0..3 {
        app.send(&[]);
    }
    app.tap("show");
    // A mounted view owns a new instance, and the binding is to it.
    let before = app
        .texts()
        .into_iter()
        .find(|t| t.starts_with("n="))
        .unwrap_or_else(|| panic!("{:?}", app.texts()));
    let n: i64 = before[2..].parse().unwrap();
    app.tap("inc");
    assert!(app.shows(&format!("n={}", n + 1)), "{:?}", app.texts());
}

#[test]
fn a_row_given_back_with_its_binding_still_works_after_the_list_shrinks() {
    let mut app = App::start(
        r#"
View Bump(bind n: Int, label: Str) { Button(label) => n++ }
View Item(label: Str) {
    var local = 0
    Row {
        Bump(bind: local, label: "+{label}")
        Text("{label}={local}")
    }
}
View Main() {
    var names = ["a", "b", "c"]
    Column {
        for name in names { Item(label: name) }
        Button("one") => names = ["a"]
        Button("three") => names = ["a", "b", "c"]
    }
}
"#,
    );
    app.tap("+c");
    assert!(app.shows("c=1"), "{:?}", app.texts());
    app.tap("one");
    assert!(!app.shows("+c"), "{:?}", app.texts());
    app.tap("three");
    app.tap("+a");
    assert!(app.shows("a=1"), "{:?}", app.texts());
    app.tap("+b");
    assert!(app.shows("b=1"), "{:?}", app.texts());
}

#[test]
fn a_child_that_goes_away_leaves_the_callers_var_alone() {
    let mut app = App::start(
        r#"
View Bump(bind n: Int) { Button("bump") => n++ }
View Main() {
    var n = 0
    var show = true
    Column {
        when show { Bump(bind: n) }
        Button("hide") => show = false
        Button("show") => show = true
        Button("direct") => n += 10
        Text("n={n}")
    }
}
"#,
    );
    app.tap("bump");
    app.tap("hide");
    app.tap("direct");
    assert!(app.shows("n=11"), "{:?}", app.texts());
    app.tap("show");
    app.tap("bump");
    assert!(app.shows("n=12"), "{:?}", app.texts());
}

// ── the scheme flag ─────────────────────────────────────────────────────

#[test]
fn a_binding_of_the_themes_scheme_flag_is_accepted() {
    let said = check(
        "View Sw(bind on: Bool) { Toggle #[bind: on] }
         View Main() { inject Theme as t
             Sw(bind: t.dark) }",
    );
    assert!(said.is_empty(), "{said:?}");
}
