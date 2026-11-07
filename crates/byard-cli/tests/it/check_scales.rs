//! `byard check` of an app that calls one view many times (RFC-0046).
//!
//! The Material gallery calls `m.Button` about 190 times and its check took
//! 120 ms where the package alone took 27: checking lowered and laid out the
//! button's body once per call. A call is now checked against the view's
//! signature, and the body once for each shape of call, so the cost of a check
//! is no longer the number of calls times the size of what they call.
//!
//! The ceilings are in `ci/budgets.toml` under `[check]`. That every call is
//! checked and nothing is lost is held in every run; the times only by the
//! ignored test, which CI's budgets step runs with `--ignored` (a time flakes
//! while the suite loads every core) and which prints what it measured next
//! to the ceilings.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// How many measurements are made; the ceiling is on the best.
const RUNS: usize = 3;

/// A button's worth of body: state, a style, a `when`, a nested view, an
/// animated attribute, so that a call that expands it costs what a real one does.
const BUTTON: &str = r#"
View Face(label: Str, selected: Bool = false, on_tap: Fn() = {}) {
    inject Theme as t
    var hov = false
    var down = false
    let r = down ? 8 : 20
    let chrome = style {
        bg: selected ? t.primary : t.surface with t.springEffectsFast
        radius: r with t.springSpatialFast
        height: 40
    }
    Box #[..chrome, p: (8, 16), tap => on_tap()] {
        Row #[gap: 8, align: center] {
            when selected { Text("*") #[color: t.onPrimary] }
            Text(label) #[typo: t.labelLarge, color: selected ? t.onPrimary : t.onSurface]
        }
    }
}

View Pill(label: Str, selected: Bool = false, wide: Bool = false, on_tap: Fn() = {}) {
    when wide {
        Box #[grow: 1] { Face(label, selected: selected, on_tap: on_tap) }
    } else {
        Face(label, selected: selected, on_tap: on_tap)
    }
}
"#;

/// A project of `calls` calls of `Pill`, each with its own label.
fn project(dir: &Path, calls: usize) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("byard.toml"),
        "[project]\nname = \"scales\"\nentry = \"src/main.byd\"\n",
    )
    .unwrap();
    let mut src = String::from(BUTTON);
    src.push_str("\nView Main() {\n    inject Theme as t\n    Column #[bg: t.surface, gap: 4] {\n");
    for i in 0..calls {
        writeln!(src, "        Pill(\"button {i}\", selected: true)").unwrap();
    }
    src.push_str("    }\n}\n");
    std::fs::write(dir.join("src/main.byd"), src).unwrap();
}

fn dir(tag: &str, calls: usize) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("byard-scales-{}-{tag}-{calls}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    project(&dir, calls);
    dir
}

/// One `byard check`, with how long the process took.
fn check(dir: &Path) -> (bool, String, Duration) {
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("check")
        .current_dir(dir)
        .output()
        .expect("run byard");
    let took = started.elapsed();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), said, took)
}

fn best_ms(dir: &Path) -> u64 {
    (0..RUNS)
        .map(|_| {
            let (ok, said, took) = check(dir);
            assert!(ok, "{said}");
            u64::try_from(took.as_millis()).unwrap_or(u64::MAX)
        })
        .min()
        .unwrap()
}

fn ceiling(key: &str) -> u64 {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci/budgets.toml");
    let text = std::fs::read_to_string(&path).expect("read ci/budgets.toml");
    let budgets: toml::Table = text.parse().expect("ci/budgets.toml is TOML");
    budgets
        .get("check")
        .and_then(|s| s.get(key))
        .and_then(toml::Value::as_integer)
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or_else(|| panic!("ci/budgets.toml has no non-negative integer [check] {key}"))
}

/// A thousand calls are all checked: a mistake in the thousandth is reported,
/// and so is each call's own mistake, at its own place.
#[test]
fn every_call_is_checked_however_many_there_are() {
    let d = dir("all", 1000);
    let (ok, said, _) = check(&d);
    assert!(ok, "{said}");
    assert!(said.contains("0 errors"), "{said}");

    let main = d.join("src/main.byd");
    let src = std::fs::read_to_string(&main).unwrap();
    let broken = src.replace(
        "Pill(\"button 999\", selected: true)",
        "Pill(\"button 999\", selectd: true)",
    );
    assert_ne!(src, broken);
    std::fs::write(&main, broken).unwrap();
    let (ok, said, _) = check(&d);
    assert!(!ok, "{said}");
    assert!(said.contains("selectd"), "{said}");
    let _ = std::fs::remove_dir_all(&d);
}

/// What a check costs with 10 and with 1000 calls, against the ceilings.
#[test]
#[ignore = "a time: run alone, as CI's budgets step does"]
fn a_thousand_calls_cost_little_more_than_ten() {
    let (few, many) = (dir("time", 10), dir("time", 1000));
    let (t10, t1000) = (best_ms(&few), best_ms(&many));
    let (limit, ratio_limit) = (
        ceiling("calls_1000_ms"),
        ceiling("calls_1000_over_10_percent"),
    );
    let ratio = t1000 * 100 / t10.max(1);
    println!(
        "check: 10 calls {t10} ms, 1000 calls {t1000} ms best of {RUNS} ({ratio}% of the 10), \
         ceilings {limit} ms and {ratio_limit}%"
    );
    let _ = std::fs::remove_dir_all(&few);
    let _ = std::fs::remove_dir_all(&many);
    assert!(
        t1000 <= limit,
        "1000 calls took {t1000} ms; the ceiling is {limit} ms (ci/budgets.toml, [check] calls_1000_ms)"
    );
    assert!(
        ratio <= ratio_limit,
        "1000 calls cost {ratio}% of 10; the ceiling is {ratio_limit}% (ci/budgets.toml, [check])"
    );
}
