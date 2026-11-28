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
pub(crate) struct Case {
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
    pub(crate) fn at(&self) -> String {
        format!("{}:{} `{}`", self.file, self.line, self.name)
    }

    pub(crate) fn program(&self) -> String {
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

pub(crate) fn all_cases() -> Vec<Case> {
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
// ---- past the parser ----------------------------------------------------
//
// The fuzzing in `byard-compiler` feeds random text to the lexer and the
// parser. These go further: the programs of the suite, formatted, mutated and
// written afresh, go through the checker and the interpreter, which must tell
// a person what is wrong and never panic.

/// A small deterministic generator, so a failure is the same failure next time.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n.max(1) as u64).unwrap_or(0)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Runs a program through everything `byard check` does and draws it,
/// returning whether it was drawn (it had no error), or the panic message if
/// it panicked.
fn survives(source: &str, edition: byard_compiler::edition::Edition) -> Result<bool, String> {
    let source = source.to_string();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(&source, 0, edition).text.is_some()
    }))
    .map_err(|payload| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_else(|| "a panic".to_string())
    })
}

/// Every program of the suite formats to the same program, and formatting what
/// it gave changes nothing. A case that does not parse has nothing to format.
#[test]
fn every_case_formats_to_a_fixed_point_with_the_same_tree() {
    let mut formatted = 0;
    let mut problems = Vec::new();
    for case in all_cases() {
        let source = case.program();
        match byard_compiler::fmt::format(&source) {
            Ok(once) => {
                formatted += 1;
                match byard_compiler::fmt::format(&once) {
                    Ok(twice) if twice == once => {}
                    Ok(_) => problems.push(format!("{}: formatting twice changes it", case.at())),
                    Err(e) => {
                        problems.push(format!("{}: formatted text is refused: {e}", case.at()));
                    }
                }
            }
            Err(byard_compiler::fmt::FmtError::Syntax { .. }) => {}
            Err(e) => problems.push(format!("{}: {e}", case.at())),
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    println!("conformance: {formatted} programs formatted to a fixed point");
}

/// The programs of the suite, damaged a few hundred ways each: a line gone,
/// two swapped, a token put where it does not go, the end cut off. Each is
/// told or runs; none panics.
#[test]
fn damaged_cases_are_told_and_never_panic() {
    const TOKENS: [&str; 24] = [
        "{", "}", "(", ")", "[", "]", "=>", ":", ",", "..", "as", "?.", "??", "\"\"\"", "match",
        "for", "store", "fn", "return", "none", "0..3", "<T>", ".", "|",
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut runs = 0;
    let mut failures = Vec::new();
    for case in all_cases() {
        let program = case.program();
        let lines: Vec<&str> = program.lines().collect();
        for _ in 0..6 {
            let mut damaged: Vec<String> = lines.iter().map(ToString::to_string).collect();
            match rng.below(6) {
                0 if !damaged.is_empty() => {
                    let at = rng.below(damaged.len());
                    damaged.remove(at);
                }
                1 if damaged.len() > 1 => {
                    let (a, b) = (rng.below(damaged.len()), rng.below(damaged.len()));
                    damaged.swap(a, b);
                }
                2 if !damaged.is_empty() => {
                    let at = rng.below(damaged.len());
                    let line = damaged[at].clone();
                    damaged.insert(at, line);
                }
                3 if !damaged.is_empty() => {
                    let at = rng.below(damaged.len());
                    let line = &mut damaged[at];
                    let mut cut = rng.below(line.len() + 1);
                    while !line.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    line.insert_str(cut, rng.pick(&TOKENS));
                }
                4 if !damaged.is_empty() => {
                    let at = rng.below(damaged.len());
                    let line = &mut damaged[at];
                    let mut cut = rng.below(line.len() + 1);
                    while !line.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    line.truncate(cut);
                }
                _ => {
                    let keep = rng.below(damaged.len() + 1);
                    damaged.truncate(keep);
                }
            }
            let source = damaged.join("\n");
            runs += 1;
            if let Err(message) = survives(&source, case.edition) {
                failures.push(format!(
                    "{} damaged panics: {message}\n--- program ---\n{source}",
                    case.at()
                ));
                if failures.len() >= 5 {
                    break;
                }
            }
        }
        if failures.len() >= 5 {
            break;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    println!("conformance: {runs} damaged programs told or run, none panicked");
}

/// Programs written afresh from the grammar. An expression is written for the
/// type that is wanted, so most of what comes out checks and runs, and now and
/// then one of any shape is put in its place, so the checker's refusals are
/// reached too.
fn gen_wild(rng: &mut Rng, depth: u32) -> String {
    const ATOMS: [&str; 12] = [
        "1", "0", "-3", "2.5", "\"hi\"", "true", "none", "n", "xs", "m", "s", "u",
    ];
    const BINARY: [&str; 12] = [
        "+", "-", "*", "/", "%", "==", "<", "&&", "||", "??", "..", "!=",
    ];
    if depth == 0 || rng.below(3) == 0 {
        return (*rng.pick(&ATOMS)).to_string();
    }
    let a = gen_wild(rng, depth - 1);
    let b = gen_wild(rng, depth - 1);
    match rng.below(6) {
        0 | 1 => format!("({a} {} {b})", rng.pick(&BINARY)),
        2 => format!("({a} ? {b} : {a})"),
        3 => format!("{a}[{b}]"),
        4 => format!("({a} as P)"),
        _ => format!("{a}.len"),
    }
}

fn gen_int(rng: &mut Rng, depth: u32) -> String {
    if depth == 0 {
        return (*rng.pick(&["0", "1", "7", "-2", "n", "xs.len", "m.len", "s.len"])).to_string();
    }
    let a = gen_int(rng, depth - 1);
    let b = gen_int(rng, depth - 1);
    match rng.below(14) {
        0 => format!("({a} + {b})"),
        1 => format!("({a} - {b})"),
        2 => format!("({a} * {b})"),
        3 => format!("({a} / {b})"),
        4 => format!("({a} % {b})"),
        5 => format!("({} ? {a} : {b})", gen_bool(rng, depth - 1)),
        6 => format!("{}.sum()", gen_list(rng, depth - 1)),
        7 => format!("({}.first() ?? {a})", gen_list(rng, depth - 1)),
        8 => format!("({}.get({a}) ?? {b})", gen_list(rng, depth - 1)),
        9 => format!("({}[\"k\"] ?? {a})", "m"),
        10 => format!("{}.count(x => x > {a})", gen_list(rng, depth - 1)),
        11 => format!("f({a})"),
        12 => format!("first({})", gen_list(rng, depth - 1)),
        _ => format!("(p.x + {a})"),
    }
}

fn gen_bool(rng: &mut Rng, depth: u32) -> String {
    if depth == 0 {
        return (*rng.pick(&["true", "false", "(n > 0)", "(u == none)"])).to_string();
    }
    let a = gen_int(rng, depth - 1);
    let b = gen_int(rng, depth - 1);
    match rng.below(7) {
        0 => format!("({a} < {b})"),
        1 => format!("({a} == {b})"),
        2 => format!(
            "({} && {})",
            gen_bool(rng, depth - 1),
            gen_bool(rng, depth - 1)
        ),
        3 => format!(
            "({} || {})",
            gen_bool(rng, depth - 1),
            gen_bool(rng, depth - 1)
        ),
        4 => format!("{}.any(x => x > {a})", gen_list(rng, depth - 1)),
        5 => format!("{}.contains({a})", gen_list(rng, depth - 1)),
        _ => format!("(!{})", gen_bool(rng, depth - 1)),
    }
}

fn gen_str(rng: &mut Rng, depth: u32) -> String {
    if depth == 0 {
        return (*rng.pick(&["\"a\"", "s", "p.y", "(u ?? \"-\")"])).to_string();
    }
    match rng.below(6) {
        0 => format!("(\"{{{}}}\")", gen_int(rng, depth - 1)),
        1 => format!(
            "({} + {})",
            gen_str(rng, depth - 1),
            gen_str(rng, depth - 1)
        ),
        2 => format!("{}.join(\",\")", gen_list(rng, depth - 1)),
        3 => format!("{}.toUpper()", gen_str(rng, depth - 1)),
        4 => format!("\"{{{}:>5}}\"", gen_int(rng, depth - 1)),
        _ => format!(
            "{}.slice({}, {})",
            gen_str(rng, depth - 1),
            gen_int(rng, 0),
            gen_int(rng, 0)
        ),
    }
}

fn gen_list(rng: &mut Rng, depth: u32) -> String {
    if depth == 0 {
        return (*rng.pick(&["xs", "[1, 2]", "[]", "(0..3)"])).replace("[]", "xs");
    }
    let l = gen_list(rng, depth - 1);
    match rng.below(9) {
        0 => format!("{l}.push({})", gen_int(rng, depth - 1)),
        1 => format!("{l}.map(x => x + {})", gen_int(rng, 0)),
        2 => format!("{l}.filter(x => x > {})", gen_int(rng, 0)),
        3 => format!("{l}.insertAt({}, {})", gen_int(rng, 0), gen_int(rng, 0)),
        4 => format!("{l}.updateAt({}, {})", gen_int(rng, 0), gen_int(rng, 0)),
        5 => format!("{l}.reverse()"),
        6 => format!("({} .. {})", gen_int(rng, 0), gen_int(rng, 0)),
        7 => format!("{l}.slice({}, {})", gen_int(rng, 0), gen_int(rng, 0)),
        _ => format!("({l} + {})", gen_list(rng, depth - 1)),
    }
}

fn generated_program(rng: &mut Rng) -> String {
    let mut out = String::from(
        "type P = { x: Int, y: Str }\nenum E { a, b(n: Int) }\n\
         fn f(a: Int) -> Int => a\nfn first<T>(xs: List<T>) -> T => xs[0]\n\
         View Main() {\n    var n = 1\n    var xs = [1, 2, 3]\n    var m: Map<Str, Int> = [\"k\": 1]\n    \
         var s = \"hi\"\n    var u: Str? = none\n    var p: P = { x: 1, y: \"a\" }\n    var e = E.b(n: 2)\n",
    );
    for _ in 0..(2 + rng.below(5)) {
        let wild = rng.below(8) == 0;
        let int = if wild {
            gen_wild(rng, 3)
        } else {
            gen_int(rng, 3)
        };
        let line = match rng.below(12) {
            0 => format!("    Text(\"{{{int}}}\")\n"),
            1 => format!("    Text({})\n", gen_str(rng, 3)),
            2 => format!("    on mount => {{ n = {int} }}\n"),
            3 => format!(
                "    on mount => {{ for i in {} {{ n += i }} }}\n",
                gen_list(rng, 2)
            ),
            4 => format!("    for i in {} {{ Text(\"{{i}}\") }}\n", gen_list(rng, 2)),
            5 => format!(
                "    when {} {{ Text(\"a\") }} else {{ Text(\"b\") }}\n",
                gen_bool(rng, 3)
            ),
            6 => format!("    on mount => {{ xs = {} }}\n", gen_list(rng, 3)),
            7 => format!(
                "    on mount => {{ xs[{}] = {int}\n if {} {{ return }} }}\n",
                gen_int(rng, 1),
                gen_bool(rng, 2)
            ),
            8 => format!(
                "    on mount => {{ m = m.set({}, {int}) }}\n",
                gen_str(rng, 1)
            ),
            9 => {
                format!("    match e {{ .a => Text(\"a\")\n .b(k) => Text(\"{{k + {int}}}\") }}\n")
            }
            10 => format!("    Text(\"{{{}}}\")\n", gen_bool(rng, 3)),
            _ => format!("    Text(\"{{p.x + {int}}} {{(p as P).y}}\")\n"),
        };
        out.push_str(&line);
    }
    out.push_str("}\n");
    out
}

/// Programs written from the grammar, many of them wrong in what they mean,
/// in both editions: the checker says so or the interpreter runs them, and
/// neither panics.
#[test]
fn generated_programs_are_told_or_run_and_never_panic() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut failures = Vec::new();
    let mut runs = 0;
    let mut drawn = 0;
    for _ in 0..1200 {
        let source = generated_program(&mut rng);
        for edition in [
            byard_compiler::edition::Edition::BASELINE,
            byard_compiler::edition::Edition::E2027,
        ] {
            runs += 1;
            match survives(&source, edition) {
                Ok(true) => drawn += 1,
                Ok(false) => {}
                Err(message) => {
                    failures.push(format!("panics: {message}\n--- program ---\n{source}"));
                }
            }
        }
        if failures.len() >= 3 {
            break;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    // A generator whose programs are all refused tests the checker's first
    // error and nothing past it.
    assert!(
        drawn * 20 >= runs,
        "only {drawn} of {runs} generated programs got as far as being drawn"
    );
    println!("conformance: {runs} generated programs told or run ({drawn} drawn), none panicked");
}

// ---- the promise --------------------------------------------------------
//
// A program that checks under an edition keeps its meaning for as long as that
// edition exists. The suite is the test of that, and what it can be a test of
// is a suite that does not change: once an edition is named stable, the cases
// written in it are locked, and a change to one, or its removal, is a change
// to the language, which is a new edition.

/// FNV-1a: a hash that is the same on every machine and every compiler, which
/// the standard library's is not promised to be.
fn fnv1a(text: &str) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325_u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01B3);
    }
    hash
}

/// What a case is: its name, the edition it is written in, everything it
/// expects and the program. A header written in another order is the same case.
fn fingerprint(case: &Case) -> String {
    let join = |set: &BTreeSet<String>| set.iter().cloned().collect::<Vec<_>>().join(",");
    format!(
        "{:016x}",
        fnv1a(&format!(
            "{}\n{}\n{}\n{:?}\n{}\n{:?}\n{}\n{}\n{}",
            case.name,
            case.edition.as_str(),
            case.fragment,
            case.text,
            case.wait,
            case.diagnostics,
            join(&case.errors),
            join(&case.warnings),
            case.source
        ))
    )
}

fn frozen_dir() -> PathBuf {
    cases_dir().join("frozen")
}

/// The lock of an edition: `file:name<TAB>fingerprint` for each of its cases.
fn lock_of(edition: byard_compiler::edition::Edition) -> String {
    let mut lines: Vec<String> = all_cases()
        .iter()
        .filter(|c| c.edition == edition && c.gap.is_none())
        .map(|c| format!("{}:{}\t{}", c.file, c.name, fingerprint(c)))
        .collect();
    lines.sort();
    lines.join("\n") + "\n"
}

/// An edition with a lock keeps every case it had, exactly as written. New
/// cases may join it: they are the language being found to do what it does.
#[test]
fn a_frozen_edition_keeps_its_cases() {
    let Ok(entries) = std::fs::read_dir(frozen_dir()) else {
        return;
    };
    let mut problems = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "lock") {
            continue;
        }
        let label = path.file_stem().unwrap().to_string_lossy().into_owned();
        let Ok(edition) = byard_compiler::edition::Edition::parse(&label) else {
            problems.push(format!("{}: `{label}` is not an edition", path.display()));
            continue;
        };
        let now: std::collections::BTreeMap<String, String> = all_cases()
            .iter()
            .filter(|c| c.edition == edition)
            .map(|c| (format!("{}:{}", c.file, c.name), fingerprint(c)))
            .collect();
        for line in std::fs::read_to_string(&path).unwrap_or_default().lines() {
            let Some((name, locked)) = line.split_once('\t') else {
                continue;
            };
            match now.get(name) {
                None => problems.push(format!(
                    "edition {label} is frozen and `{name}` is gone: a rule an edition had is a \
                     rule it keeps, so a change goes in a new edition"
                )),
                Some(found) if found != locked => problems.push(format!(
                    "edition {label} is frozen and `{name}` was changed: a case of a frozen \
                     edition is not edited, a change goes in a new edition"
                )),
                Some(_) => {}
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Writes the lock of the edition named in `BYARD_FREEZE`, which is how an
/// edition is frozen:
///
/// ```text
/// BYARD_FREEZE=2026 cargo test -p byard-cli --test it write_the_lock -- --ignored
/// ```
#[test]
#[ignore = "run on purpose, to freeze an edition"]
fn write_the_lock_of_an_edition() {
    let label = std::env::var("BYARD_FREEZE").expect("BYARD_FREEZE names the edition");
    let edition = byard_compiler::edition::Edition::parse(&label).expect("an edition");
    std::fs::create_dir_all(frozen_dir()).expect("frozen dir");
    let path = frozen_dir().join(format!("{}.lock", edition.as_str()));
    std::fs::write(&path, lock_of(edition)).expect("write the lock");
    println!("froze {}", path.display());
}

/// The lock of an edition is what `lock_of` writes: a case changed after it was
/// written is told, and one added is not.
#[test]
fn the_lock_tells_a_changed_case_from_a_new_one() {
    let cases = all_cases();
    let mut one = cases
        .iter()
        .find(|c| c.edition == byard_compiler::edition::Edition::BASELINE && c.gap.is_none())
        .expect("a baseline case")
        .clone();
    let before = fingerprint(&one);
    one.source.push_str("// changed\n");
    assert_ne!(
        before,
        fingerprint(&one),
        "a changed program is a changed case"
    );
    one.source = one.source.replace("// changed\n", "");
    assert_eq!(before, fingerprint(&one));
    one.errors.insert("Anything".to_string());
    assert_ne!(
        before,
        fingerprint(&one),
        "what it expects is part of the case"
    );
    let lock = lock_of(byard_compiler::edition::Edition::BASELINE);
    assert!(
        lock.lines().count() > 600,
        "the baseline has most of the suite"
    );
}
