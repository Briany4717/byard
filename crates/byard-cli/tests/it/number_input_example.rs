//! Guards the committed RFC-0048 example through the real `byard` binary: the
//! `number_input` project checks clean, and the lines it draws are the ones its
//! header says they are.
//!
//! The lines are read from the accessibility dump `byard shot --a11y` writes,
//! which needs no GPU, and compared as text rather than as bounds so the test
//! says what the methods answer, not how wide the font is.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/number_input")
}

#[test]
fn number_input_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
    assert!(stdout.contains("0 errors"), "got:\n{stdout}");
}

#[test]
fn the_example_draws_what_its_header_says() {
    let out = std::env::temp_dir().join(format!("byard-number-input-{}.txt", std::process::id()));
    let run = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("shot")
        .arg(example_dir())
        .arg("--a11y")
        .arg(&out)
        .output()
        .expect("run `byard shot --a11y`");
    assert!(
        run.status.success(),
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let dump = std::fs::read_to_string(&out).expect("the tree was written");
    let _ = std::fs::remove_file(&out);

    let labels: Vec<&str> = dump
        .lines()
        .filter_map(|l| l.strip_prefix("Label \""))
        .filter_map(|l| l.split("\" @").next())
        .collect();
    assert_eq!(
        labels,
        [
            "RFC-0048",
            "Quantity",
            // 1234567 doubled, grouped by thousands.
            "doubled: 2,469,134",
            "Unit price",
            // "12,50" is not a number (the decimal mark is a point): nothing,
            // so the default 0.0 is what multiplies.
            "total: 0.00",
            // " -3 ", "1_000" (nothing, so 0), a half away from zero, abs.
            "-3 0 -3 2.5",
        ],
        "{dump}"
    );
}

/// The price field holds text that is not a number, so it draws as invalid;
/// typing a number into it clears that and shows the total.
#[test]
fn typing_a_number_clears_the_invalid_field() {
    let shot = |extra: &[&str]| {
        let out = std::env::temp_dir().join(format!(
            "byard-number-input-{}-{}.txt",
            std::process::id(),
            extra.len()
        ));
        let run = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
            .arg("shot")
            .arg(example_dir())
            .args(extra)
            .arg("--a11y")
            .arg(&out)
            .output()
            .expect("run `byard shot`");
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let dump = std::fs::read_to_string(&out).expect("the tree was written");
        let _ = std::fs::remove_file(&out);
        dump
    };
    let typed = shot(&["--tap", "200,227", "--key", "mod+a", "--type", "12.5"]);
    assert!(typed.contains("= \"12.5\""), "{typed}");
    assert!(typed.contains("Label \"total: 15,432,087.50\""), "{typed}");
}
