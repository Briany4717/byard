//! `byard test`: run a project's scenario files, `tests/*.toml` (RFC-0058).
//!
//! A scenario draws a view headlessly, drives it with the inputs `byard shot`
//! takes, and checks the accessibility tree (RFC-0042), which needs no GPU. A
//! package written only in Byld is tested with it, on any machine.

use std::path::{Path, PathBuf};

use toml::{Table, Value};

use super::shot::{Gpu, Key, Project, Scene, SceneOptions, Step};
use crate::style;

/// What `byard test` was asked to do.
pub struct TestArgs<'a> {
    /// The project directory or `byard.toml`; the current project if absent.
    pub path: Option<&'a Path>,
    /// Run only the tests whose `file::name` contains this.
    pub filter: Option<&'a str>,
    /// Where to write each test's final picture, if anywhere.
    pub shots: Option<&'a Path>,
}

/// Where a pointer step goes: a point, or the node whose line contains a text.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    Point(f32, f32),
    Line(String),
}

/// An input.
#[derive(Clone, Debug, PartialEq)]
enum Act {
    Tap(Target),
    Hold(Target),
    Hover(Target),
    Key(Key, String),
    Type(String),
    Wait(u32),
}

/// What the tree must and must not hold.
#[derive(Clone, Debug, Default, PartialEq)]
struct Check {
    expect: Vec<String>,
    exact: Vec<String>,
    not: Vec<String>,
    not_exact: Vec<String>,
}

impl Check {
    fn is_empty(&self) -> bool {
        self.expect.is_empty()
            && self.exact.is_empty()
            && self.not.is_empty()
            && self.not_exact.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
enum TestStep {
    Act(Act),
    Check(Check),
}

#[derive(Clone, Debug, PartialEq)]
struct Test {
    /// The file's stem.
    file: String,
    name: String,
    view: Option<String>,
    size: (u32, u32),
    dark: Option<bool>,
    at: u32,
    now: Option<String>,
    steps: Vec<TestStep>,
    /// Checked after the last step and `at`.
    last: Check,
}

impl Test {
    fn id(&self) -> String {
        format!("{}::{}", self.file, self.name)
    }
}

const FILE_KEYS: &[&str] = &["view", "size", "scheme", "at", "now", "test"];
const TEST_KEYS: &[&str] = &[
    "name",
    "view",
    "size",
    "scheme",
    "at",
    "now",
    "steps",
    "expect",
    "exact",
    "not",
    "not_exact",
];
const ACT_KEYS: &[&str] = &["tap", "hold", "hover", "key", "type", "wait"];
const CHECK_KEYS: &[&str] = &["expect", "exact", "not", "not_exact"];

fn unknown_key(key: &str, allowed: &[&str]) -> String {
    let near = byard_compiler::util::closest_match(key, allowed.iter().copied());
    match near {
        Some(n) => format!("unknown key `{key}`; did you mean `{n}`?"),
        None => format!("unknown key `{key}`; the keys are {}", allowed.join(", ")),
    }
}

fn string_of(v: &Value, what: &str) -> Result<String, String> {
    v.as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("`{what}` must be a string"))
}

fn ms_of(v: &Value, what: &str) -> Result<u32, String> {
    v.as_integer()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("`{what}` must be a whole number of milliseconds"))
}

fn size_of(v: &Value) -> Result<(u32, u32), String> {
    let s = string_of(v, "size")?;
    let bad = || format!("`size` must be \"WIDTHxHEIGHT\", like \"440x880\", not `{s}`");
    let (w, h) = s.split_once('x').ok_or_else(bad)?;
    Ok((
        w.trim().parse().map_err(|_| bad())?,
        h.trim().parse().map_err(|_| bad())?,
    ))
}

fn scheme_of(v: &Value) -> Result<bool, String> {
    match string_of(v, "scheme")?.as_str() {
        "dark" => Ok(true),
        "light" => Ok(false),
        other => Err(format!(
            "`scheme` must be \"light\" or \"dark\", not `{other}`"
        )),
    }
}

fn strings_of(v: &Value, what: &str) -> Result<Vec<String>, String> {
    match v {
        Value::String(s) => Ok(vec![s.clone()]),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("`{what}` must hold strings"))
            })
            .collect(),
        _ => Err(format!("`{what}` must be a string or a list of strings")),
    }
}

fn target_of(v: &Value, what: &str) -> Result<Target, String> {
    match v {
        Value::String(s) => Ok(Target::Line(s.clone())),
        Value::Array(a) if a.len() == 2 => {
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let num = |x: &Value| {
                x.as_float()
                    .or_else(|| x.as_integer().map(|i| i as f64))
                    .map(|f| f as f32)
            };
            match (num(&a[0]), num(&a[1])) {
                (Some(x), Some(y)) => Ok(Target::Point(x, y)),
                _ => Err(format!("`{what}` takes [x, y] numbers")),
            }
        }
        _ => Err(format!(
            "`{what}` takes [x, y] or text from a line of the tree"
        )),
    }
}

fn check_of(t: &Table) -> Result<Check, String> {
    let get = |k: &str| {
        t.get(k)
            .map_or_else(|| Ok(Vec::new()), |v| strings_of(v, k))
    };
    Ok(Check {
        expect: get("expect")?,
        exact: get("exact")?,
        not: get("not")?,
        not_exact: get("not_exact")?,
    })
}

fn step_of(t: &Table) -> Result<TestStep, String> {
    for k in t.keys() {
        if !ACT_KEYS.contains(&k.as_str()) && !CHECK_KEYS.contains(&k.as_str()) {
            return Err(unknown_key(k, &[ACT_KEYS, CHECK_KEYS].concat()));
        }
    }
    let acts: Vec<&str> = ACT_KEYS
        .iter()
        .copied()
        .filter(|k| t.contains_key(*k))
        .collect();
    let has_check = CHECK_KEYS.iter().any(|k| t.contains_key(*k));
    match (acts.as_slice(), has_check) {
        ([], true) => Ok(TestStep::Check(check_of(t)?)),
        ([], false) => Err("an empty step".into()),
        ([_], true) | ([_, _, ..], _) => Err(
            "a step does one thing: one of tap, hold, hover, key, type, wait, or checks \
             (expect, exact, not, not_exact)"
                .into(),
        ),
        ([act], false) => {
            let v = &t[*act];
            Ok(TestStep::Act(match *act {
                "tap" => Act::Tap(target_of(v, act)?),
                "hold" => Act::Hold(target_of(v, act)?),
                "hover" => Act::Hover(target_of(v, act)?),
                "key" => {
                    let chord = string_of(v, act)?;
                    let (mods, name) = byard_core::KeyModifiers::parse_chord(&chord)
                        .map_err(|e| format!("`key`: {e}"))?;
                    Act::Key(Key { name, mods }, chord)
                }
                "type" => Act::Type(string_of(v, act)?),
                _ => Act::Wait(ms_of(v, act)?),
            }))
        }
    }
}

/// The tests of one scenario file. Every mistake in it, not the first.
#[allow(clippy::too_many_lines)]
fn parse_file(file: &str, text: &str) -> Result<Vec<Test>, Vec<String>> {
    let table: Table = text
        .parse()
        .map_err(|e: toml::de::Error| vec![format!("{file}.toml: {e}")])?;
    let errors = std::cell::RefCell::new(Vec::<String>::new());
    let err = |where_: &str, msg: String| {
        errors
            .borrow_mut()
            .push(format!("{file}.toml: {where_}{msg}"));
    };
    for k in table.keys() {
        if !FILE_KEYS.contains(&k.as_str()) {
            err("", unknown_key(k, FILE_KEYS));
        }
    }
    // The file's own defaults, which a test overrides key by key.
    let mut base = Table::new();
    for k in ["view", "size", "scheme", "at", "now"] {
        if let Some(v) = table.get(k) {
            base.insert(k.into(), v.clone());
        }
    }
    let tests = match table.get("test") {
        Some(Value::Array(a)) => a.as_slice(),
        Some(_) => {
            err("", "`test` must be written `[[test]]`".into());
            &[]
        }
        None => &[],
    };
    if tests.is_empty() && errors.borrow().is_empty() {
        err("", "no `[[test]]` in this file".into());
    }
    let mut out: Vec<Test> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (n, t) in tests.iter().enumerate() {
        let Some(t) = t.as_table() else {
            err("", format!("test {}: not a table", n + 1));
            continue;
        };
        let name = t.get("name").and_then(Value::as_str).map(str::to_string);
        let label = name.as_deref().map_or_else(
            || format!("test {}: ", n + 1),
            |n| format!("test \"{n}\": "),
        );
        for k in t.keys() {
            if !TEST_KEYS.contains(&k.as_str()) {
                err(&label, unknown_key(k, TEST_KEYS));
            }
        }
        let Some(name) = name else {
            err(&label, "needs a `name`".into());
            continue;
        };
        if !seen.insert(name.clone()) {
            err(&label, "another test in this file has this name".into());
            continue;
        }
        let pick = |k: &str| t.get(k).or_else(|| base.get(k));
        let mut case = Test {
            file: file.to_string(),
            name,
            view: None,
            size: (440, 880),
            dark: None,
            at: 0,
            now: None,
            steps: Vec::new(),
            last: Check::default(),
        };
        let mut ok = true;
        let mut field = |r: Result<(), String>| {
            if let Err(m) = r {
                err(&label, m);
                ok = false;
            }
        };
        field(pick("view").map_or(Ok(()), |v| {
            case.view = Some(string_of(v, "view")?);
            Ok(())
        }));
        field(pick("size").map_or(Ok(()), |v| {
            case.size = size_of(v)?;
            Ok(())
        }));
        field(pick("scheme").map_or(Ok(()), |v| {
            case.dark = Some(scheme_of(v)?);
            Ok(())
        }));
        field(pick("at").map_or(Ok(()), |v| {
            case.at = ms_of(v, "at")?;
            Ok(())
        }));
        field(pick("now").map_or(Ok(()), |v| {
            case.now = Some(string_of(v, "now")?);
            Ok(())
        }));
        field(check_of(t).map(|c| case.last = c));
        match t.get("steps") {
            None => {}
            Some(Value::Array(steps)) => {
                for (i, s) in steps.iter().enumerate() {
                    let step = s
                        .as_table()
                        .ok_or_else(|| "not a table".to_string())
                        .and_then(step_of);
                    match step {
                        Ok(s) => case.steps.push(s),
                        Err(m) => field(Err(format!("step {}: {m}", i + 1))),
                    }
                }
            }
            Some(_) => field(Err("`steps` must be a list of tables".into())),
        }
        if ok
            && case.last.is_empty()
            && !case
                .steps
                .iter()
                .any(|s| matches!(s, TestStep::Check(c) if !c.is_empty()))
        {
            err(
                &label,
                "checks nothing; a test that cannot fail is not a test (add `expect`, `exact`, \
                 `not` or `not_exact`)"
                    .into(),
            );
            ok = false;
        }
        if ok {
            out.push(case);
        }
    }
    let errors = errors.into_inner();
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// A line of the tree text without its indent.
fn lines_of(text: &str) -> Vec<&str> {
    text.lines().map(str::trim_start).collect()
}

/// A line without the ` @ x,y WxH` that ends it.
fn without_bounds(line: &str) -> &str {
    line.rsplit_once(" @ ").map_or(line, |(head, _)| head)
}

/// What of `check` the tree does not satisfy.
fn unmet(check: &Check, tree: &str) -> Vec<String> {
    let lines = lines_of(tree);
    let mut out = Vec::new();
    for s in &check.expect {
        if !lines.iter().any(|l| l.contains(s.as_str())) {
            out.push(format!("expected a line containing\n    {s}"));
        }
    }
    for s in &check.exact {
        if !lines.iter().any(|l| without_bounds(l) == s) {
            out.push(format!("expected a line equal to\n    {s}"));
        }
    }
    for s in &check.not {
        if let Some(l) = lines.iter().find(|l| l.contains(s.as_str())) {
            out.push(format!(
                "expected no line containing\n    {s}\n  but there is\n    {l}"
            ));
        }
    }
    for s in &check.not_exact {
        if let Some(l) = lines.iter().find(|l| without_bounds(l) == s) {
            out.push(format!(
                "expected no line equal to\n    {s}\n  but there is\n    {l}"
            ));
        }
    }
    out
}

fn describe(act: &Act) -> String {
    let target = |t: &Target| match t {
        Target::Point(x, y) => format!("{x},{y}"),
        Target::Line(l) => format!("{l:?}"),
    };
    match act {
        Act::Tap(t) => format!("tap {}", target(t)),
        Act::Hold(t) => format!("hold {}", target(t)),
        Act::Hover(t) => format!("hover {}", target(t)),
        Act::Key(_, chord) => format!("key {chord}"),
        Act::Type(s) => format!("type {s:?}"),
        Act::Wait(ms) => format!("wait {ms}"),
    }
}

/// Why a test failed, and with what on screen.
struct Failure {
    at: String,
    reasons: Vec<String>,
    tree: String,
}

/// The centre of the one node whose line contains `sel`.
fn resolve(scene: &Scene, sel: &str) -> Result<(f32, f32), Vec<String>> {
    let tree = scene
        .access()
        .ok_or_else(|| vec!["no accessibility tree".to_string()])?;
    let text = tree.to_text();
    let hits: Vec<usize> = lines_of(&text)
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(sel))
        .map(|(i, _)| i)
        .collect();
    match hits.as_slice() {
        [] => Err(vec![format!("no line of the tree contains\n    {sel}")]),
        [i] => {
            let b = tree.nodes[*i].bounds;
            Ok((b.x + b.width / 2.0, b.y + b.height / 2.0))
        }
        many => {
            let lines = lines_of(&text);
            let mut m = vec![format!(
                "{} lines of the tree contain `{sel}`; say which:",
                many.len()
            )];
            m.extend(many.iter().map(|&i| format!("    {}", lines[i])));
            Err(m)
        }
    }
}

fn shot_step(scene: &Scene, act: &Act) -> Result<Step, Vec<String>> {
    let point = |t: &Target| match t {
        Target::Point(x, y) => Ok((*x, *y)),
        Target::Line(l) => resolve(scene, l),
    };
    Ok(match act {
        Act::Tap(t) => {
            let (x, y) = point(t)?;
            Step::Tap(x, y)
        }
        Act::Hold(t) => {
            let (x, y) = point(t)?;
            Step::Hold(x, y)
        }
        Act::Hover(t) => {
            let (x, y) = point(t)?;
            Step::Hover(x, y)
        }
        Act::Key(k, _) => Step::Key(k.clone()),
        Act::Type(s) => Step::Type(s.clone()),
        Act::Wait(ms) => Step::Wait(*ms),
    })
}

fn tree_text(scene: &Scene) -> String {
    scene
        .access()
        .map(byard_core::access::AccessTree::to_text)
        .unwrap_or_default()
}

/// Runs one test. `Ok` carries the scene, for its picture.
fn run_test(project: &Project, test: &Test) -> (Option<Failure>, Option<Scene>) {
    let view = project
        .view(test.view.as_deref())
        .expect("validated before the run");
    let mut scene = match Scene::new(
        project,
        view,
        &SceneOptions {
            size: test.size,
            dark: test.dark,
            wall_clock: test.now.as_deref(),
            access: true,
        },
    ) {
        Ok(scene) => scene,
        Err(e) => {
            let at = "starting".to_string();
            return (
                Some(Failure {
                    at,
                    reasons: vec![e],
                    tree: String::new(),
                }),
                None,
            );
        }
    };
    // What the last input was, for saying where a check ran.
    let mut prev: Option<String> = None;
    for (i, step) in test.steps.iter().enumerate() {
        let failed = match step {
            TestStep::Act(act) => {
                let desc = describe(act);
                let failed = match shot_step(&scene, act) {
                    Ok(s) => {
                        scene.apply(&s);
                        None
                    }
                    Err(reasons) => {
                        Some((format!("step {} ({desc}) could not run", i + 1), reasons))
                    }
                };
                prev = Some(desc);
                failed
            }
            TestStep::Check(check) => {
                // One frame with no input, so a late-landing text is in.
                scene.apply(&Step::Wait(1));
                let reasons = unmet(check, &tree_text(&scene));
                let when = prev
                    .as_ref()
                    .map_or("before any input".to_string(), |d| format!("after {d}"));
                (!reasons.is_empty())
                    .then(|| (format!("step {} (a check, {when})", i + 1), reasons))
            }
        };
        if let Some((at, reasons)) = failed {
            let tree = tree_text(&scene);
            return (Some(Failure { at, reasons, tree }), Some(scene));
        }
    }
    scene.settle(test.at);
    let tree = tree_text(&scene);
    let reasons = unmet(&test.last, &tree);
    if reasons.is_empty() {
        return (None, Some(scene));
    }
    let at = prev.map_or("on the first frame".to_string(), |d| {
        format!("after the last step ({d})")
    });
    (Some(Failure { at, reasons, tree }), Some(scene))
}

/// `file-name` for a picture: the name's letters and digits, lowercased, with
/// anything else between them one `-`.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn scenario_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = root.join("tests");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|_| {
            format!(
                "no tests: {} does not exist (scenarios are `tests/*.toml` in the project)",
                dir.display()
            )
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("no tests: {} has no `.toml` files", dir.display()));
    }
    Ok(files)
}

/// Every test of the project's scenario files, or all the mistakes in them.
fn load_tests(project: &Project) -> Result<Vec<Test>, String> {
    let mut tests = Vec::new();
    let mut errors = Vec::new();
    for file in scenario_files(project.root())? {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("tests")
            .to_string();
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        match parse_file(&stem, &text) {
            Ok(t) => tests.extend(t),
            Err(e) => errors.extend(e),
        }
    }
    for t in &tests {
        if let Err(e) = project.view(t.view.as_deref()) {
            errors.push(format!("{}.toml: test \"{}\": {e}", t.file, t.name));
        }
    }
    if !errors.is_empty() {
        for e in &errors {
            style::err(e);
        }
        return Err(format!(
            "{} mistake{} in the scenario files; no test run",
            errors.len(),
            if errors.len() == 1 { "" } else { "s" }
        ));
    }
    Ok(tests)
}

fn print_failures(failures: &[(String, Failure)]) {
    if failures.is_empty() {
        return;
    }
    println!("\nfailures:");
    for (id, f) in failures {
        println!("\n---- {id} ----");
        println!("{}:", f.at);
        for r in &f.reasons {
            println!("  {r}");
        }
        if !f.tree.is_empty() {
            println!("the tree was:");
            for l in f.tree.lines() {
                println!("    {l}");
            }
        }
    }
}

pub fn run(args: &TestArgs<'_>) -> Result<(), String> {
    if args
        .path
        .is_some_and(|p| p.extension().is_some_and(|x| x == "byd"))
    {
        return Err(
            "`byard test` takes a project, not a `.byd` file: name the project directory \
             (a file path reads no manifest, so none of its packages)"
                .into(),
        );
    }
    let project = Project::load(args.path, "test")?;
    let mut tests = load_tests(&project)?;
    let total = tests.len();
    if let Some(f) = args.filter {
        tests.retain(|t| t.id().contains(f));
        if tests.is_empty() {
            return Err(format!("no test of the {total} matches `{f}`"));
        }
    }
    let filtered = total - tests.len();
    if let Some(dir) = args.shots {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }

    println!(
        "running {} test{}",
        tests.len(),
        if tests.len() == 1 { "" } else { "s" }
    );
    // The GPU is opened on the first picture, once; no adapter is one note.
    let mut gpu: Option<Result<Gpu, ()>> = None;
    let mut failures: Vec<(String, Failure)> = Vec::new();
    let mut passed = 0_usize;
    for test in &tests {
        let (outcome, scene) = run_test(&project, test);
        println!(
            "test {} ... {}",
            test.id(),
            if outcome.is_none() { "ok" } else { "FAILED" }
        );
        if let (Some(dir), Some(scene)) = (args.shots, scene) {
            let gpu = gpu.get_or_insert_with(|| match Gpu::open() {
                Ok(g) => Ok(g),
                Err(e) => {
                    style::info(&format!("{e}; the pictures are skipped"));
                    Err(())
                }
            });
            if let Ok(gpu) = gpu {
                let out = dir.join(format!("{}-{}.png", test.file, slug(&test.name)));
                write_picture(gpu, &scene, test.size, &out)?;
            }
        }
        match outcome {
            None => passed += 1,
            Some(f) => failures.push((test.id(), f)),
        }
    }
    print_failures(&failures);
    println!(
        "\ntest result: {}. {passed} passed; {} failed; {filtered} filtered out",
        if failures.is_empty() { "ok" } else { "FAILED" },
        failures.len()
    );
    if failures.is_empty() {
        Ok(())
    } else {
        Err(String::new())
    }
}

fn write_picture(gpu: &Gpu, scene: &Scene, size: (u32, u32), out: &Path) -> Result<(), String> {
    let scale = 2.0;
    let pixels = gpu.paint(scene.frame(), size, scale)?;
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let (pw, ph) = (
        (size.0 as f32 * scale).round() as u32,
        (size.1 as f32 * scale).round() as u32,
    );
    image::save_buffer(out, &pixels, pw, ph, image::ColorType::Rgba8)
        .map_err(|e| format!("{}: {e}", out.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = "Label \"Switches\" @ 24,24 392x24\n  Switch \"Wi-Fi\" = on [focusable] @ 24,64 52x30\n  Label \"1 on\" @ 24,100 40x17\n";

    fn parse(text: &str) -> Result<Vec<Test>, Vec<String>> {
        parse_file("f", text)
    }

    #[test]
    fn a_file_gives_defaults_and_a_test_overrides_them() {
        let tests = parse(
            "view = \"A\"\nsize = \"100x200\"\nscheme = \"dark\"\n\
             [[test]]\nname = \"one\"\nexpect = \"x\"\n\
             [[test]]\nname = \"two\"\nview = \"B\"\nscheme = \"light\"\nat = 300\nexpect = [\"x\"]\n",
        )
        .unwrap();
        assert_eq!(tests[0].view.as_deref(), Some("A"));
        assert_eq!(tests[0].size, (100, 200));
        assert_eq!(tests[0].dark, Some(true));
        assert_eq!(tests[0].last.expect, ["x"]);
        assert_eq!(tests[1].view.as_deref(), Some("B"));
        assert_eq!(tests[1].dark, Some(false));
        assert_eq!(tests[1].at, 300);
    }

    #[test]
    fn steps_are_the_inputs_of_shot_and_checks_in_order() {
        let t = &parse(
            "[[test]]\nname = \"t\"\nsteps = [\n { tap = [1, 2.5] },\n { hover = 'Button \"A\"' },\n \
             { key = \"mod+a\" },\n { type = \"hi\" },\n { wait = 200 },\n { hold = [3, 4] },\n \
             { expect = [\"a\"], not = \"b\" },\n]\n",
        )
        .unwrap()[0];
        assert_eq!(t.steps.len(), 7);
        assert_eq!(t.steps[0], TestStep::Act(Act::Tap(Target::Point(1.0, 2.5))));
        assert_eq!(
            t.steps[1],
            TestStep::Act(Act::Hover(Target::Line("Button \"A\"".into())))
        );
        assert_eq!(t.steps[4], TestStep::Act(Act::Wait(200)));
        let TestStep::Check(c) = &t.steps[6] else {
            panic!("a check")
        };
        assert_eq!(
            (c.expect.as_slice(), c.not.as_slice()),
            (&["a".to_string()][..], &["b".to_string()][..])
        );
    }

    #[test]
    fn every_mistake_is_reported_with_its_file_test_and_step() {
        let errs = parse(
            "[[test]]\nname = \"t\"\nexpect = \"x\"\nsteps = [{ tap = [1] }, { key = \"Flibber\" }, \
             { tap = [1, 2], wait = 3 }, { tapp = [1, 2] }, {}]\nsize = \"big\"\n\
             [[test]]\nname = \"t\"\nexpect = \"x\"\n\
             [[test]]\nname = \"empty\"\n\
             [[test]]\nexpect = \"x\"\nscheme = \"blue\"\n",
        )
        .unwrap_err();
        let all = errs.join("\n");
        assert!(
            all.contains("f.toml: test \"t\": step 1: `tap` takes [x, y] or text"),
            "{all}"
        );
        assert!(all.contains("step 2: `key`"), "{all}");
        assert!(all.contains("step 3: a step does one thing"), "{all}");
        assert!(
            all.contains("step 4: unknown key `tapp`; did you mean `tap`?"),
            "{all}"
        );
        assert!(all.contains("step 5: an empty step"), "{all}");
        assert!(all.contains("`size` must be \"WIDTHxHEIGHT\""), "{all}");
        assert!(
            all.contains("another test in this file has this name"),
            "{all}"
        );
        assert!(all.contains("test \"empty\": checks nothing"), "{all}");
        assert!(all.contains("test 4: needs a `name`"), "{all}");
    }

    #[test]
    fn an_unknown_key_suggests_the_near_one() {
        let errs = parse("[[test]]\nname = \"t\"\nexpet = \"x\"\n").unwrap_err();
        assert!(
            errs[0].contains("unknown key `expet`; did you mean `expect`?"),
            "{errs:?}"
        );
        let errs = parse("veiw = \"A\"\n").unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("did you mean `view`?")),
            "{errs:?}"
        );
    }

    #[test]
    fn a_file_with_no_tests_or_bad_toml_is_an_error() {
        assert!(parse("view = \"A\"\n").unwrap_err()[0].contains("no `[[test]]`"));
        assert!(parse("[[test\n").unwrap_err()[0].starts_with("f.toml:"));
    }

    #[test]
    fn expect_is_a_substring_and_exact_ignores_indent_and_bounds() {
        let check = |c: Check| unmet(&c, TREE);
        let c = Check {
            expect: vec!["Wi-Fi\" = on".into(), "@ 24,64".into()],
            exact: vec!["Switch \"Wi-Fi\" = on [focusable]".into()],
            not: vec!["= off".into()],
            not_exact: vec!["Label \"0 on\"".into()],
        };
        assert_eq!(check(c), Vec::<String>::new());
        // A substring is not an exact line.
        let r = check(Check {
            exact: vec!["Switch \"Wi-Fi\"".into()],
            ..Check::default()
        });
        assert_eq!(r.len(), 1);
        let r = check(Check {
            expect: vec!["= off".into()],
            not: vec!["Wi-Fi".into()],
            not_exact: vec!["Label \"1 on\"".into()],
            ..Check::default()
        });
        assert_eq!(r.len(), 3, "{r:?}");
        assert!(r[1].contains("Switch \"Wi-Fi\" = on"), "{r:?}");
    }

    #[test]
    fn a_picture_is_named_from_letters_and_digits() {
        assert_eq!(
            slug("A tap turns the switch on!"),
            "a-tap-turns-the-switch-on"
        );
        assert_eq!(slug("  --x__y "), "x-y");
    }
}
