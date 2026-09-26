//! Guards the committed IME example (RFC-0040): it checks clean, and a
//! composition driven through the file a reader runs behaves as the header
//! says: the preedit is drawn, the echo line does not move until the commit.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, ImeEvent, InputEvent};
use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/text_input")
}

#[test]
fn text_input_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
}

#[test]
fn a_composition_shows_but_only_the_commit_reaches_the_echo() {
    let src = std::fs::read_to_string(example_dir().join("src/main.byd")).unwrap();
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let mut frame = RenderFrame::new();
    let mut step = |interp: &mut Interpreter, events: Vec<InputEvent>| {
        interp.dispatch_events(&events);
        interp.tick();
        frame = RenderFrame::new();
        interp.render(&tree, &mut frame, 560.0, 480.0);
        frame
            .texts()
            .iter()
            .map(|t| t.text.clone())
            .collect::<Vec<_>>()
    };
    let _ = step(&mut interp, vec![]);
    // The first field: 32 of padding, then the title line, then the field.
    let tap = |kind, t| InputEvent {
        kind,
        pos: (200.0, 90.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: t,
    };
    let _ = step(
        &mut interp,
        vec![
            tap(EventKind::PointerDown, 10),
            tap(EventKind::PointerUp, 20),
        ],
    );
    assert!(
        frame_has_caret(&mut interp, &tree),
        "tapping the first field focuses it"
    );

    let preedit = ImeEvent::Preedit {
        text: "にほん".into(),
        cursor: Some((9, 9)),
    }
    .into_input(30)
    .unwrap();
    let texts = step(&mut interp, vec![preedit]);
    assert!(texts.iter().any(|t| t == "にほん"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "name = \"\""), "{texts:?}");

    let commit: Vec<InputEvent> = [
        ImeEvent::Preedit {
            text: String::new(),
            cursor: None,
        },
        ImeEvent::Commit("日本".into()),
    ]
    .into_iter()
    .filter_map(|e| e.into_input(40))
    .collect();
    let texts = step(&mut interp, commit);
    assert!(texts.iter().any(|t| t == "name = \"日本\""), "{texts:?}");
    let sig = interp.var_signal(&Symbol::intern("name")).unwrap();
    assert_eq!(interp.peek(sig), Value::Str("日本".into()));
}

fn frame_has_caret(
    interp: &mut Interpreter,
    tree: &[byard_compiler::interp::eval::RenderNode],
) -> bool {
    let mut frame = RenderFrame::new();
    interp.render(tree, &mut frame, 560.0, 480.0);
    frame.text_input().is_some()
}
