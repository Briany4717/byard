//! RFC-0058: `byard shot --view` and `byard test`, through the real binary,
//! on the `package_testing` example (a gallery app that depends on a package
//! by path) and on small projects written here for the failures.
//!
//! None of it needs a GPU, except the one picture test, which skips without.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn gallery() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/package_testing/gallery")
}

fn byard(args: &[&str], dir: Option<&Path>) -> (Output, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_byard-cli"));
    cmd.args(args);
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    let out = cmd.output().expect("run byard");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, text)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-pkgtest-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        dir.join("byard.toml"),
        "[project]\nname = \"t\"\nentry = \"main.byd\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.byd"),
        "View Main() {\n    var n = 0\n    Column {\n        Button(\"Add\") => n = n + 1\n        \
         Text(\"n = {n}\")\n    }\n}\nView Other() {\n    Text(\"other\")\n}\n",
    )
    .unwrap();
    dir
}

fn tree_of_view(view: &str) -> String {
    let dir = scratch(&format!("tree-{view}"));
    let tree = dir.join("tree.txt");
    let (out, text) = byard(
        &[
            "shot",
            gallery().to_str().unwrap(),
            "--view",
            view,
            "--a11y",
            tree.to_str().unwrap(),
        ],
        None,
    );
    assert!(out.status.success(), "{text}");
    std::fs::read_to_string(tree).unwrap()
}

#[test]
fn shot_view_draws_the_named_page_of_an_app_with_packages() {
    let counters = tree_of_view("Counters");
    assert!(counters.contains("Button \"Clicks\""), "{counters}");
    assert!(!counters.contains("Switch"), "{counters}");
    let settings = tree_of_view("Settings");
    assert!(settings.contains("Switch \"Wi-Fi\""), "{settings}");
    assert!(!settings.contains("Button"), "{settings}");
}

#[test]
fn shot_view_names_the_close_views_when_there_is_no_such_one() {
    let (out, text) = byard(
        &[
            "shot",
            gallery().to_str().unwrap(),
            "--view",
            "Countrs",
            "--a11y",
            "tree.txt",
        ],
        Some(&scratch("unknown")),
    );
    assert!(!out.status.success());
    assert!(text.contains("no view `Countrs`"), "{text}");
    assert!(text.contains("closest: Counters"), "{text}");
}

#[test]
fn shot_view_refuses_a_view_with_required_parameters() {
    let (out, text) = byard(
        &[
            "shot",
            gallery().to_str().unwrap(),
            "--view",
            "Section",
            "--a11y",
            "tree.txt",
        ],
        Some(&scratch("required")),
    );
    assert!(!out.status.success());
    assert!(
        text.contains("view `Section` has required parameters (title)"),
        "{text}"
    );
}

#[test]
fn the_gallery_scenarios_pass_without_a_gpu() {
    let (out, text) = byard(&["test", gallery().to_str().unwrap()], None);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("running 7 tests"), "{text}");
    assert!(
        text.contains("test counters::a tap counts, and only its own counter ... ok"),
        "{text}"
    );
    assert!(
        text.contains("test result: ok. 7 passed; 0 failed; 0 filtered out"),
        "{text}"
    );
}

#[test]
fn a_filter_runs_the_tests_it_matches_and_a_filter_matching_none_fails() {
    let (out, text) = byard(
        &[
            "test",
            gallery().to_str().unwrap(),
            "--filter",
            "settings::",
        ],
        None,
    );
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("test result: ok. 3 passed; 0 failed; 4 filtered out"),
        "{text}"
    );
    let (out, text) = byard(
        &["test", gallery().to_str().unwrap(), "--filter", "nope"],
        None,
    );
    assert!(!out.status.success());
    assert!(text.contains("no test of the 7 matches `nope`"), "{text}");
}

#[test]
fn a_failing_test_says_which_step_what_and_the_tree_and_exits_non_zero() {
    let dir = scratch("fail");
    std::fs::write(
        dir.join("tests/a.toml"),
        "[[test]]\nname = \"good\"\nsteps = [{ tap = 'Button \"Add\"' }]\nexpect = 'n = 1'\n\
         [[test]]\nname = \"bad\"\nsteps = [\n  { tap = 'Button \"Add\"' },\n  \
         { expect = 'n = 1' },\n  { tap = 'Button \"Add\"' },\n]\nexpect = ['n = 1']\n\
         not = ['n = 2']\n",
    )
    .unwrap();
    let (out, text) = byard(&["test"], Some(&dir));
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("test a::good ... ok"), "{text}");
    assert!(text.contains("test a::bad ... FAILED"), "{text}");
    assert!(text.contains("---- a::bad ----"), "{text}");
    assert!(
        text.contains("after the last step (tap \"Button \\\"Add\\\"\")"),
        "{text}"
    );
    assert!(
        text.contains("expected a line containing\n    n = 1"),
        "{text}"
    );
    assert!(
        text.contains("expected no line containing\n    n = 2"),
        "{text}"
    );
    assert!(text.contains("the tree was:\n    Button \"Add\""), "{text}");
    assert!(text.contains("Label \"n = 2\""), "{text}");
    assert!(
        text.contains("test result: FAILED. 1 passed; 1 failed; 0 filtered out"),
        "{text}"
    );
}

#[test]
fn a_check_between_steps_names_its_step() {
    let dir = scratch("mid");
    std::fs::write(
        dir.join("tests/a.toml"),
        "[[test]]\nname = \"mid\"\nsteps = [\n  { tap = 'Button \"Add\"' },\n  \
         { expect = 'n = 5' },\n  { tap = 'Button \"Add\"' },\n]\nexpect = 'n = 2'\n",
    )
    .unwrap();
    let (out, text) = byard(&["test"], Some(&dir));
    assert!(!out.status.success());
    assert!(text.contains("step 2 (a check, after tap"), "{text}");
}

#[test]
fn a_selector_that_matches_nothing_or_many_nodes_is_a_failure_that_says_so() {
    let dir = scratch("select");
    std::fs::write(
        dir.join("tests/a.toml"),
        "[[test]]\nname = \"none\"\nsteps = [{ tap = 'Button \"Nope\"' }]\nexpect = 'x'\n\
         [[test]]\nname = \"many\"\nsteps = [{ tap = 'Label' }]\nexpect = 'x'\n",
    )
    .unwrap();
    // Two labels would match `Label`; one Button has to be added to the view.
    std::fs::write(
        dir.join("main.byd"),
        "View Main() {\n    Column {\n        Text(\"one\")\n        Text(\"two\")\n    }\n}\n",
    )
    .unwrap();
    let (out, text) = byard(&["test"], Some(&dir));
    assert!(!out.status.success());
    assert!(
        text.contains("step 1 (tap \"Button \\\"Nope\\\"\") could not run"),
        "{text}"
    );
    assert!(text.contains("no line of the tree contains"), "{text}");
    assert!(
        text.contains("2 lines of the tree contain `Label`; say which:"),
        "{text}"
    );
}

#[test]
fn mistakes_in_the_scenario_files_stop_the_run_before_any_test() {
    let dir = scratch("mistakes");
    std::fs::write(
        dir.join("tests/a.toml"),
        "[[test]]\nname = \"typo\"\nexpet = 'x'\nview = \"Mian\"\n\
         [[test]]\nname = \"nothing\"\n\
         [[test]]\nname = \"ok\"\nview = \"Other\"\nexpect = 'other'\n",
    )
    .unwrap();
    let (out, text) = byard(&["test"], Some(&dir));
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("unknown key `expet`; did you mean `expect`?"),
        "{text}"
    );
    assert!(text.contains("test \"nothing\": checks nothing"), "{text}");
    assert!(!text.contains("running"), "{text}");
    assert!(text.contains("no test run"), "{text}");
    // The valid view of a valid test is not a mistake.
    assert!(!text.contains("test \"ok\""), "{text}");
}

#[test]
fn an_unknown_view_or_one_with_parameters_is_a_mistake_in_the_test() {
    let dir = scratch("views");
    std::fs::write(
        dir.join("tests/a.toml"),
        "view = \"Mian\"\n[[test]]\nname = \"t\"\nexpect = 'x'\n",
    )
    .unwrap();
    let (out, text) = byard(&["test"], Some(&dir));
    assert!(!out.status.success());
    assert!(
        text.contains("a.toml: test \"t\": no view `Mian` in this project; closest: Main"),
        "{text}"
    );
}

#[test]
fn no_tests_directory_and_a_byd_path_are_errors_not_empty_runs() {
    let dir = scratch("none");
    std::fs::remove_dir_all(dir.join("tests")).unwrap();
    let (out, text) = byard(&["test"], Some(&dir));
    assert!(!out.status.success());
    assert!(text.contains("no tests:"), "{text}");
    let main = dir.join("main.byd");
    let (out, text) = byard(&["test", main.to_str().unwrap()], None);
    assert!(!out.status.success());
    assert!(
        text.contains("takes a project, not a `.byd` file"),
        "{text}"
    );
}

#[test]
fn shots_are_written_with_a_gpu_and_skipped_with_a_note_without() {
    let dir = scratch("shots");
    let pictures = dir.join("pictures");
    let (out, text) = byard(
        &[
            "test",
            gallery().to_str().unwrap(),
            "--filter",
            "settings::a tap",
            "--shots",
            pictures.to_str().unwrap(),
        ],
        None,
    );
    assert!(out.status.success(), "{text}");
    if text.contains("no GPU adapter") {
        assert!(text.contains("the pictures are skipped"), "{text}");
        return;
    }
    let png = pictures.join("settings-a-tap-turns-a-switch-on.png");
    let img = image::open(&png).expect("a PNG").to_rgb8();
    assert_eq!(img.dimensions(), (880, 1200));
}
