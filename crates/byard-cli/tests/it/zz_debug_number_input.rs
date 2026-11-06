//! Temporary: what CI sees when the price field is edited. Not for merging.
use std::process::Command;

fn shot(steps: &[&str]) -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/number_input");
    let out = std::env::temp_dir().join(format!(
        "zz-dbg-{}-{}.txt",
        std::process::id(),
        steps.join("_").replace(['/', ' ', ','], "-")
    ));
    let run = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("shot")
        .arg(&dir)
        .args(steps)
        .arg("--a11y")
        .arg(&out)
        .output()
        .unwrap();
    let tree = std::fs::read_to_string(&out).unwrap_or_default();
    format!(
        "STEPS {steps:?}\nstatus {:?}\nstderr {}\n{}",
        run.status,
        String::from_utf8_lossy(&run.stderr),
        tree
    )
}

#[test]
fn zz_debug_number_input() {
    let mut all = String::new();
    for steps in [
        vec![],
        vec!["--tap", "200,227"],
        vec!["--tap", "200,227", "--type", "9"],
        vec!["--tap", "200,227", "--key", "ctrl+a"],
        vec!["--tap", "200,227", "--key", "ctrl+a", "--type", "9"],
        vec!["--tap", "200,227", "--key", "mod+a", "--type", "9"],
        vec![
            "--tap",
            "200,227",
            "--key",
            "Home",
            "--key",
            "shift+End",
            "--type",
            "9",
        ],
        vec!["--tap", "200,227", "--key", "Backspace", "--type", "9"],
        vec!["--tap", "200,100", "--key", "ctrl+a", "--type", "9"],
    ] {
        all.push_str(&shot(&steps));
        all.push_str("\n----\n");
    }
    panic!("DEBUG OUTPUT\n{all}");
}
