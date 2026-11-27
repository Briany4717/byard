//! The fuzz targets (`fuzz/`), on stable, over the seeds.
//!
//! `cargo fuzz` runs them every night on nightly, on inputs it makes up from
//! the seeds. Here they run on what it starts from: every program of the
//! conformance suite, and every input it once found a panic on
//! (`fuzz/regressions`). So a target that no longer compiles, or a change that
//! makes one panic on a seed, fails the pull request, not the night after.

#[path = "../../../../fuzz/src/targets.rs"]
mod targets;

use std::path::Path;

/// A target, by its name.
type Target = (&'static str, fn(&str));

/// Runs every target on `src`, and says which panicked.
fn panics(src: &str) -> Vec<&'static str> {
    let all: [Target; 4] = [
        ("parse", targets::parse),
        ("check", targets::check),
        ("format", targets::format),
        ("run", targets::run),
    ];
    all.into_iter()
        .filter(|(_, target)| std::panic::catch_unwind(|| target(src)).is_err())
        .map(|(name, _)| name)
        .collect()
}

#[test]
fn every_target_takes_every_program_of_the_suite() {
    let mut failures = Vec::new();
    let cases = crate::conformance::all_cases();
    for case in &cases {
        let program = case.program();
        if program.len() > targets::MAX_LEN {
            continue;
        }
        let failed = panics(&program);
        if !failed.is_empty() {
            failures.push(format!("{}: {failed:?}", case.at()));
        }
    }
    assert!(
        failures.is_empty(),
        "targets panicked:\n{}",
        failures.join("\n")
    );
    assert!(cases.len() > 500, "the suite was not read");
}

#[test]
fn every_target_takes_every_input_it_once_panicked_on() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/regressions");
    let mut failures = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("fuzz/regressions") {
        let path = entry.expect("an entry").path();
        if path.extension().is_some_and(|e| e == "md") {
            continue;
        }
        let bytes = std::fs::read(&path).expect("a readable input");
        let Some(src) = targets::text(&bytes) else {
            continue;
        };
        let failed = panics(&src);
        if !failed.is_empty() {
            failures.push(format!("{}: {failed:?}", path.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "targets panicked:\n{}",
        failures.join("\n")
    );
}
