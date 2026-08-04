//! Guards the committed RFC-0031 §S7–§S8 example through the real `byard`
//! binary: the `organic_fusion` project, a smoothing-radius ramp, an animated
//! member approaching a fixed one, a fused outline drawn from the first shape's
//! stroke, and a fused pair of `ngon`s under a paint-time transform, must
//! check clean (parse, type-check, lower, validate with no diagnostics).

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/organic_fusion")
}

/// `byard check <project-dir>` on the organic-fusion example reports no errors.
#[test]
fn organic_fusion_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("0 errors"),
        "expected a clean check, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("warning"),
        "the example must not lean on a diagnostic it also documents:\n{stdout}"
    );
}

/// The other half, and the reason the warning severity exists at all
/// (RFC-0031 §Q5): a per-member stroke inside a fused group is reported and the
/// check still **passes**. An inert attribute is worth a word, not a failed
/// build; a dash on a fused stroke, which cannot be drawn at all, is worth a
/// failure.
///
/// Both cases are committed fixtures rather than generated sources, so the
/// thing being checked is a file a reader can open.
#[test]
fn an_inert_stroke_warns_while_an_undrawable_dash_fails() {
    let fixture = |name: &str| {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let out = Command::new(env!("CARGO_BIN_EXE_byard"))
            .arg("check")
            .arg(dir)
            .output()
            .expect("run `byard check`");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let (ok, stdout, stderr) = fixture("fusion_inert_stroke");
    assert!(ok, "an inert property must not fail the build:\n{stdout}");
    assert!(
        stdout.contains("0 errors, 1 warning"),
        "the warning must be counted and not hidden:\n{stdout}"
    );
    assert!(
        stderr.contains("warning[StrokeInFusionGroup]"),
        "…and reported as a warning, not an error:\n{stderr}"
    );

    let (ok, stdout, stderr) = fixture("fusion_dashed_stroke");
    assert!(!ok, "a dash that cannot be drawn must fail:\n{stdout}");
    assert!(
        stderr.contains("error[DashOnFusedStroke]"),
        "…and say so:\n{stderr}"
    );
}
