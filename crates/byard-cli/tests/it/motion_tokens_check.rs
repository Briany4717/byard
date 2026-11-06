//! `byard check` on motion tokens (RFC-0053): a good one passes, a misspelt or
//! wrongly used one is an error naming it, and a package's springs reach a
//! project that extends it.

use std::path::PathBuf;
use std::process::Command;

fn project(tag: &str, manifest: &str, view: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-motion-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("byard.toml"), manifest).unwrap();
    std::fs::write(dir.join("src/main.byd"), view).unwrap();
    dir
}

fn check(dir: &PathBuf) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("check")
        .arg(dir)
        .output()
        .expect("run `byard check`");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

const MANIFEST: &str = "[project]\nname = \"m\"\nentry = \"src/main.byd\"\n\
                        [theme.motion]\nbouncy = { stiffness = 300, damping_ratio = 0.5 }\nbeat = 640\n";

fn view(curve: &str) -> String {
    format!(
        "View Main() {{\n inject Theme as t\n var on = false\n \
         Box #[bg: t.primary, width: 10, height: 10, scale: on ? 2.0 : 1.0 with {curve}]\n}}\n"
    )
}

#[test]
fn a_base_token_and_a_declared_one_check_clean() {
    for (tag, curve) in [
        ("base", "t.springSpatialFast"),
        ("own", "t.springBouncy"),
        ("dur", "anim.ease(t.durationBeat)"),
    ] {
        let dir = project(tag, MANIFEST, &view(curve));
        let (ok, text) = check(&dir);
        assert!(ok, "{curve}:\n{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_misspelt_or_misused_token_is_an_error_naming_it() {
    for (tag, curve, expect) in [
        ("miss", "t.springBouncey", "UnknownThemeToken"),
        ("kind-colour", "t.primary", "is a colour"),
        ("kind-dur", "t.durationBeat", "anim.ease(t.durationBeat)"),
    ] {
        let dir = project(tag, MANIFEST, &view(curve));
        let (ok, text) = check(&dir);
        assert!(!ok && text.contains(expect), "{curve}:\n{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_bad_motion_entry_stops_the_check_and_says_how() {
    let manifest = "[project]\nname = \"m\"\nentry = \"src/main.byd\"\n\
                    [theme.motion]\nbouncy = { stiffness = 300 }\n";
    let dir = project("bad", manifest, &view("t.springBouncy"));
    let (ok, text) = check(&dir);
    assert!(
        !ok && text.contains("motion `bouncy`") && text.contains("damping_ratio"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
