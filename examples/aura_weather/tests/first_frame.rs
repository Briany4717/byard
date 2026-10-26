//! The first-frame budget (RFC-0046 section 2): how long a headless start of
//! Aura Weather takes to its first frame.
//!
//! The ceiling is `[startup] first_frame_ms` in `ci/budgets.toml`, and the
//! test prints what it measured next to it every run (`--nocapture` shows it).
//! Like the frame budget in `byard-platform`, a ceiling may be lowered by any
//! change that improves things and raised only by one that says in its
//! description the old value, the new one and why. The other budget measured
//! with this app, the idle wakes, is in `tests/it/budgets.rs`.
//!
//! # Why this is its own test binary
//!
//! A time measured while the other tests of `aura_weather` run beside it
//! (they run in parallel, on every core) says how busy the machine was. Cargo
//! runs one test binary at a time, so this one has the machine to itself. It
//! also registers the app's native view the way the app does, which the
//! process-wide registry allows once.

#[path = "it/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::time::Instant;

use common::Server;

const FORECAST: &str = include_str!("fixtures/forecast.json");

// ── First frame ────────────────────────────────────────────────────────────

/// How many headless starts are timed, each in a process of its own. The
/// ceiling is on the fastest: the slower ones measure what else the machine
/// was doing, and the best of three is the number that a real regression
/// moves and a busy moment does not.
const STARTS: usize = 3;

/// Copies a directory tree.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create a directory");
    for entry in std::fs::read_dir(from).expect("read a directory") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy a file");
        }
    }
}

/// A copy of the project in this directory whose `[http]` origins are all
/// `origin`, so that a start asks the fixture server and never the network.
/// Deleted when dropped.
struct LocalProject(PathBuf);

impl LocalProject {
    fn new(origin: &str) -> Self {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = std::env::temp_dir().join(format!("aura-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for tree in ["src", "fonts", "assets"] {
            copy_tree(&source.join(tree), &dir.join(tree));
        }

        let manifest = std::fs::read_to_string(source.join("byard.toml")).expect("the manifest");
        let mut manifest: toml::Table = manifest.parse().expect("the manifest is TOML");
        let http = manifest
            .get_mut("http")
            .and_then(toml::Value::as_table_mut)
            .expect("the manifest has an [http] table");
        http.insert("base_url".into(), origin.into());
        if let Some(hosts) = http.get_mut("hosts").and_then(toml::Value::as_table_mut) {
            for (_, host) in hosts.iter_mut() {
                *host = origin.into();
            }
        }
        let manifest = toml::to_string(&manifest).expect("write the manifest");
        assert!(
            !manifest.contains("open-meteo"),
            "a real origin is left in the copied manifest:\n{manifest}"
        );
        std::fs::write(dir.join("byard.toml"), manifest).expect("write the manifest");
        Self(dir)
    }
}

impl Drop for LocalProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Where a CI runner is told that a check did not run.
fn announce_skip(why: &str) {
    eprintln!("first frame: skipped, {why}");
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        println!("::warning::the first-frame budget did not run: {why}");
    }
}

/// What the child process of [`a_headless_start_reaches_its_first_frame_within_budget`]
/// is told: where the local copy of the project is, and where the tiles are.
const CHILD_PROJECT: &str = "AURA_FIRST_FRAME_PROJECT";
const CHILD_TILES: &str = "AURA_FIRST_FRAME_TILES";

/// The two lines a child reports on its standard output.
const REPORT_MS: &str = "first-frame-ms=";
const REPORT_SKIP: &str = "first-frame-skip=";

/// One start, in this process: from the app being built to its first frame
/// drawn and saved. The project is read, the program compiled, the GPU found,
/// the first frame laid out and drawn and its picture written; no time is
/// given to running, so one frame is all there is.
fn start_once(project: &Path, tiles: &str) {
    let picture = project.join("first-frame.png");
    let started = Instant::now();
    let shot = aura_weather::app_at(project, tiles).shot(&picture, &byard::Shot::after(0));
    let took = started.elapsed();
    match shot {
        Ok(()) => println!("{REPORT_MS}{}", took.as_millis()),
        Err(byard::ByardError::Platform(e)) if e.contains("no GPU adapter") => {
            println!("{REPORT_SKIP}this machine has no GPU adapter to draw with");
        }
        Err(e) => panic!("the headless start failed: {e}"),
    }
}

/// Starts the app once in a process of its own, as this test binary, and
/// returns what it reported.
///
/// A start is a process start: the process-wide registries are empty, the
/// fonts and the shaders have not been touched. Repeating it in one process
/// would time a warm app, and registering the app's native view twice is
/// refused besides.
fn start_in_a_new_process(project: &Path, tiles: &str) -> Result<u64, String> {
    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "a_headless_start_reaches_its_first_frame_within_budget",
            "--nocapture",
        ])
        .env(CHILD_PROJECT, project)
        .env(CHILD_TILES, tiles)
        .output()
        .expect("start the test binary again");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Some(ms) = line.strip_prefix(REPORT_MS) {
            return ms.trim().parse().map_err(|e| format!("{line}: {e}"));
        }
        if let Some(why) = line.strip_prefix(REPORT_SKIP) {
            return Err(why.trim().to_string());
        }
    }
    panic!(
        "a headless start reported nothing ({}):\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_headless_start_reaches_its_first_frame_within_budget() {
    // The same test, run again by itself as the process one start is timed in.
    if let Some(project) = std::env::var_os(CHILD_PROJECT) {
        let tiles = std::env::var(CHILD_TILES).expect("the tile server");
        start_once(Path::new(&project), &tiles);
        return;
    }

    let ceiling_ms = common::ceiling("startup", "first_frame_ms");
    let server = Server::serve("200 OK", FORECAST);
    let project = LocalProject::new(&server.base_url());
    let tiles = format!("{}/tiles", server.base_url());

    let mut starts = Vec::new();
    for _ in 0..STARTS {
        match start_in_a_new_process(&project.0, &tiles) {
            Ok(ms) => starts.push(ms),
            Err(why) => {
                announce_skip(&why);
                return;
            }
        }
    }
    let best_ms = starts.iter().copied().min().expect("at least one start");
    let all = starts
        .iter()
        .map(|ms| format!("{ms} ms"))
        .collect::<Vec<_>>()
        .join(", ");
    // Timing is only enforced where the ceiling was measured. Another system
    // has its own graphics stack and its own runner, and a ceiling nobody has
    // measured there is a guess that would flake.
    let enforced = cfg!(target_os = "linux");
    eprintln!(
        "first frame: {best_ms} ms best of {STARTS} ({all}), ceiling {ceiling_ms} ms{}",
        if enforced {
            ""
        } else {
            " (not enforced on this system)"
        }
    );

    assert!(
        !enforced || best_ms <= ceiling_ms,
        "Aura Weather took {best_ms} ms to its first frame in a headless start, over the \
         ceiling of {ceiling_ms} ms in ci/budgets.toml [startup] (every start: {all}). The \
         ceiling is six times what it measured where it was set and about three times what \
         a CI runner takes, so this is a slowdown, not noise: something on the way to the first frame got much more expensive (reading the project, compiling \
         the views, finding the GPU, building pipelines, the first layout). Find it with \
         `--trace`. Raise the ceiling only if the cost is worth it, and say so in the PR."
    );
}
