//! `byard bundle`: an app packaged with its project, to run where neither its
//! sources nor its dependencies' sources are.

use std::path::{Path, PathBuf};
use std::process::Command;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// An app that uses `dial`, a path dependency which itself uses `tick`, with
/// an icon, and with the things a copy leaves out: Rust sources, build
/// output, a lockfile and a hidden directory.
fn workspace(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("byard-bundle-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    write(
        &root.join("vendor/tick/byard.toml"),
        "[package]\nname = \"tick\"\nversion = \"0.1.0\"\n",
    );
    write(
        &root.join("vendor/tick/src/tick.byd"),
        "View Tick() { Text(\"tick\") }\n",
    );
    write(
        &root.join("dial/byard.toml"),
        "[package]\nname = \"dial\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\ntick = { path = \"../vendor/tick\" }\n",
    );
    write(
        &root.join("dial/src/dial.byd"),
        "use tick as k\nView Dial() { k.Tick() }\n",
    );
    let app = root.join("app");
    write(
        &app.join("byard.toml"),
        "[project]\nname = \"dials\"\nentry = \"src/main.byd\"\nicon = \"icon.png\"\n\n\
         [bundle]\nname = \"Dials\"\nid = \"com.example.dials\"\nversion = \"1.4.0\"\n\n\
         # The dependency is where it is on this machine.\n\
         [dependencies]\ndial = { path = \"../dial\" }\n",
    );
    write(
        &app.join("src/main.byd"),
        "use dial as d\nView Main() { d.Dial() }\n",
    );
    write(&app.join("src/lib.rs"), "// compiled into the binary\n");
    write(&app.join("tests/it/main.rs"), "// the Rust half's tests\n");
    write(
        &app.join("src/ffi/bridge.rs"),
        "// a directory of Rust only\n",
    );
    write(&app.join("Cargo.toml"), "[package]\nname = \"dials\"\n");
    write(&app.join("byard.lock"), "");
    write(&app.join("target/release/junk"), "junk");
    write(&app.join(".git/HEAD"), "ref");
    image::RgbaImage::from_pixel(128, 128, image::Rgba([40, 90, 200, 255]))
        .save(app.join("icon.png"))
        .unwrap();
    write(&root.join("dials-bin"), "#!/bin/sh\n");
    root
}

fn bundle(root: &Path, layout: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("bundle")
        .arg(root.join("app"))
        .arg("--bin")
        .arg(root.join("dials-bin"))
        .arg("--out")
        .arg(root.join("out"))
        .arg("--layout")
        .arg(layout)
        .output()
        .expect("run `byard bundle`");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// The project copy holds what the app reads and nothing it does not.
fn assert_project_copy(project: &Path) {
    assert!(project.join("byard.toml").is_file());
    assert!(project.join("src/main.byd").is_file());
    assert!(project.join("icon.png").is_file());
    for gone in [
        "src/lib.rs",
        "src/ffi",
        "tests",
        "Cargo.toml",
        "byard.lock",
        "target",
        ".git",
    ] {
        assert!(!project.join(gone).exists(), "{gone} was copied");
    }
    // The dependencies travel inside it, each pointed at the other copy.
    assert!(project.join(".byard/deps/dial/src/dial.byd").is_file());
    assert!(project.join(".byard/deps/tick/src/tick.byd").is_file());
    let manifest = std::fs::read_to_string(project.join("byard.toml")).unwrap();
    assert!(
        manifest.contains("dial = { path = \".byard/deps/dial\" }"),
        "{manifest}"
    );
    assert!(
        manifest.contains("# The dependency is where it is"),
        "comments are kept"
    );
    let dial = std::fs::read_to_string(project.join(".byard/deps/dial/byard.toml")).unwrap();
    assert!(dial.contains("tick = { path = \"../tick\" }"), "{dial}");
}

#[test]
fn a_mac_bundle_carries_the_binary_the_project_and_its_packages() {
    let root = workspace("app");
    let (ok, text) = bundle(&root, "app");
    assert!(ok, "{text}");
    let contents = root.join("out/Dials.app/Contents");
    assert!(contents.join("MacOS/dials-bin").is_file());
    assert_project_copy(&contents.join("Resources/project"));

    let plist = std::fs::read_to_string(contents.join("Info.plist")).unwrap();
    for (key, value) in [
        ("CFBundleName", "Dials"),
        ("CFBundleIdentifier", "com.example.dials"),
        ("CFBundleShortVersionString", "1.4.0"),
        ("CFBundleExecutable", "dials-bin"),
        ("CFBundleIconFile", "icon.icns"),
    ] {
        assert!(
            plist.contains(&format!("<key>{key}</key>\n    <string>{value}</string>")),
            "{key}: {plist}"
        );
    }
    // The icon at every size up to its own 128 px, and none above it.
    let icns = std::fs::read(contents.join("Resources/icon.icns")).unwrap();
    assert_eq!(&icns[..4], b"icns");
    assert_eq!(
        u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize,
        icns.len()
    );
    let mut kinds = Vec::new();
    let mut at = 8;
    while at < icns.len() {
        kinds.push(String::from_utf8_lossy(&icns[at..at + 4]).to_string());
        let len = u32::from_be_bytes(icns[at + 4..at + 8].try_into().unwrap()) as usize;
        assert_eq!(&icns[at + 9..at + 12], b"PNG", "each size is a PNG");
        at += len;
    }
    assert_eq!(kinds, ["icp4", "icp5", "ic11", "ic12", "ic07"]);

    // Bundling again replaces the bundle rather than adding to it.
    std::fs::write(contents.join("stale"), "x").unwrap();
    let (ok, text) = bundle(&root, "app");
    assert!(ok, "{text}");
    assert!(!contents.join("stale").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_directory_bundle_puts_the_project_beside_the_binary() {
    let root = workspace("dir");
    let (ok, text) = bundle(&root, "dir");
    assert!(ok, "{text}");
    let bundle = root.join("out/dials");
    assert!(bundle.join("dials-bin").is_file());
    assert_project_copy(&bundle.join("project"));
    assert!(!bundle.join("Info.plist").exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// A project that reads a file from outside its directory would bundle
/// without it; that is an error at bundle time, naming the cause.
#[test]
fn a_project_reaching_outside_itself_does_not_bundle() {
    let root = workspace("outside");
    std::fs::create_dir_all(root.join("shared")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/aura_weather/fonts/Manrope-Variable.ttf"),
        root.join("shared/Display.ttf"),
    )
    .unwrap();
    let manifest = root.join("app/byard.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        format!("{text}\n[assets.fonts]\nDisplay = \"../shared/Display.ttf\"\n"),
    )
    .unwrap();
    let (ok, text) = bundle(&root, "dir");
    assert!(!ok, "{text}");
    assert!(
        text.contains("bundle's project") && text.contains("outside"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_binary_is_named() {
    let root = workspace("nobin");
    std::fs::remove_file(root.join("dials-bin")).unwrap();
    let (ok, text) = bundle(&root, "dir");
    assert!(!ok);
    assert!(
        text.contains("dials-bin") && text.contains("cargo build"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A project with an error is not bundled, and neither is a library.
#[test]
fn a_project_with_an_error_or_no_entry_does_not_bundle() {
    let root = workspace("broken");
    write(
        &root.join("app/src/main.byd"),
        "use dial as d\nView Main() { d.Dail() }\n",
    );
    let (ok, text) = bundle(&root, "dir");
    assert!(!ok, "{text}");
    assert!(text.contains("before bundling"), "{text}");

    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("bundle")
        .arg(root.join("dial"))
        .arg("--bin")
        .arg(root.join("dials-bin"))
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("library package"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}
