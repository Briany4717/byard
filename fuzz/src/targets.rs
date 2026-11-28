//! What each fuzz target does with one input, as plain functions.
//!
//! `cargo fuzz` runs them on inputs it makes up (`fuzz/fuzz_targets/*.rs`,
//! nightly), and the workspace's own tests run them on stable over the seed
//! corpus (`crates/byard-cli/tests/it/fuzz_targets.rs`), so a target that no
//! longer compiles, or that panics on a seed, fails a pull request and not the
//! nightly run.
//!
//! Each one either returns or panics, and a panic is what the fuzzer reports:
//! every assertion here is a promise the language makes for any text at all.

use byard_compiler::edition::Edition;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::resolve::{PackageProvider, ResolvedProgram, SourceFile, resolve_program_in};
use byard_core::frame::RenderFrame;

/// Longer inputs are skipped: what they find, a shorter one finds too, and
/// they spend the run's time on lexing.
pub const MAX_LEN: usize = 8 * 1024;

/// The editions a program is read in: every one there is.
const EDITIONS: [Edition; Edition::ALL.len()] = Edition::ALL;

/// The text of an input, or `None` for one too long to be worth running.
#[must_use]
pub fn text(data: &[u8]) -> Option<String> {
    (data.len() <= MAX_LEN).then(|| String::from_utf8_lossy(data).into_owned())
}

struct NoPackages;

impl PackageProvider for NoPackages {
    fn package_files(&mut self, _: &str, _: &str) -> Result<Vec<SourceFile>, String> {
        Err("a fuzzed program has no packages".to_string())
    }
}

fn resolve(src: &str, edition: Edition) -> ResolvedProgram {
    resolve_program_in(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: src.to_string(),
        }],
        &mut NoPackages,
        edition,
    )
}

/// Lexing and parsing: any text is tokens and a tree, with errors for what
/// does not fit, and every error points inside the text.
pub fn parse(src: &str) {
    let parsed = byard_compiler::parser::parse(src);
    for e in &parsed.errors {
        let span = e.span();
        assert!(
            span.start <= span.end && span.end as usize <= src.len(),
            "an error's span {span:?} is outside the text of {} bytes: {e:?}",
            src.len()
        );
    }
}

/// Checking, in every edition: resolve, names, types, and every error a
/// diagnostic that can be printed.
pub fn check(src: &str) {
    for edition in EDITIONS {
        let program = resolve(src, edition);
        for e in &program.errors {
            let _ = program.source_map.render_line(e);
            let _ = e.headline();
        }
    }
}

/// Formatting: a text the formatter takes, it gives back formatted, and
/// formatting that changes nothing.
pub fn format(src: &str) {
    let Ok(once) = byard_compiler::fmt::format(src) else {
        return;
    };
    let twice = byard_compiler::fmt::format(&once)
        .unwrap_or_else(|e| panic!("the formatter refuses what it wrote: {e}\n{once}"));
    assert_eq!(once, twice, "formatting twice changes the text");
}

/// Running: what `byard check` does to a program (lower and render every
/// view, every branch), and, for one with no error, its `Main` drawn for a few
/// frames with time going by.
pub fn run(src: &str) {
    for edition in EDITIONS {
        let program = resolve(src, edition);
        let found = byard_project::check::check_program(&program, Theme::byard_base());
        if found.iter().any(|e| !e.is_warning()) {
            continue;
        }
        let Some(main) = program
            .views
            .iter()
            .find(|v| v.name.as_str() == "Main")
            .or_else(|| program.views.first())
        else {
            continue;
        };
        let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
        let mut interp = Interpreter::new();
        interp.set_theme(Theme::byard_base());
        interp.declare_controllers(&byard_core::cap::provided_names());
        interp.use_virtual_timers();
        interp.load_views(&program.views);
        let tree = interp.lower_view(main, &known);
        let mut frame = RenderFrame::new();
        for now in [0, 16, 1000] {
            interp.set_now_ms(now);
            interp.tick();
            frame.clear();
            interp.render(&tree, &mut frame, 400.0, 400.0);
        }
    }
}
