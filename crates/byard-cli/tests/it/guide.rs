//! The user guide (`docs/guide`) is checked like code.
//!
//! A guide whose samples stop compiling teaches the wrong thing, and nobody
//! notices until a newcomer pastes one. So every `byld` block in
//! `docs/guide/src/**/*.md` is run through the same steps `byard check` runs
//! (resolve, type check, lower and render every view), and the build fails,
//! naming the file, the line and the diagnostics, when one of them does not
//! hold.
//!
//! # The fence tags
//!
//! | Fence                  | What the test requires                                   |
//! |------------------------|----------------------------------------------------------|
//! | ```` ```byld ````          | A complete file. Zero diagnostics, warnings included.   |
//! | ```` ```byld,fragment ```` | Statements or elements. Wrapped in `View Main() { … }`, then as above. |
//! | ```` ```byld,fails ````    | At least one diagnostic. For showing what an error looks like. |
//! | ```` ```byld,ignore ````   | Not checked. Say why in a comment beside it.            |
//!
//! Two more tags keep the prose honest:
//!
//! - ```` ```diagnostic ```` straight after a `fails` block quotes the
//!   compiler's words. Every non-empty line must appear in a diagnostic the
//!   block really produces, as `error[Kind]: message`.
//! - ```` ```toml ```` must parse as TOML (`toml,ignore` is skipped).
//! - ```` ```rust,ignore,excerpt ```` is a piece of real Rust: every non-blank
//!   line must appear, as written, in a `.rs` file of the repository or in the
//!   README. (`ignore` keeps mdBook from compiling it; `excerpt` is the tag this
//!   test reads.) A line that is only `// …` marks something left out.
//!
//! # The book
//!
//! `book.toml` and `SUMMARY.md` must exist, every chapter file must be listed
//! in `SUMMARY.md`, and every relative link must point at a file that exists,
//! at a heading that exists when it names one.
//! A link to a file outside the book is written as a GitHub URL under
//! `https://github.com/Briany4717/byard/blob/main/` (or `tree/main`), because
//! the rendered book cannot reach the repository's files; the test checks
//! that the path is there.
//!
//! `docs/guide/CONTRIBUTING.md` says the same in the words of someone writing
//! a chapter.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use byard_compiler::CompileError;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::resolve::{PackageProvider, SourceFile, resolve_program};

/// The repository, as the book links to it.
const REPO_URL: &str = "https://github.com/Briany4717/byard/";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

fn guide_dir() -> PathBuf {
    repo_root().join("docs/guide")
}

// ── Reading the book ────────────────────────────────────────────────────────

/// How a `byld` fence asks to be treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// A complete file with no diagnostics.
    Whole,
    /// Statements wrapped in `View Main() { … }`, with no diagnostics.
    Fragment,
    /// At least one diagnostic.
    Fails,
    /// Not checked.
    Ignore,
}

/// One fenced block of a chapter.
#[derive(Debug)]
struct Block {
    /// The chapter, relative to the guide's `src/`.
    file: String,
    /// The line of the opening fence, from 1.
    line: usize,
    /// The info string after the fence, split at commas.
    tags: Vec<String>,
    /// The text between the fences, without the fence's own indentation.
    body: String,
}

impl Block {
    fn language(&self) -> &str {
        self.tags.first().map_or("", String::as_str)
    }

    fn has(&self, tag: &str) -> bool {
        self.tags.iter().skip(1).any(|t| t == tag)
    }

    /// Where the block is, as an editor would jump to it.
    fn place(&self) -> String {
        format!("docs/guide/src/{}:{}", self.file, self.line)
    }
}

/// Every `.md` file under `dir`, sorted, as paths relative to `dir`.
fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
            .map(|e| e.expect("a directory entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, base, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path.strip_prefix(base).expect("under base").to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

/// The fenced blocks of one chapter, in order.
///
/// A fence is three or more backticks; it closes at a line of at least as
/// many. Blocks may be indented (inside a list); the fence's indentation is
/// taken off every line of the body.
fn blocks_of(file: &str, text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    // (backticks, indent, block being read)
    let mut open: Option<(usize, usize, Block)> = None;
    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let trimmed = raw.trim_start();
        let indent = raw.len() - trimmed.len();
        let ticks = trimmed.chars().take_while(|&c| c == '`').count();
        match &mut open {
            None if ticks >= 3 => {
                let info = trimmed[ticks..].trim();
                let tags = info
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                open = Some((
                    ticks,
                    indent,
                    Block {
                        file: file.to_string(),
                        line: line_no,
                        tags,
                        body: String::new(),
                    },
                ));
            }
            Some((n, _, _)) if ticks >= *n && trimmed[ticks..].trim().is_empty() => {
                let (_, _, block) = open.take().expect("open");
                blocks.push(block);
            }
            Some((_, indent, block)) => {
                let cut = raw
                    .char_indices()
                    .take(*indent)
                    .take_while(|(_, c)| c.is_whitespace())
                    .count();
                block.body.push_str(&raw[cut..]);
                block.body.push('\n');
            }
            None => {}
        }
    }
    assert!(
        open.is_none(),
        "{file}: a code fence opened at line {} is never closed",
        open.as_ref().map_or(0, |(_, _, b)| b.line)
    );
    blocks
}

/// Every block of every chapter.
fn all_blocks() -> Vec<Block> {
    let src = guide_dir().join("src");
    let mut blocks = Vec::new();
    for rel in markdown_files(&src) {
        let text = std::fs::read_to_string(src.join(&rel)).expect("a readable chapter");
        blocks.extend(blocks_of(&rel.to_string_lossy().replace('\\', "/"), &text));
    }
    blocks
}

// ── Checking a sample ───────────────────────────────────────────────────────

/// A bare file has no `[dependencies]`, so a `use` in a sample is an error
/// that says so (what `byard check file.byd` does).
struct NoPackages;

impl PackageProvider for NoPackages {
    fn package_files(&mut self, _: &str, _: &str) -> Result<Vec<SourceFile>, String> {
        Err("a sample has no `[dependencies]`".to_string())
    }
}

/// The diagnostics of one complete file, as `file:line:col: error[Kind]:
/// message` lines.
///
/// The same steps as `commands::check` takes for a project, which is a
/// binary's private module and so cannot be called from here: resolve the
/// program (parse and type check), then lower and render every view into a
/// throwaway frame, so the checks that run while lowering are exercised too.
/// If `check_program_with_theme` changes, change this with it.
fn diagnostics_of(src: &str) -> Vec<String> {
    let program = resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: src.to_string(),
        }],
        &mut NoPackages,
    );
    let errors: Vec<CompileError> = if program.errors.is_empty() {
        let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
        let mut interp = Interpreter::new();
        interp.set_theme(Theme::byard_base());
        interp.declare_controllers(&byard_core::cap::provided_names());
        interp.load_views(&program.views);
        let mut frame = byard_core::frame::RenderFrame::new();
        for view in &program.views {
            let tree = interp.lower_view(view, &known);
            interp.tick();
            interp.render(&tree, &mut frame, 1024.0, 768.0);
            frame.clear();
        }
        interp.errors().to_vec()
    } else {
        program.errors.clone()
    };
    errors
        .iter()
        .map(|err| program.source_map.render_line(err))
        .collect()
}

/// The text to check for a block, and how many lines the test added above it.
fn source_of(block: &Block, mode: Mode) -> (String, usize) {
    match mode {
        Mode::Fragment => (format!("View Main() {{\n{}}}\n", block.body), 1),
        _ => (block.body.clone(), 0),
    }
}

/// Moves the `main.byd:LINE:COL` of a diagnostic to the chapter's own line
/// numbers, so the message points at the file a reader opens.
fn in_chapter_lines(block: &Block, added: usize, diagnostic: &str) -> String {
    let Some(rest) = diagnostic.strip_prefix("main.byd:") else {
        return diagnostic.to_string();
    };
    let mut parts = rest.splitn(3, ':');
    let (Some(line), Some(col), Some(message)) = (parts.next(), parts.next(), parts.next()) else {
        return diagnostic.to_string();
    };
    let Ok(line) = line.parse::<usize>() else {
        return diagnostic.to_string();
    };
    let line = block.line + line.saturating_sub(added);
    format!("docs/guide/src/{}:{line}:{col}:{message}", block.file)
}

fn mode_of(block: &Block) -> Result<Mode, String> {
    let mut mode = Mode::Whole;
    for tag in block.tags.iter().skip(1) {
        mode = match tag.as_str() {
            "fragment" => Mode::Fragment,
            "fails" => Mode::Fails,
            "ignore" => Mode::Ignore,
            other => {
                return Err(format!(
                    "{}: unknown tag `{other}` on a byld block (use fragment, fails or ignore)",
                    block.place()
                ));
            }
        };
    }
    Ok(mode)
}

/// What the guide's `byld` samples came to.
#[derive(Default, Debug)]
struct Tally {
    checked: usize,
    fragments: usize,
    fails: usize,
    ignored: usize,
}

/// Runs every `byld` block, and every `diagnostic` block that follows a
/// `fails` one. Returns what was checked, or every problem found.
fn check_samples(blocks: &[Block]) -> Result<Tally, String> {
    let mut problems = String::new();
    let mut tally = Tally::default();
    // The messages the previous block produced, for a `diagnostic` block that
    // quotes them.
    let mut shown: Option<(usize, Vec<String>)> = None;

    for (index, block) in blocks.iter().enumerate() {
        if block.language() == "diagnostic" {
            let follows = shown.as_ref().is_some_and(|(prev, _)| prev + 1 == index);
            let Some((_, produced)) = shown.as_ref().filter(|_| follows) else {
                let _ = writeln!(
                    problems,
                    "{}: a `diagnostic` block must come straight after a `byld,fails` block",
                    block.place()
                );
                continue;
            };
            for quoted in block.body.lines().map(str::trim).filter(|l| !l.is_empty()) {
                if !produced.iter().any(|p| p.contains(quoted)) {
                    let _ = writeln!(
                        problems,
                        "{}: the guide quotes a diagnostic the sample does not produce:\n    {quoted}\n  it produces:\n{}",
                        block.place(),
                        indent_lines(produced)
                    );
                }
            }
            continue;
        }
        if block.language() != "byld" {
            continue;
        }
        let mode = match mode_of(block) {
            Ok(mode) => mode,
            Err(message) => {
                let _ = writeln!(problems, "{message}");
                continue;
            }
        };
        if mode == Mode::Ignore {
            tally.ignored += 1;
            continue;
        }
        let (src, added) = source_of(block, mode);
        let found = diagnostics_of(&src);
        match mode {
            Mode::Whole | Mode::Fragment => {
                if mode == Mode::Whole {
                    tally.checked += 1;
                } else {
                    tally.fragments += 1;
                }
                if !found.is_empty() {
                    let lines: Vec<String> = found
                        .iter()
                        .map(|d| in_chapter_lines(block, added, d))
                        .collect();
                    let _ = writeln!(
                        problems,
                        "{}: this byld sample does not check ({} diagnostic{}):\n{}\n  (a `byld,fails` fence is for a sample that is meant to be wrong)",
                        block.place(),
                        lines.len(),
                        if lines.len() == 1 { "" } else { "s" },
                        indent_lines(&lines)
                    );
                }
            }
            Mode::Fails => {
                tally.fails += 1;
                if found.is_empty() {
                    let _ = writeln!(
                        problems,
                        "{}: this `byld,fails` sample checks clean, so it shows nothing wrong",
                        block.place()
                    );
                }
                shown = Some((index, found));
            }
            Mode::Ignore => {}
        }
    }

    if problems.is_empty() {
        Ok(tally)
    } else {
        Err(problems)
    }
}

fn indent_lines(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── The tests ───────────────────────────────────────────────────────────────

/// The reason the guide exists as a test: every sample in it checks.
#[test]
fn every_byld_sample_in_the_guide_checks() {
    let blocks = all_blocks();
    let tally = match check_samples(&blocks) {
        Ok(tally) => tally,
        Err(problems) => panic!("\n{problems}"),
    };
    // A walk that found nothing would pass for the wrong reason.
    assert!(
        tally.checked + tally.fragments >= 40,
        "the guide should hold dozens of checked samples, the walk found {tally:?}"
    );
    eprintln!("guide: {tally:?}");
}

/// A `fails` sample that stops failing, and a `diagnostic` quote that stops
/// matching, are reported: the harness is tested against samples written here.
#[test]
fn the_harness_reports_what_it_should() {
    let chapter = |text: &str| blocks_of("probe.md", text);

    // A good file, a good fragment.
    let ok = chapter(
        "```byld\nView Main() { Text(\"hi\") }\n```\n\n```byld,fragment\nText(\"hi\")\n```\n",
    );
    assert!(check_samples(&ok).is_ok());

    // A broken file names the chapter, the line and the diagnostic.
    let broken = chapter("intro\n\n```byld\nView Main() {\n    Box #[colour: 1] {}\n}\n```\n");
    let said = check_samples(&broken).unwrap_err();
    assert!(said.contains("docs/guide/src/probe.md:3"), "{said}");
    assert!(said.contains("probe.md:5:"), "{said}");
    assert!(said.contains("UnknownAttribute"), "{said}");

    // A fragment's line numbers are the chapter's, not the wrapper's.
    let fragment = chapter("```byld,fragment\nText(\"a\")\nBox #[colour: 1] {}\n```\n");
    let said = check_samples(&fragment).unwrap_err();
    assert!(said.contains("probe.md:3:"), "{said}");

    // A `fails` sample must fail.
    let calm = chapter("```byld,fails\nView Main() { Text(\"hi\") }\n```\n");
    assert!(
        check_samples(&calm)
            .unwrap_err()
            .contains("shows nothing wrong")
    );

    // A quoted diagnostic must be real.
    let real = chapter(
        "```byld,fails\nView Main() { Box #[colour: 1] {} }\n```\n```diagnostic\nerror[UnknownAttribute]: unknown attribute `colour`\n```\n",
    );
    assert!(check_samples(&real).is_ok());
    let made_up = chapter(
        "```byld,fails\nView Main() { Box #[colour: 1] {} }\n```\n```diagnostic\nerror[NoSuchThing]: nope\n```\n",
    );
    assert!(
        check_samples(&made_up)
            .unwrap_err()
            .contains("does not produce")
    );

    // A tag the test does not know is not silently skipped.
    let typo = chapter("```byld,fial\nView Main() {}\n```\n");
    assert!(check_samples(&typo).unwrap_err().contains("unknown tag"));

    // `ignore` is the one way out, and it is counted.
    let skipped = chapter("```byld,ignore\nthis is not byld\n```\n");
    assert_eq!(check_samples(&skipped).unwrap().ignored, 1);

    // Fences inside a list are read, and an unrelated language is left alone.
    let nested = chapter(
        "- item\n\n  ```byld\n  View Main() { Text(\"x\") }\n  ```\n\n```rust\nfn x(\n```\n",
    );
    assert_eq!(check_samples(&nested).unwrap().checked, 1);
}

/// A `toml` block shows a `byard.toml`, so it has to be one.
#[test]
fn every_toml_block_in_the_guide_parses() {
    let mut problems = String::new();
    let mut seen = 0;
    for block in all_blocks()
        .iter()
        .filter(|b| b.language() == "toml" && !b.has("ignore"))
    {
        seen += 1;
        if let Err(e) = block.body.parse::<toml::Table>() {
            let _ = writeln!(problems, "{}: not valid TOML: {e}", block.place());
        }
    }
    assert!(problems.is_empty(), "\n{problems}");
    assert!(seen > 0, "the guide shows no byard.toml at all");
}

// ── Rust excerpts ───────────────────────────────────────────────────────────

/// Every trimmed line of every `.rs` file under `crates/` and `examples/` (a
/// doc comment's lines also without their `///`), and of the README, which is where the shortest `App` example lives.
fn repository_lines() -> std::collections::HashSet<String> {
    fn walk(dir: &Path, out: &mut std::collections::HashSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name();
                if name != "target" && !name.to_string_lossy().starts_with('.') {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    // A doc comment's code is code too: keep the line as it is
                    // and without its `///` or `//!`.
                    for line in text.lines().map(str::trim) {
                        out.insert(line.to_string());
                        if let Some(rest) = line
                            .strip_prefix("///")
                            .or_else(|| line.strip_prefix("//!"))
                        {
                            out.insert(rest.trim().to_string());
                        }
                    }
                }
            }
        }
    }
    let root = repo_root();
    let mut lines = std::collections::HashSet::new();
    walk(&root.join("crates"), &mut lines);
    walk(&root.join("examples"), &mut lines);
    if let Ok(readme) = std::fs::read_to_string(root.join("README.md")) {
        lines.extend(readme.lines().map(|l| l.trim().to_string()));
    }
    lines
}

/// The lines of the `rust,ignore,excerpt` blocks that are not in `known`, as
/// `place: line` messages.
fn missing_excerpt_lines(
    blocks: &[Block],
    known: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut missing = Vec::new();
    for block in blocks
        .iter()
        .filter(|b| b.language() == "rust" && b.has("excerpt"))
    {
        for (i, line) in block.body.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("// …") || known.contains(line) {
                continue;
            }
            missing.push(format!(
                "docs/guide/src/{}:{}: not a line of any Rust file in the repository: {line}",
                block.file,
                block.line + 1 + i
            ));
        }
    }
    missing
}

/// A Rust sample in the guide is a piece of code that builds in CI, quoted.
/// When the API it shows is renamed, the line no longer exists, and this fails.
#[test]
fn every_rust_excerpt_in_the_guide_is_real_code() {
    let blocks = all_blocks();
    let missing = missing_excerpt_lines(&blocks, &repository_lines());
    assert!(missing.is_empty(), "\n{}", missing.join("\n"));
    assert!(
        blocks
            .iter()
            .any(|b| b.language() == "rust" && b.has("excerpt")),
        "the guide quotes no Rust at all"
    );
}

#[test]
fn the_excerpt_check_names_the_line_it_cannot_find() {
    let known: std::collections::HashSet<String> =
        ["fn real() {}".to_string()].into_iter().collect();
    let blocks = blocks_of(
        "probe.md",
        "x\n\n```rust,ignore,excerpt\n    fn real() {}\n    // …\n\n    fn invented() {}\n```\n```rust,ignore\nfn unchecked() {}\n```\n",
    );
    let missing = missing_excerpt_lines(&blocks, &known);
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert!(missing[0].contains("probe.md:7"), "{missing:?}");
    assert!(missing[0].contains("fn invented() {}"), "{missing:?}");
}

// ── Links and the table of contents ─────────────────────────────────────────

/// The targets of the inline links (`[text](target)`) on one line, skipping
/// code spans, with an image's `!` ignored.
fn link_targets(line: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut in_code = false;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '`' => in_code = !in_code,
            ']' if !in_code && chars.get(i + 1) == Some(&'(') => {
                let start = i + 2;
                if let Some(len) = chars[start..].iter().position(|&c| c == ')') {
                    let target: String = chars[start..start + len].iter().collect();
                    // `[text](target "title")` keeps only the target.
                    let target = target.split_whitespace().next().unwrap_or("").to_string();
                    targets.push(target);
                    i = start + len;
                }
            }
            _ => {}
        }
        i += 1;
    }
    targets
}

/// The anchor mdBook gives a heading: lower case, spaces to `-`, and nothing
/// but letters, digits, `-` and `_` kept.
fn slug(heading: &str) -> String {
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

/// Whether `file` has a heading whose anchor is `fragment`.
fn has_heading(file: &Path, fragment: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(file) else {
        return false;
    };
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence
            && line.starts_with('#')
            && slug(line.trim_start_matches('#')) == fragment
        {
            return true;
        }
    }
    false
}

/// Why a link does not lead anywhere, or `None` when it does.
fn broken_link(chapter: &Path, target: &str) -> Option<String> {
    if target.is_empty() {
        return Some("an empty link".to_string());
    }
    if target.starts_with("mailto:") {
        return None;
    }
    if let Some(path) = target
        .strip_prefix(&format!("{REPO_URL}blob/main/"))
        .or_else(|| target.strip_prefix(&format!("{REPO_URL}tree/main/")))
    {
        let path = path.split('#').next().unwrap_or(path);
        return (!repo_root().join(path).exists())
            .then(|| format!("`{path}` is not in the repository"));
    }
    if target.contains("://") {
        return None;
    }
    let (file, fragment) = match target.split_once('#') {
        Some((file, fragment)) => (file, Some(fragment)),
        None => (target, None),
    };
    // `#heading` alone is a heading of this chapter.
    let resolved = if file.is_empty() {
        chapter.to_path_buf()
    } else {
        chapter
            .parent()
            .expect("a chapter has a directory")
            .join(file)
    };
    if !resolved.exists() {
        return Some(format!("{} does not exist", resolved.display()));
    }
    // A link out of the book would work in the repository and 404 in the
    // rendered book.
    let src = guide_dir().join("src");
    let inside = resolved
        .canonicalize()
        .is_ok_and(|p| p.starts_with(src.canonicalize().expect("src")));
    if !inside {
        return Some(format!(
            "{file} is outside the book: link to it as {REPO_URL}blob/main/<path> so the rendered book can reach it"
        ));
    }
    match fragment {
        Some(fragment) if !fragment.is_empty() && !has_heading(&resolved, fragment) => {
            Some(format!(
                "{} has no heading that becomes `#{fragment}`",
                resolved.display()
            ))
        }
        _ => None,
    }
}

#[test]
fn the_book_is_a_book_and_every_link_in_it_leads_somewhere() {
    let guide = guide_dir();
    let src = guide.join("src");
    assert!(
        std::fs::read_to_string(guide.join("book.toml"))
            .expect("docs/guide/book.toml")
            .contains("[book]"),
        "book.toml has no [book] table"
    );
    let summary_path = src.join("SUMMARY.md");
    let summary = std::fs::read_to_string(&summary_path).expect("docs/guide/src/SUMMARY.md");

    let mut problems = String::new();

    // Every link of every chapter, SUMMARY.md's included.
    let mut linked: Vec<PathBuf> = Vec::new();
    for rel in markdown_files(&src) {
        let path = src.join(&rel);
        let text = std::fs::read_to_string(&path).expect("a readable chapter");
        let mut in_fence = false;
        for (i, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }
            for target in link_targets(line) {
                if let Some(why) = broken_link(&path, &target) {
                    let _ = writeln!(
                        problems,
                        "docs/guide/src/{}:{}: link `{target}`: {why}",
                        rel.display(),
                        i + 1
                    );
                } else if path == summary_path {
                    let file = target.split('#').next().unwrap_or(&target);
                    if Path::new(file)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                    {
                        linked.push(src.join(file));
                    }
                }
            }
        }
    }

    // A chapter nobody can reach from the sidebar is a chapter nobody reads.
    for rel in markdown_files(&src) {
        if rel == Path::new("SUMMARY.md") {
            continue;
        }
        let full = src.join(&rel);
        if !linked.iter().any(|l| l == &full) {
            let _ = writeln!(
                problems,
                "docs/guide/src/{}: not listed in SUMMARY.md",
                rel.display()
            );
        }
    }
    assert!(
        summary.contains("](") && !linked.is_empty(),
        "SUMMARY.md lists no chapters"
    );
    assert!(problems.is_empty(), "\n{problems}");
}

#[test]
fn the_link_checker_catches_a_missing_file() {
    let chapter = guide_dir().join("src/README-probe.md");
    assert!(broken_link(&chapter, "no-such-chapter.md").is_some());
    assert!(broken_link(&chapter, "https://example.com/x").is_none());
    assert!(broken_link(&chapter, "#a-heading").is_some());
    let intro = guide_dir().join("src/introduction.md");
    assert!(broken_link(&intro, "#two-halves").is_none());
    assert!(broken_link(&intro, "getting-started.md#what-you-need").is_none());
    assert!(broken_link(&intro, "getting-started.md#no-such-heading").is_some());
    assert_eq!(slug("`byard new` & friends"), "byard-new--friends");
    assert!(
        broken_link(
            &chapter,
            "https://github.com/Briany4717/byard/blob/main/no/such/file.rs"
        )
        .is_some()
    );
    assert!(
        broken_link(
            &chapter,
            "https://github.com/Briany4717/byard/blob/main/README.md#status"
        )
        .is_none()
    );
    assert_eq!(
        link_targets("see [a](x.md#y), `[b](no.md)` and ![c](d.png \"t\")"),
        vec!["x.md#y".to_string(), "d.png".to_string()]
    );
}
