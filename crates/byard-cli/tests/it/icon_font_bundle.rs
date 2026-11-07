//! `byard bundle` cuts an icon font down to the icons the program uses
//! (RFC-0054), and leaves every other font alone.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

const ICONS: &[u8] = include_bytes!("../../examples/assets/fonts/ByardIcons.ttf");
const ALL: [&str; 8] = [
    "favorite", "home", "star", "search", "settings", "mail", "menu", "close",
];

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A project whose `byard.toml` declares the symbols font as `font_line`.
fn project(name: &str, font_line: &str, main: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("byard-icons-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let app = root.join("app");
    write(
        &app.join("byard.toml"),
        format!(
            "[project]\nname = \"app\"\nentry = \"src/main.byd\"\n\n[assets.fonts]\n{font_line}\n"
        ),
    );
    write(&app.join("src/main.byd"), main);
    write(&app.join("fonts/Icons.ttf"), ICONS);
    write(&root.join("app-bin"), "#!/bin/sh\n");
    root
}

fn run(root: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .args(args)
        .output()
        .expect("run byard-cli");
    let _ = root;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// Bundles the project and returns the report and the bundled font's bytes.
fn bundle(root: &Path) -> (String, Vec<u8>) {
    let (ok, text) = run(
        root,
        &[
            "bundle",
            root.join("app").to_str().unwrap(),
            "--bin",
            root.join("app-bin").to_str().unwrap(),
            "--out",
            root.join("out").to_str().unwrap(),
            "--layout",
            "dir",
        ],
    );
    assert!(ok, "bundle failed:\n{text}");
    let font = std::fs::read(root.join("out/app/project/fonts/Icons.ttf")).unwrap();
    (text, font)
}

fn drawn(font: &[u8]) -> Vec<String> {
    let names: Vec<String> = ALL.iter().map(|s| (*s).to_string()).collect();
    byard_core::text::ligatures(&Arc::from(font), &names)
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

const STATIC: &str = r#"View Main() {
    Row {
        Text("favorite") #[font: symbols]
        Text("home") #[font: symbols, variations: (FILL: 1)]
    }
}
"#;

#[test]
fn an_icon_font_keeps_only_the_names_the_program_writes() {
    let root = project(
        "static",
        r#"symbols = { path = "fonts/Icons.ttf", icons = true }"#,
        STATIC,
    );
    let (report, font) = bundle(&root);
    assert!(
        font.len() < ICONS.len(),
        "{} vs {}",
        font.len(),
        ICONS.len()
    );
    assert_eq!(drawn(&font), ["favorite", "home"], "{report}");
    assert!(report.contains("Icon font"), "{report}");
    assert!(report.contains("2 icons kept"), "{report}");
    // The axes came with the glyphs.
    for table in [b"fvar", b"gvar"] {
        assert!(font.windows(4).any(|w| w == table));
    }
}

#[test]
fn a_name_in_a_view_argument_is_still_a_literal() {
    let main = r#"View Glyph(name: Str) { Text(name) #[font: symbols] }
View Main() { Column { Glyph("star") Glyph("close") } }
"#;
    let root = project(
        "arg",
        r#"symbols = { path = "fonts/Icons.ttf", icons = true }"#,
        main,
    );
    let (_, font) = bundle(&root);
    assert_eq!(drawn(&font), ["star", "close"]);
}

#[test]
fn names_built_at_run_time_ship_the_whole_font_and_check_says_where() {
    let main = r#"View Main() {
    var n = "home"
    Text(n + "_x") #[font: symbols]
}
"#;
    let root = project(
        "dynamic",
        r#"symbols = { path = "fonts/Icons.ttf", icons = true }"#,
        main,
    );
    let (report, font) = bundle(&root);
    assert_eq!(font, ICONS, "the font is shipped whole");
    assert!(report.contains("cannot be read at build time"), "{report}");
    let (ok, text) = run(&root, &["check", root.join("app").to_str().unwrap()]);
    assert!(ok, "a warning does not fail the check:\n{text}");
    assert!(text.contains("IconNameNotStatic"), "{text}");
    assert!(
        text.contains("main.byd:3"),
        "the warning names the line:\n{text}"
    );
}

#[test]
fn icons_include_names_what_the_program_builds() {
    let main = r#"View Main() {
    var n = "home"
    Text(n + "_x") #[font: symbols]
}
"#;
    let root = project(
        "include",
        r#"symbols = { path = "fonts/Icons.ttf", icons = true, icons_include = ["mail", "menu"] }"#,
        main,
    );
    let (report, font) = bundle(&root);
    // `home` is a literal in the program, the other two are listed.
    assert_eq!(drawn(&font), ["home", "mail", "menu"], "{report}");
    let (_, text) = run(&root, &["check", root.join("app").to_str().unwrap()]);
    assert!(!text.contains("IconNameNotStatic"), "{text}");
}

#[test]
fn a_font_not_declared_as_an_icon_font_is_never_cut() {
    let root = project("plain", r#"symbols = "fonts/Icons.ttf""#, STATIC);
    let (report, font) = bundle(&root);
    assert_eq!(font, ICONS, "{report}");
    assert!(!report.contains("Icon font"), "{report}");
}

#[test]
fn a_package_s_icon_font_is_cut_for_the_names_the_app_passes_it() {
    let root = project("package", r#"other = "fonts/Icons.ttf""#, "");
    write(
        &root.join("app/byard.toml"),
        "[project]\nname = \"app\"\nentry = \"src/main.byd\"\n\n\
         [dependencies]\nkit = { path = \"../kit\" }\n",
    );
    write(
        &root.join("app/src/main.byd"),
        "use kit as k\nView Main() { k.Glyph(\"search\") }\n",
    );
    write(
        &root.join("kit/byard.toml"),
        "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n\n[assets.fonts]\n\
         symbols = { path = \"fonts/Icons.ttf\", icons = true }\n",
    );
    write(&root.join("kit/fonts/Icons.ttf"), ICONS);
    write(
        &root.join("kit/src/kit.byd"),
        "View Glyph(name: Str) { Text(name) #[font: symbols] }\n",
    );
    let (ok, text) = run(
        &root,
        &[
            "bundle",
            root.join("app").to_str().unwrap(),
            "--bin",
            root.join("app-bin").to_str().unwrap(),
            "--out",
            root.join("out").to_str().unwrap(),
            "--layout",
            "dir",
        ],
    );
    assert!(ok, "{text}");
    let font = std::fs::read(root.join("out/app/project/.byard/deps/kit/fonts/Icons.ttf")).unwrap();
    assert_eq!(drawn(&font), ["search"], "{text}");
}

/// The committed sample checks clean: its font is a file that exists and parses,
/// and every name in it is static.
#[test]
fn the_icon_font_sample_checks_clean() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/icon_font");
    let (ok, text) = run(&dir, &["check", dir.to_str().unwrap()]);
    assert!(ok && text.contains("0 errors"), "{text}");
    assert!(!text.contains("IconNameNotStatic"), "{text}");
}
