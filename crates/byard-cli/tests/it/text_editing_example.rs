//! Guards the committed editing example (RFC-0041): it checks clean, and the
//! command its header gives, driven through the interpreter, selects what the
//! header says it does.
//!
//! The same command is `byard shot`'s job with a GPU; this is the part of it
//! that needs none, the part that decides what the picture shows.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, InputPayload, KeyModifiers};
use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/text_editing")
}

#[test]
fn text_editing_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
}

#[test]
fn the_header_command_selects_the_typo_it_describes() {
    let src = std::fs::read_to_string(example_dir().join("src/main.byd")).unwrap();
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let mut frame = RenderFrame::new();
    let mut t = 0;
    let mut step = |interp: &mut Interpreter, events: Vec<InputEvent>| {
        interp.dispatch_events(&events);
        interp.tick();
        frame = RenderFrame::new();
        interp.render(&tree, &mut frame, 440.0, 880.0);
    };
    let event = |t: u64, kind, pos, payload, shift| InputEvent {
        kind,
        pos,
        delta: (0.0, 0.0),
        payload,
        time_ms: t,
        mods: KeyModifiers {
            shift,
            ..KeyModifiers::default()
        },
    };
    step(&mut interp, vec![]);

    // `--tap 200,120`: the first field, which is where the header says.
    t += 16;
    step(
        &mut interp,
        vec![
            event(t, EventKind::PointerDown, (200.0, 120.0), None, false),
            event(t, EventKind::PointerUp, (200.0, 120.0), None, false),
        ],
    );
    // `--type "hello wrold"`.
    t += 16;
    let typed = "hello wrold"
        .chars()
        .map(|c| {
            event(
                t,
                EventKind::TextInput,
                (0.0, 0.0),
                Some(InputPayload::Key(c.to_string())),
                false,
            )
        })
        .collect();
    step(&mut interp, typed);
    // `--key Left` three times, then `--key shift+Left` twice.
    for shift in [false, false, false, true, true] {
        t += 16;
        step(
            &mut interp,
            vec![event(
                t,
                EventKind::KeyDown,
                (0.0, 0.0),
                Some(InputPayload::Key("ArrowLeft".into())),
                shift,
            )],
        );
    }

    let sig = interp.var_signal(&Symbol::intern("query")).unwrap();
    assert_eq!(interp.peek(sig), Value::Str("hello wrold".into()));
    // `wr`, selected backwards from the caret between `r` and `o`.
    assert_eq!(interp.focused_selection(), Some((8, 6)));
    assert_eq!(
        frame.decorated().len(),
        1,
        "the selection is drawn: the picture has something to show"
    );
}
