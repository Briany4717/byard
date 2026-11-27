//! The byld conformance suite: what the language reference says, one small
//! program at a time.
//!
//! The cases are in `tests/conformance/*.byd`. A file holds many, each opened
//! by a `//# case:` line and described by the `//#` lines under it:
//!
//! ```text
//! //# case: int-division-truncates
//! //# text: 3
//! View Main() { Text("{7 / 2}") }
//! ```
//!
//! | Line | Meaning |
//! |------|---------|
//! | `//# case: name` | Starts a case. A name is used once in its file. |
//! | `//# fragment` | The source is the body of a view: it is wrapped in `View Main() { … }`. |
//! | `//# text: a \| b` | What the program draws: every text of the first frame, in order, joined by ` \| `. |
//! | `//# wait: 300` | Lets that many milliseconds of the app clock pass before the frame is read, so timers run. |
//! | `//# error: Kind` | `byard check` reports this error. One line per kind. |
//! | `//# warning: Kind` | `byard check` reports this warning. One line per kind. |
//! | `//# edition: 2027` | The edition the case is written in. Without it, the baseline. |
//! | `//# diagnostics: 1` | How many diagnostics there are in all, for a case about one mistake being one error. |
//! | `//# gap: why` | The lines above say what *should* happen; today it does not. |
//!
//! A case with no `error:` and no `warning:` must check with no diagnostic at
//! all. The kinds a case lists are the whole set: one more, or one fewer, is a
//! failure, so an error that cascades into three is seen. `text:` is read from
//! the program as an app runs it, after `byard check` has passed it.
//!
//! A `gap:` case is the other half of the suite. It records what the language
//! is meant to do where it does not do it yet, and it is expected to *fail*:
//! the day it passes, the test fails and says to delete the `gap:` line. So
//! the list of gaps can only shrink on purpose, and it is always the list of
//! what is left (`docs/language/roadmap.md` has the reasons and the order).
//!
//! A program that makes the checker or the interpreter panic is a failure of
//! its case, named as such, whatever the case expected.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::ast::root_view;
use byard_compiler::resolve::{PackageProvider, ResolvedProgram, SourceFile, resolve_program_in};
use byard_core::frame::RenderFrame;

fn cases_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/conformance")
}

/// One program and what it is expected to do.
#[derive(Debug, Clone, Default)]
struct Case {
    file: String,
    line: usize,
    name: String,
    source: String,
    fragment: bool,
    text: Option<String>,
    wait: u32,
    edition: byard_compiler::edition::Edition,
    diagnostics: Option<usize>,
    errors: BTreeSet<String>,
    warnings: BTreeSet<String>,
    gap: Option<String>,
}

impl Case {
    fn at(&self) -> String {
        format!("{}:{} `{}`", self.file, self.line, self.name)
    }

    fn program(&self) -> String {
        if self.fragment {
            format!("View Main() {{\n{}\n}}\n", self.source)
        } else {
            self.source.clone()
        }
    }
}

/// Splits one file into its cases. A malformed header is an error that names
/// the line, so a typo cannot turn a case off.
fn cases_of(file: &str, text: &str) -> Result<Vec<Case>, String> {
    let mut cases: Vec<Case> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let Some(directive) = line.strip_prefix("//#") else {
            if let Some(case) = cases.last_mut() {
                case.source.push_str(line);
                case.source.push('\n');
            } else if !line.trim().is_empty() && !line.trim_start().starts_with("//") {
                return Err(format!(
                    "{file}:{number}: code before the first `//# case:`"
                ));
            }
            continue;
        };
        let directive = directive.trim();
        let (key, value) = match directive.split_once(':') {
            Some((key, value)) => (key.trim(), value.trim()),
            None => (directive, ""),
        };
        // Which sections of the reference a file is the cases of: read by
        // `every_section_of_the_reference_has_cases`, not by a case.
        if key == "covers" && cases.is_empty() {
            continue;
        }
        if key == "case" {
            if value.is_empty() {
                return Err(format!("{file}:{number}: a case needs a name"));
            }
            cases.push(Case {
                file: file.to_string(),
                line: number,
                name: value.to_string(),
                ..Case::default()
            });
            continue;
        }
        let Some(case) = cases.last_mut() else {
            return Err(format!(
                "{file}:{number}: `//# {key}` before any `//# case:`"
            ));
        };
        if !case.source.trim().is_empty() {
            return Err(format!(
                "{file}:{number}: `//# {key}` after the code of `{}`; headers go above it",
                case.name
            ));
        }
        match key {
            "fragment" => case.fragment = true,
            "text" => case.text = Some(value.to_string()),
            "diagnostics" => {
                case.diagnostics = Some(
                    value
                        .parse()
                        .map_err(|_| format!("{file}:{number}: `diagnostics:` takes a count"))?,
                );
            }
            "edition" => {
                case.edition = byard_compiler::edition::Edition::parse(value)
                    .map_err(|e| format!("{file}:{number}: {e}"))?;
            }
            "wait" => {
                case.wait = value
                    .parse()
                    .map_err(|_| format!("{file}:{number}: `wait:` takes milliseconds"))?;
            }
            "error" if !value.is_empty() => {
                case.errors.insert(value.to_string());
            }
            "warning" if !value.is_empty() => {
                case.warnings.insert(value.to_string());
            }
            "gap" if !value.is_empty() => case.gap = Some(value.to_string()),
            _ => return Err(format!("{file}:{number}: unknown header `//# {directive}`")),
        }
    }
    Ok(cases)
}

fn all_cases() -> Vec<Case> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(cases_dir())
        .expect("tests/conformance exists")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "byd"))
        .collect();
    files.sort();
    let mut cases = Vec::new();
    for path in files {
        let name = file_name(&path);
        let text = std::fs::read_to_string(&path).expect("a case file reads");
        match cases_of(&name, &text) {
            Ok(found) => cases.extend(found),
            Err(problem) => panic!("{problem}"),
        }
    }
    cases
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

struct NoPackages;

impl PackageProvider for NoPackages {
    fn package_files(&mut self, _: &str, _: &str) -> Result<Vec<SourceFile>, String> {
        Err("a conformance case has no `[dependencies]`".to_string())
    }
}

/// What a program did: the diagnostics of `byard check`, and what it drew.
#[derive(Debug, Default)]
struct Outcome {
    errors: BTreeSet<String>,
    warnings: BTreeSet<String>,
    /// Each diagnostic as `byard check` prints its first line.
    said: Vec<String>,
    /// The texts of the first frame, when the check found no error.
    text: Option<String>,
}

fn resolve(source: &str, edition: byard_compiler::edition::Edition) -> ResolvedProgram {
    resolve_program_in(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: source.to_string(),
        }],
        &mut NoPackages,
        edition,
    )
}

/// The texts `Main` draws, run the way an app runs it: mounted, ticked so
/// that `on mount` and what it wrote have settled, then `wait` milliseconds of
/// the app clock a frame at a time, and rendered.
fn drawn(program: &ResolvedProgram, wait: u32) -> Option<String> {
    let view = root_view(&program.views)?;
    let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    interp.set_theme(Theme::byard_base());
    interp.declare_controllers(&byard_core::cap::provided_names());
    interp.use_virtual_timers();
    interp.load_views(&program.views);
    let tree = interp.lower_view(view, &known);
    let mut frame = RenderFrame::new();
    let mut settle = |interp: &mut Interpreter| {
        for _ in 0..3 {
            interp.tick();
            frame.clear();
            interp.render(&tree, &mut frame, 400.0, 400.0);
        }
    };
    settle(&mut interp);
    let mut now = 0;
    while now < wait {
        now = (now + 16).min(wait);
        interp.set_now_ms(now);
        settle(&mut interp);
    }
    let texts: Vec<String> = frame.texts().iter().map(|t| t.text.clone()).collect();
    Some(texts.join(" | "))
}

fn run(source: &str, wait: u32, edition: byard_compiler::edition::Edition) -> Outcome {
    let program = resolve(source, edition);
    let found = byard_project::check::check_program(&program, Theme::byard_base());
    let mut outcome = Outcome::default();
    for diagnostic in &found {
        let kind = diagnostic.kind().to_string();
        outcome
            .said
            .push(program.source_map.render_line(diagnostic));
        if diagnostic.is_warning() {
            outcome.warnings.insert(kind);
        } else {
            outcome.errors.insert(kind);
        }
    }
    if outcome.errors.is_empty() {
        outcome.text = drawn(&program, wait);
    }
    outcome
}

/// Why `case` does not hold, or `None` when it does.
fn verdict(case: &Case) -> Option<String> {
    let source = case.program();
    let wait = case.wait;
    let edition = case.edition;
    let ran = std::panic::catch_unwind(|| run(&source, wait, edition));
    let Ok(outcome) = ran else {
        return Some("the checker or the interpreter panicked".to_string());
    };
    let mut wrong = String::new();
    if outcome.errors != case.errors {
        let _ = writeln!(
            wrong,
            "    errors: expected {:?}, found {:?}",
            case.errors, outcome.errors
        );
    }
    if outcome.warnings != case.warnings {
        let _ = writeln!(
            wrong,
            "    warnings: expected {:?}, found {:?}",
            case.warnings, outcome.warnings
        );
    }
    if let Some(expected) = case.diagnostics
        && outcome.said.len() != expected
    {
        let _ = writeln!(
            wrong,
            "    diagnostics: expected {expected}, found {}",
            outcome.said.len()
        );
    }
    if let Some(expected) = &case.text {
        match &outcome.text {
            Some(text) if text == expected => {}
            Some(text) => {
                let _ = writeln!(wrong, "    text: expected {expected:?}, found {text:?}");
            }
            None => {
                let _ = writeln!(
                    wrong,
                    "    text: expected {expected:?}, and nothing was drawn"
                );
            }
        }
    }
    if wrong.is_empty() {
        return None;
    }
    for line in &outcome.said {
        let _ = writeln!(wrong, "      {line}");
    }
    Some(wrong.trim_end().to_string())
}

#[test]
fn every_case_has_a_name_of_its_own_and_says_what_it_expects() {
    let cases = all_cases();
    assert!(cases.len() >= 400, "found the cases: {}", cases.len());
    let mut seen = BTreeSet::new();
    for case in &cases {
        assert!(
            seen.insert((case.file.clone(), case.name.clone())),
            "{}: a second case of that name in the file",
            case.at()
        );
        assert!(!case.source.trim().is_empty(), "{}: no code", case.at());
        assert!(
            case.gap.is_none()
                || case.text.is_some()
                || !case.errors.is_empty()
                || !case.warnings.is_empty(),
            "{}: a gap has to say what should happen (`text:`, `error:` or `warning:`)",
            case.at()
        );
    }
}

#[test]
fn the_language_does_what_the_reference_says() {
    let cases = all_cases();
    let mut failures = Vec::new();
    let mut held = 0usize;
    for case in cases.iter().filter(|case| case.gap.is_none()) {
        match verdict(case) {
            None => held += 1,
            Some(why) => failures.push(format!("{}\n{why}", case.at())),
        }
    }
    println!("conformance: {held} cases hold");
    assert!(
        failures.is_empty(),
        "{} conformance case(s) do not hold:\n\n{}\n",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn every_gap_is_still_a_gap() {
    let cases = all_cases();
    let mut closed = Vec::new();
    let mut open = Vec::new();
    for case in cases.iter().filter(|case| case.gap.is_some()) {
        match verdict(case) {
            None => closed.push(case.at()),
            Some(_) => open.push(case),
        }
    }
    println!("conformance: {} gaps open", open.len());
    for case in &open {
        println!(
            "  gap  {}: {}",
            case.at(),
            case.gap.as_deref().unwrap_or_default()
        );
    }
    assert!(
        closed.is_empty(),
        "{} gap(s) now do what they should. Delete their `//# gap:` line, and take them off \
         docs/language/roadmap.md:\n  {}\n",
        closed.len(),
        closed.join("\n  ")
    );
}

#[test]
fn the_harness_reads_headers_and_refuses_a_typo() {
    let text = "// a comment before\n//# case: one\n//# fragment\n//# text: a | b\nText(\"a\")\n\n\
                //# case: two\n//# error: TypeMismatch\n//# gap: not yet\nView Main() {}\n";
    let cases = cases_of("x.byd", text).expect("well formed");
    assert_eq!(cases.len(), 2);
    assert!(cases[0].fragment);
    assert_eq!(cases[0].text.as_deref(), Some("a | b"));
    assert_eq!(cases[0].program(), "View Main() {\nText(\"a\")\n\n\n}\n");
    assert_eq!(cases[1].gap.as_deref(), Some("not yet"));
    assert!(cases[1].errors.contains("TypeMismatch"));

    let typo = cases_of("x.byd", "//# case: one\n//# txet: a\nView Main() {}\n");
    assert!(typo.is_err_and(|e| e.contains("unknown header")));
    let late = cases_of("x.byd", "//# case: one\nView Main() {}\n//# text: a\n");
    assert!(late.is_err_and(|e| e.contains("headers go above")));
    let orphan = cases_of("x.byd", "View Main() {}\n");
    assert!(orphan.is_err_and(|e| e.contains("before the first")));
}

#[test]
fn the_harness_tells_a_case_that_holds_from_one_that_does_not() {
    let holds = Case {
        name: "holds".into(),
        source: "Text(\"{1 + 1}\")".into(),
        fragment: true,
        text: Some("2".into()),
        ..Case::default()
    };
    assert_eq!(verdict(&holds), None);

    let wrong_text = Case {
        text: Some("3".into()),
        ..holds.clone()
    };
    assert!(verdict(&wrong_text).is_some_and(|why| why.contains("expected \"3\", found \"2\"")));

    let missing_error = Case {
        errors: BTreeSet::from(["TypeMismatch".to_string()]),
        ..holds
    };
    assert!(verdict(&missing_error).is_some_and(|why| why.contains("errors: expected")));

    let reports = Case {
        name: "reports".into(),
        source: "Text(\"{nope}\")".into(),
        fragment: true,
        errors: BTreeSet::from(["UnknownName".to_string()]),
        ..Case::default()
    };
    assert_eq!(verdict(&reports), None);
}

/// A NUL in a case file would make git call the whole file binary, so this
/// one lives here.
#[test]
fn a_nul_character_is_an_error_and_not_a_panic() {
    let case = Case {
        name: "nul".into(),
        source: "Text(\"a\") \0".into(),
        fragment: true,
        errors: BTreeSet::from(["UnexpectedChar".to_string()]),
        ..Case::default()
    };
    assert_eq!(verdict(&case), None);
}

/// The anchor GitHub gives a heading: what the guide's own links use.
fn anchor(heading: &str) -> String {
    heading
        .trim()
        .chars()
        .filter_map(|c| match c {
            c if c.is_alphanumeric() => Some(c.to_ascii_lowercase()),
            '-' | '_' => Some(c),
            c if c.is_whitespace() => Some('-'),
            _ => None,
        })
        .collect()
}

/// The sections of the language reference, `##` and `###`, by anchor.
fn reference_sections() -> BTreeSet<String> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/guide/src/language-reference.md");
    let text = std::fs::read_to_string(&path).expect("the language reference");
    let mut in_code = false;
    let mut out = BTreeSet::new();
    for line in text.lines() {
        if line.starts_with("```") {
            in_code = !in_code;
        }
        if in_code {
            continue;
        }
        if let Some(heading) = line
            .strip_prefix("### ")
            .or_else(|| line.strip_prefix("## "))
        {
            out.insert(anchor(heading));
        }
    }
    out
}

/// What the reference promises is what the suite checks: the sections each file
/// of cases says it covers (`//# covers: lists, maps-and-sets`) must all be
/// sections, and a section no file covers must be in `uncovered.txt`, which can
/// only get shorter. A section is the unit of a rule; it is covered when a file
/// of cases is about it, and the cases in that file are what says it is so.
#[test]
fn every_section_of_the_reference_has_cases() {
    let sections = reference_sections();
    let mut covered: BTreeSet<String> = BTreeSet::new();
    let mut problems = Vec::new();
    for entry in std::fs::read_dir(cases_dir()).expect("conformance dir") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "byd") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read case file");
        for line in text.lines() {
            let Some(list) = line.strip_prefix("//# covers:") else {
                continue;
            };
            for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                if sections.contains(name) {
                    covered.insert(name.to_string());
                } else {
                    problems.push(format!(
                        "{}: `covers: {name}` is not a section of the language reference",
                        file_name(&path)
                    ));
                }
            }
        }
    }
    let listed: BTreeSet<String> = std::fs::read_to_string(cases_dir().join("uncovered.txt"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect();
    for name in &listed {
        if !sections.contains(name) {
            problems.push(format!("uncovered.txt: `{name}` is not a section any more"));
        } else if covered.contains(name) {
            problems.push(format!(
                "uncovered.txt: `{name}` has cases now: take it off the list"
            ));
        }
    }
    for name in sections.difference(&covered) {
        if !listed.contains(name) {
            problems.push(format!(
                "the section `{name}` of the reference has no cases: write some and add \
                 `//# covers: {name}` to their file, or list it in uncovered.txt"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    println!(
        "conformance: {} of {} sections of the reference have cases",
        covered.len(),
        sections.len()
    );
}
