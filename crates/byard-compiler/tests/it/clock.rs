//! `now()` and `nowUtc()` (RFC-0049): where they may be called, what they
//! answer, and the clock a host can fix.

use byard_compiler::CompileError;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent};

/// A view that is lowered and drawn once, as `byard check` does.
struct Run {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    frame: RenderFrame,
}

impl Run {
    fn new(src: &str) -> Self {
        Self::with(src, |_| {})
    }

    fn with(src: &str, setup: impl FnOnce(&mut Interpreter)) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.declare_controllers(&["Http"]);
        setup(&mut interp);
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let tree = interp.lower_view(&parsed.views[0], &known);
        interp.tick();
        let mut run = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
        };
        run.draw();
        run
    }

    fn draw(&mut self) {
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 200.0);
    }

    /// Taps the point (20, 20), where the first button is.
    fn tap(&mut self, time: u64) {
        let at = |kind, time| InputEvent {
            kind,
            pos: (20.0, 20.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: time,
            mods: byard_core::KeyModifiers::default(),
        };
        self.interp.dispatch_events(&[
            at(EventKind::PointerDown, time),
            at(EventKind::PointerUp, time + 8),
        ]);
        self.interp.tick();
        self.draw();
    }

    fn var(&self, name: &str) -> String {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(name))
            .expect("a declared var");
        match self.interp.peek(sig) {
            Value::Str(s) => s,
            other => panic!("{name} is {other:?}"),
        }
    }

    fn clock_errors(&self) -> Vec<&CompileError> {
        self.interp
            .errors()
            .iter()
            .filter(|e| e.kind() == "ClockOutsideAction")
            .collect()
    }
}

/// The text of a view whose body is `body`, with one `var t` for the clock.
fn view(body: &str) -> String {
    format!("View Main() {{\n    var t: Str = \"\"\n{body}\n}}")
}

// ── where it may be called ───────────────────────────────────────────────

#[test]
fn it_may_be_called_in_every_action() {
    for (place, body) in [
        (
            "a button handler",
            "Button(\"go\") #[width: 100, height: 40] => t = now()",
        ),
        (
            "a brace handler",
            "Button(\"go\") #[width: 100, height: 40] => { t = nowUtc() }",
        ),
        ("on mount", "on mount => t = now()"),
        ("on unmount", "on unmount => t = now()"),
        ("every", "every 1s => t = now()"),
        ("after", "after 1s => { t = now() }"),
        ("on change", "var n = 0\n    on change(n) => t = now()"),
        (
            "an ok arm",
            "inject Http as http\n    on mount => http.get(\"/x\") ok r => { t = now() } err e => { t = nowUtc() }",
        ),
        (
            "a named action called from a handler",
            "let stamp = () => { t = now() }\n    Button(\"go\") #[width: 100, height: 40] => stamp()",
        ),
        (
            "a named action with a parameter",
            "let stamp = (tag) => { t = tag + now() }\n    Button(\"go\") #[width: 100, height: 40] => stamp(\"a\")",
        ),
        (
            "an argument of a fn called in an action",
            "fn wrap(s) => \"[\" + s + \"]\"\n    Button(\"go\") #[width: 100, height: 40] => t = wrap(now())",
        ),
    ] {
        let run = Run::new(&view(&format!("    {body}\n    Text(t)")));
        assert!(
            run.interp.errors().is_empty(),
            "{place}: {:?}",
            run.interp.errors()
        );
    }
}

#[test]
fn it_may_not_be_called_where_a_value_is_wanted() {
    for (place, body) in [
        ("a let", "let today = now().slice(0, 10)\n    Text(today)"),
        ("a text", "Text(now())"),
        ("an interpolation", "Text(\"it is {now()}\")"),
        (
            "an attribute",
            "Box #[bg: now() == \"\" ? 0xFF0000 : 0x00FF00, width: 10, height: 10] {}",
        ),
        ("a var seed", "var seeded = nowUtc()\n    Text(seeded)"),
        (
            "a fn without parameters",
            "fn stamp() => now()\n    Text(stamp())",
        ),
        (
            "a fn that nothing calls",
            "fn stamp(a) => a + now()\n    Text(\"x\")",
        ),
        (
            "a fn called from an action",
            "fn stamp(a) => a + now()\n    Button(\"go\") #[width: 100, height: 40] => t = stamp(\"x\")",
        ),
        (
            "a named action called from a let",
            "let stamp = () => { t = now() }\n    let again = stamp()\n    Text(\"x\")",
        ),
    ] {
        let run = Run::new(&view(&format!("    {body}")));
        assert!(
            !run.clock_errors().is_empty(),
            "{place}: expected ClockOutsideAction, got {:?}",
            run.interp.errors()
        );
    }
}

#[test]
fn a_top_level_fn_may_not_read_the_clock() {
    let src = "fn stamped(s: Str) -> Str => s + now()\n\
               View Main() {\n    var t: Str = \"\"\n    \
               Button(\"go\") #[width: 100, height: 40] => t = stamped(\"x\")\n    Text(t)\n}";
    let run = Run::new(src);
    assert_eq!(run.clock_errors().len(), 1, "{:?}", run.interp.errors());
}

#[test]
fn the_message_names_the_repair() {
    let run = Run::new(&view("    Text(now())"));
    let message = run.clock_errors()[0].headline();
    assert!(
        message.contains("reads the clock once, when an action runs")
            && message.contains("every 1s => time = now()"),
        "{message}"
    );
}

#[test]
fn a_program_that_names_its_own_now_gets_its_own() {
    let run = Run::new("View Main() {\n    fn now() => \"mine\"\n    Text(now())\n}");
    assert!(run.interp.errors().is_empty(), "{:?}", run.interp.errors());
    assert_eq!(run.frame.texts()[0].text, "mine");
}

#[test]
fn the_type_checker_knows_them_as_text() {
    let checked = |body: &str| {
        let parsed = parse(&view(body));
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        byard_compiler::infer::check_views(&parsed.views).errors
    };
    let clean = |body: &str| {
        let found = checked(body);
        assert_eq!(found.len(), 0, "{found:?}");
    };
    // Text methods and comparisons check; a number's do not.
    clean("    on mount => t = now().slice(0, 10)");
    clean("    on mount => t = nowUtc().formatDate(\"D\") ?? \"\"");
    clean("    var due = \"2026-12-01\"\n    on mount => t = now() < due ? \"a\" : \"b\"");
    assert!(
        !checked("    var n: Int = 0\n    on mount => n = now() * 2").is_empty(),
        "text is not a number"
    );
}

// ── what it answers ──────────────────────────────────────────────────────

/// `YYYY-MM-DDTHH:MM:SS`, with `Z` after it when `utc`.
fn is_iso(text: &str, utc: bool) -> bool {
    let want = if utc { 20 } else { 19 };
    if text.len() != want || text.ends_with('Z') != utc {
        return false;
    }
    text.bytes().take(19).enumerate().all(|(i, b)| match i {
        4 | 7 => b == b'-',
        10 => b == b'T',
        13 | 16 => b == b':',
        _ => b.is_ascii_digit(),
    })
}

#[test]
fn the_real_clock_answers_iso_text() {
    let mut run = Run::new(&view(
        "    var u: Str = \"\"\n    on mount => { t = now() u = nowUtc() }\n    Text(t)",
    ));
    run.tap(0);
    let (local, utc) = (run.var("t"), run.var("u"));
    assert!(is_iso(&local, false), "{local}");
    assert!(is_iso(&utc, true), "{utc}");
    // The year is this one, whatever the date: the system's clock, not a stub.
    assert!(local.starts_with("20"), "{local}");
    // Both read as dates, so `formatDate` takes them, `Z` and all.
    let parsed = byard_compiler::interp::dates::format_date(&utc, "YYYY");
    assert_eq!(parsed.as_deref(), Some(&utc[..4]));
}

#[test]
fn a_fixed_clock_is_fixed_and_app_time_advances_it() {
    let src = view(
        "    var u: Str = \"\"\n    \
         Button(\"go\") #[width: 100, height: 40] => { t = now() u = nowUtc() }\n    Text(t)",
    );
    let mut run = Run::with(&src, |i| {
        i.set_wall_clock("2026-11-05T14:03:12").unwrap();
    });
    run.interp.set_now_ms(0);
    run.tap(0);
    assert_eq!(run.var("t"), "2026-11-05T14:03:12");
    // A fixed clock has no zone: the same time, with the `Z`.
    assert_eq!(run.var("u"), "2026-11-05T14:03:12Z");

    // 3.5 s of app time later, the clock reads 3 s further on.
    run.interp.set_now_ms(3500);
    run.tap(3500);
    assert_eq!(run.var("t"), "2026-11-05T14:03:15");

    // Across a day: 10 hours is the next morning.
    run.interp.set_now_ms(10 * 3_600_000);
    run.tap(10 * 3_600_000);
    assert_eq!(run.var("t"), "2026-11-06T00:03:12");
}

#[test]
fn a_fixed_clock_starts_from_the_app_time_it_was_set_at() {
    let src = view("    Button(\"go\") #[width: 100, height: 40] => t = now()\n    Text(t)");
    let mut run = Run::new(&src);
    run.interp.set_now_ms(5000);
    run.interp.set_wall_clock("2026-01-01T00:00:00").unwrap();
    run.tap(5000);
    assert_eq!(run.var("t"), "2026-01-01T00:00:00");
    run.interp.set_now_ms(7000);
    run.tap(7000);
    assert_eq!(run.var("t"), "2026-01-01T00:00:02");
}

#[test]
fn a_clock_that_is_not_a_date_is_refused() {
    let mut interp = Interpreter::new();
    for bad in ["", "soon", "2026-13-01T00:00:00", "2026-11-05T25:00:00"] {
        let err = interp.set_wall_clock(bad).unwrap_err();
        assert!(err.contains("2026-11-05T09:00:00"), "{bad}: {err}");
    }
    // A date alone is midnight, and the seconds may be left off.
    assert!(interp.set_wall_clock("2026-11-05").is_ok());
    assert!(interp.set_wall_clock("2026-11-05T09:00").is_ok());
}

#[test]
fn text_order_is_time_order() {
    let mut run = Run::with(
        &view(
            "    Button(\"go\") #[width: 100, height: 40] => t = now() < \"2026-11-05T14:03:13\" ? \"before\" : \"after\"\n    Text(t)",
        ),
        |i| i.set_wall_clock("2026-11-05T14:03:12").unwrap(),
    );
    run.interp.set_now_ms(0);
    run.tap(0);
    assert_eq!(run.var("t"), "before");
    run.interp.set_now_ms(2000);
    run.tap(2000);
    assert_eq!(run.var("t"), "after");
}
