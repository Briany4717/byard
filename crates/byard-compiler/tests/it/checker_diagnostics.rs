//! Byard never silently does nothing: four places where it did.
//!
//! - `bind:` on something a control cannot write (a view parameter, a `let`, a
//!   literal) drew its default and ignored every edit, and `check` was clean.
//! - A `Bool` given to a `Str` parameter was never compared with the declared
//!   type, and a value that was not a `Str` reached the screen as the Rust
//!   debug form (`Bool(false)`).
//! - A keyword used as a name gave a screenful of errors, none about the name.
//! - `basis` was accepted on a `Box` and ignored.

use byard_compiler::CompileError;
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

fn kinds(source: &str) -> Vec<String> {
    check(source)
        .into_iter()
        .map(|l| l.split(':').next().unwrap_or_default().to_string())
        .collect()
}

// ── `bind:` on something that cannot be written ─────────────────────────

#[test]
fn bind_on_a_view_parameter_is_an_error_that_says_what_to_do() {
    let said = check(
        "View Edit(value: Str) { TextField #[bind: value] }
         View Main() { var a = \"\"
             Edit(value: a) }",
    );
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("BindNotAVar: "), "{said:?}");
    // It names the parameter, says a parameter is a copy, and points at the
    // fix: declare the parameter a binding, with its type, for the caller to
    // pass a `var` to (RFC-0051).
    assert!(said[0].contains("`value`"), "{said:?}");
    assert!(said[0].contains("a copy"), "{said:?}");
    assert!(said[0].contains("`bind value: Str`"), "{said:?}");
}

#[test]
fn bind_on_a_let_or_a_literal_is_an_error() {
    for (src, kind) in [
        ("let k = \"x\"\n TextField #[bind: k]", "a `let`"),
        ("let k = true\n Toggle #[bind: k]", "a `let`"),
        ("TextField #[bind: \"x\"]", "a literal"),
        ("TextField #[value: \"x\"]", "a literal"),
        ("Slider #[bind: 0.5]", "a literal"),
        ("Checkbox #[value: true]", "a literal"),
    ] {
        let said = check(&format!("View Main() {{ {src} }}"));
        assert_eq!(said.len(), 1, "{src}: {said:?}");
        assert!(said[0].starts_with("BindNotAVar: "), "{src}: {said:?}");
        assert!(
            said[0].contains(kind) || kind == "a literal",
            "{src}: {said:?}"
        );
    }
}

#[test]
fn bind_on_a_loop_variable_or_a_lambda_parameter_is_an_error() {
    let said = check(
        "View Main() { var xs = [\"a\"]
             for x in xs { TextField #[bind: x] } }",
    );
    assert_eq!(kinds_of(&said), ["BindNotAVar"], "{said:?}");
}

fn kinds_of(said: &[String]) -> Vec<&str> {
    said.iter()
        .map(|l| l.split(':').next().unwrap_or_default())
        .collect()
}

#[test]
fn bind_on_a_var_is_still_fine() {
    let said = check(
        "View Main() { var a = \"\"
             var on = false
             var level = 0.5
             var plan = \"free\"
             TextField #[bind: a]
             Toggle #[bind: on]
             Checkbox #[value: on]
             Slider #[bind: level]
             RadioButton #[value: \"free\", bind: plan]
             Toggle #[bind: theme.dark] }",
    );
    // Only the unknown `theme` of a test with no theme is allowed to speak.
    assert!(
        said.iter().all(|l| !l.starts_with("BindNotAVar")),
        "{said:?}"
    );
}

// ── the pattern the error recommends ────────────────────────────────────

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
            .render(&self.tree, &mut self.frame, 400.0, 300.0);
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

/// What `BindNotAVar` recommends, written out: a field in a view of its own,
/// fed by a parameter and reporting through a callback.
const REUSABLE_FIELD: &str = r#"
View Field(value: Str, on_change: Fn(Str) = {|t| }) {
    var text = value
    on change(value) => text = value
    TextField #[bind: text, placeholder: "type", input(t) => on_change(t), width: 200, height: 36]
}
View Main() {
    var a = "hello"
    Column #[p: 10, gap: 8] {
        Field(value: a, on_change: {|t| a = t })
        Button("Reset") => a = "reset"
        Text("a={a}")
    }
}
"#;

#[test]
fn the_recommended_pattern_shows_the_value_writes_it_up_and_follows_the_caller() {
    let mut app = App::start(REUSABLE_FIELD);
    // The caller's value is shown in the field, and in the line under it.
    assert!(app.shows("hello"), "{:?}", app.texts());
    assert!(app.shows("a=hello"), "{:?}", app.texts());

    // Typing reaches the caller's `var`.
    app.tap("hello");
    app.key("End");
    app.type_text("!");
    assert!(app.shows("a=hello!"), "{:?}", app.texts());

    // The caller changing it changes the field.
    app.tap("Reset");
    assert!(app.shows("a=reset"), "{:?}", app.texts());
    assert!(app.shows("reset"), "{:?}", app.texts());
}

// ── a Bool given to a Str parameter ─────────────────────────────────────

const LABEL: &str = "View Label(error: Str) { Text(error) }\n";

#[test]
fn a_bool_given_to_a_str_parameter_is_a_type_mismatch() {
    // `a || b ? msg : ""` is `a || (b ? msg : "")`, a Bool.
    let said = check(&format!(
        "{LABEL}View Main() {{ var a = false
             var b = true
             Label(error: a || b ? \"msg\" : \"\") }}"
    ));
    assert!(
        said.contains(
            &"ArgumentTypeMismatch: parameter `error` of view `Label` is a `Str`, but this is a `Bool`"
                .to_string()
        ),
        "{said:?}"
    );
}

#[test]
fn a_view_argument_is_checked_by_position_by_name_and_as_an_attribute() {
    for call in [
        "Label(a)",
        "Label(error: a)",
        "Label() #[error: a]",
        "Label(a) #[width: 10]",
    ] {
        let said = kinds(&format!("{LABEL}View Main() {{ var a = false\n {call} }}"));
        assert_eq!(said, ["ArgumentTypeMismatch"], "{call}: {said:?}");
    }
}

#[test]
fn matching_and_unknown_argument_types_are_not_mismatches() {
    let said = check(
        "View V(n: Int, f: Float, s: Str, b: Bool, l: List<Str>, cb: Fn(Int) = {|x| }) {
             Text(s)
         }
         type P = { name: Str }
         View W(p: P) { Text(p.name) }
         View Main() {
             var s = \"x\"
             var xs = [\"a\"]
             var p: P = { name: \"x\" }
             let big = s.len > 3
             V(1, 2, s, big, xs)
             V(n: 1, f: 2.5, s: \"{big}\", b: !big, l: xs.map(x => x))
             V(n: s.len, f: s.len, s: s.slice(0, 1), b: s == \"x\", l: [])
             W(p: p)
         }",
    );
    assert!(said.is_empty(), "{said:?}");
}

#[test]
fn every_wrong_argument_is_named_with_both_types() {
    let said = check(
        "View V(n: Int, s: Str, l: List<Int>) { Text(s) }
         View Main() { var xs = [\"a\"]
             V(n: \"1\", s: 2, l: xs) }",
    );
    assert_eq!(said.len(), 3, "{said:?}");
    assert!(
        said[0].ends_with("is a `Int`, but this is a `Str`"),
        "{said:?}"
    );
    assert!(
        said[1].ends_with("is a `Str`, but this is a `Int`"),
        "{said:?}"
    );
    assert!(
        said[2].ends_with("is a `List<Int>`, but this is a `List<Str>`"),
        "{said:?}"
    );
}

/// The words on screen are the words interpolation writes: never the Rust
/// debug form of a value.
#[test]
fn no_value_reaches_the_screen_in_its_debug_form() {
    let src = r#"
View Show(t: Str) { Text(t) }
View Flag(v: Bool) { Text(v) }
View Num(v: Int) { Text(v) }
View Frac(v: Float) { Text(v) }
View Main() {
    var off = false
    Column {
        Flag(v: off)
        Flag(v: !off)
        Num(v: 42)
        Frac(v: 2.5)
        Text("{off}")
    }
}
"#;
    let app = App::start(src);
    let texts = app.texts();
    assert_eq!(
        texts.iter().filter(|t| *t == "false").count(),
        2,
        "{texts:?}"
    );
    assert!(app.shows("true"), "{texts:?}");
    assert!(app.shows("42"), "{texts:?}");
    assert!(app.shows("2.5"), "{texts:?}");
    assert!(
        texts
            .iter()
            .all(|t| !t.contains("Bool(") && !t.contains("Int(") && !t.contains("Float(")),
        "{texts:?}"
    );
}

#[test]
fn or_then_ternary_without_parentheses_is_a_warning_and_with_them_is_not() {
    let body = |expr: &str| {
        format!(
            "View Main() {{ var a = false
                 var b = true
                 Text({expr}) }}"
        )
    };
    let said = check(&body("a || b ? \"x\" : \"y\""));
    assert_eq!(kinds_of(&said), ["MixedTernaryPrecedence"], "{said:?}");
    for fine in [
        "(a || b) ? \"x\" : \"y\"",
        "a && b ? \"x\" : \"y\"",
        "a ? \"x\" : \"y\"",
    ] {
        let said = check(&body(fine));
        assert!(said.is_empty(), "{fine}: {said:?}");
    }
    // It is a warning: it does not fail the build.
    let errors = resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: body("a || b ? \"x\" : \"y\""),
        }],
        &mut NoPackages,
    )
    .errors;
    assert!(errors.iter().all(CompileError::is_warning), "{errors:?}");
}

// ── a keyword used as a name ────────────────────────────────────────────

#[test]
fn a_keyword_naming_a_function_is_one_error_at_the_name() {
    let source = "fn when(at: Str) -> Str => at\nView Main() { Text(\"x\") }";
    let errors = resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: source.to_string(),
        }],
        &mut NoPackages,
    )
    .errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].kind(), "KeywordAsName");
    assert_eq!(
        errors[0].headline(),
        "`when` is a keyword and cannot name a function"
    );
    let span = errors[0].span();
    assert_eq!(&source[span.start as usize..span.end as usize], "when");
}

#[test]
fn a_keyword_naming_anything_else_is_one_error_each() {
    for (src, headline) in [
        (
            "View for() { Text(\"x\") }\nView Main() { Text(\"x\") }",
            "`for` is a keyword and cannot name a view",
        ),
        (
            "type in = { name: Str }\nView Main() { Text(\"x\") }",
            "`in` is a keyword and cannot name a type",
        ),
        (
            "type P = { style: Str }\nView Main() { Text(\"x\") }",
            "`style` is a keyword and cannot name a field",
        ),
        (
            "View V(let: Str) { Text(\"x\") }\nView Main() { Text(\"x\") }",
            "`let` is a keyword and cannot name a parameter",
        ),
        (
            "fn f(as: Str) -> Str => \"x\"\nView Main() { Text(\"x\") }",
            "`as` is a keyword and cannot name a parameter",
        ),
        (
            "View Main() { var else = 1\n Text(\"x\") }",
            "`else` is a keyword and cannot name a `var`",
        ),
        (
            "View Main() { let with = 1\n Text(\"x\") }",
            "`with` is a keyword and cannot name a `let`",
        ),
        (
            "View Main() { inject Foo as use\n Text(\"x\") }",
            "`use` is a keyword and cannot name an `inject` binding",
        ),
    ] {
        let said = check(src);
        assert_eq!(said.len(), 1, "{src}: {said:?}");
        assert_eq!(said[0], format!("KeywordAsName: {headline}"), "{src}");
    }
}

#[test]
fn a_stray_top_level_token_is_one_error_not_one_per_token() {
    // The stray `when` and everything after it, up to the next item.
    let said = check(
        "when foo(a: Str) -> Str => a + (1 + 2) { } }\nView Main() { Text(\"x\") }\nstray stuff here\nfn ok() -> Int => 1",
    );
    assert_eq!(
        kinds_of(&said),
        ["UnexpectedToken", "UnexpectedToken"],
        "{said:?}"
    );
}

// ── `basis` ─────────────────────────────────────────────────────────────

fn boxes(children: &str) -> Vec<[f32; 4]> {
    let src = format!(
        "View Main() {{
    Row #[width: 400, height: 40] {{
        {children}
    }}
}}"
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame.instances().iter().map(|b| b.rect).collect()
}

fn widths(rects: &[[f32; 4]]) -> Vec<f32> {
    rects.iter().map(|r| r[2].round()).collect()
}

#[test]
fn basis_sets_the_main_axis_size_a_box_has_before_it_grows() {
    let rects = boxes(
        "Box #[basis: 150, height: 40, bg: 0x3366ff] {}
         Box #[basis: 50, grow: 1, height: 40, bg: 0xff6633] {}",
    );
    // 150, and the 50 plus the 200 that is left.
    assert_eq!(widths(&rects), [150.0, 250.0], "{rects:?}");
}

#[test]
fn basis_means_the_height_in_a_column() {
    let src = "View Main() {
        Column #[width: 100, height: 300] {
            Box #[basis: 120, bg: 0x3366ff] {}
            Box #[basis: 30, grow: 1, bg: 0xff6633] {}
        }
    }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    let heights: Vec<f32> = frame
        .instances()
        .iter()
        .map(|b| b.rect[3].round())
        .collect();
    assert_eq!(heights, [120.0, 180.0], "{:?}", frame.instances());
}

#[test]
fn without_basis_nothing_changes() {
    let rects = boxes(
        "Box #[width: 100, height: 40, bg: 0x3366ff] {}
         Box #[grow: 1, height: 40, bg: 0xff6633] {}",
    );
    assert_eq!(widths(&rects), [100.0, 300.0], "{rects:?}");
}

/// What lowering says about `source`: the element checks run there.
fn lowering_errors(source: &str) -> Vec<String> {
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    interp.lower_view(&parsed.views[0], &["Main"]);
    interp
        .errors()
        .iter()
        .map(|e| format!("{}: {}", e.kind(), e.headline()))
        .collect()
}

#[test]
fn basis_where_it_does_nothing_is_an_unknown_attribute() {
    for el in [
        "Image(\"a.png\") #[basis: 3]",
        "Toggle #[bind: on, basis: 3]",
        "Slider #[bind: level, basis: 3]",
        "TextField #[bind: text, basis: 3]",
        "Checkbox #[bind: on, basis: 3]",
    ] {
        let said = lowering_errors(&format!(
            "View Main() {{ var on = false
                 var level = 0.5
                 var text = \"\"
                 {el} }}"
        ));
        assert!(
            said.iter().any(|l| l.starts_with("UnknownAttribute")),
            "{el}: {said:?}"
        );
    }
    // And where it works it is accepted.
    for el in [
        "Box #[basis: 3] {}",
        "Row #[basis: 3] {}",
        "Column #[basis: 3] {}",
        "Button(\"b\") #[basis: 3]",
        "Spacer #[basis: 3]",
        "Text(\"x\") #[basis: 3]",
        "ScrollView #[basis: 3] {}",
    ] {
        let said = lowering_errors(&format!("View Main() {{ {el} }}"));
        assert!(said.is_empty(), "{el}: {said:?}");
    }
}

// ── `lines:` on a `Text` ────────────────────────────────────────────────

/// `lines:` was accepted and nothing read it, so a `Text` meant to stop at two
/// lines ran on to six with a clean `check`. Limiting lines is not something
/// the text stack does, so it says so instead.
#[test]
fn lines_on_a_text_is_an_unknown_attribute() {
    let parsed = parse("View Main() { Text(\"a long text\") #[lines: 2] }");
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let _ = interp.lower_view(&parsed.views[0], &["Main"]);
    let said: Vec<&str> = interp.errors().iter().map(CompileError::kind).collect();
    assert_eq!(said, ["UnknownAttribute"], "{said:?}");
}
