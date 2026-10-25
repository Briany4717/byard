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
         [bundle]\nname = \"Dials\"\nid = \"com.example.dials\"\nversion = \"1.4.0\"\n\
         categories = [\"Utility\", \"Calculator\"]\nsummary = \"Dials for things\"\n\
         license = \"MIT OR Apache-2.0\"\n\n\
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
    bundle_with(root, layout, &[])
}

fn bundle_with(root: &Path, layout: &str, extra: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("bundle")
        .arg(root.join("app"))
        .arg("--bin")
        .arg(root.join("dials-bin"))
        .arg("--out")
        .arg(root.join("out"))
        .arg("--layout")
        .arg(layout)
        .args(extra)
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

/// The `key=value` lines of a `.desktop` file's one group.
fn desktop_keys(path: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.starts_with("[Desktop Entry]\n"), "{text}");
    text.lines()
        .skip(1)
        .map(|l| {
            let (k, v) = l.split_once('=').unwrap_or_else(|| panic!("{l}"));
            (k.to_string(), v.to_string())
        })
        .collect()
}

fn key<'a>(keys: &'a [(String, String)], name: &str) -> Option<&'a str> {
    keys.iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

#[test]
fn a_linux_bundle_is_a_prefix_with_a_launcher_entry() {
    let root = workspace("linux");
    let (ok, text) = bundle(&root, "linux");
    assert!(ok, "{text}");
    let prefix = root.join("out/dials-bin");
    assert!(prefix.join("bin/dials-bin").is_file());
    assert_project_copy(&prefix.join("share/dials-bin/project"));
    // Nothing of the other layouts.
    for other in ["project", "dials-bin", "Contents", "Info.plist"] {
        assert!(!prefix.join(other).exists(), "{other}");
    }

    let keys = desktop_keys(&prefix.join("share/applications/com.example.dials.desktop"));
    for (k, v) in [
        ("Type", "Application"),
        ("Name", "Dials"),
        ("Comment", "Dials for things"),
        ("Exec", "dials-bin"),
        ("Icon", "com.example.dials"),
        ("Terminal", "false"),
        ("Categories", "Utility;Calculator;"),
        ("StartupWMClass", "com.example.dials"),
    ] {
        assert_eq!(key(&keys, k), Some(v), "{k}: {keys:?}");
    }

    let info =
        std::fs::read_to_string(prefix.join("share/metainfo/com.example.dials.metainfo.xml"))
            .unwrap();
    for line in [
        "<component type=\"desktop-application\">",
        "<id>com.example.dials</id>",
        "<name>Dials</name>",
        "<summary>Dials for things</summary>",
        "<metadata_license>CC0-1.0</metadata_license>",
        "<project_license>MIT OR Apache-2.0</project_license>",
        "<launchable type=\"desktop-id\">com.example.dials.desktop</launchable>",
    ] {
        assert!(info.lines().any(|l| l.trim() == line), "{line}: {info}");
    }

    // The icon at every hicolor size up to its own 128 px, and none above it,
    // each a PNG of that size.
    let icons = prefix.join("share/icons/hicolor");
    let mut sizes: Vec<String> = std::fs::read_dir(&icons)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    sizes.sort();
    assert_eq!(
        sizes,
        ["128x128", "16x16", "32x32", "48x48", "64x64"],
        "{sizes:?}"
    );
    for size in [16u32, 32, 48, 64, 128] {
        let file = icons.join(format!("{size}x{size}/apps/com.example.dials.png"));
        let png = image::open(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert_eq!((png.width(), png.height()), (size, size));
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A source larger than 512 px stops at 512, and a small one is not scaled up.
#[test]
fn the_hicolor_icons_stop_at_the_source_or_512() {
    for (source, expect) in [
        (600u32, vec![16u32, 32, 48, 64, 128, 256, 512]),
        (256, vec![16, 32, 48, 64, 128, 256]),
        (50, vec![16, 32, 48]),
    ] {
        let root = workspace(&format!("linux-icon-{source}"));
        image::RgbaImage::from_pixel(source, source, image::Rgba([9, 9, 9, 255]))
            .save(root.join("app/icon.png"))
            .unwrap();
        let (ok, text) = bundle(&root, "linux");
        assert!(ok, "{text}");
        let icons = root.join("out/dials-bin/share/icons/hicolor");
        let mut sizes: Vec<u32> = std::fs::read_dir(&icons)
            .unwrap()
            .map(|e| {
                let name = e.unwrap().file_name().to_string_lossy().into_owned();
                name.split('x').next().unwrap().parse().unwrap()
            })
            .collect();
        sizes.sort_unstable();
        assert_eq!(sizes, expect, "a {source} px icon");
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// An icon that is not square is refused, as for `.icns`.
#[test]
fn a_linux_bundle_refuses_an_icon_that_is_not_square() {
    let root = workspace("linux-wide");
    image::RgbaImage::from_pixel(64, 32, image::Rgba([9, 9, 9, 255]))
        .save(root.join("app/icon.png"))
        .unwrap();
    let (ok, text) = bundle(&root, "linux");
    assert!(!ok, "{text}");
    assert!(text.contains("square") && text.contains("64x32"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Without `[bundle]` keys the entry is still complete, and its id is what
/// the window announces for an app that names none.
#[test]
fn a_linux_bundle_with_no_bundle_table_falls_back_to_the_executable() {
    let root = workspace("linux-bare");
    let manifest = root.join("app/byard.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let bare = text.split("[bundle]").next().unwrap().to_string()
        + "[dependencies]"
        + text.split("[dependencies]").nth(1).unwrap();
    std::fs::write(&manifest, bare).unwrap();
    let (ok, text) = bundle(&root, "linux");
    assert!(ok, "{text}");
    let desktop = root.join("out/dials-bin/share/applications/dev.byard.dials-bin.desktop");
    let keys = desktop_keys(&desktop);
    assert_eq!(key(&keys, "Name"), Some("dials"));
    assert_eq!(key(&keys, "Categories"), Some("Utility;"));
    assert_eq!(key(&keys, "StartupWMClass"), Some("dev.byard.dials-bin"));
    assert_eq!(key(&keys, "Comment"), None);
    // No licence line, and the name stands for the summary.
    let info = std::fs::read_to_string(
        root.join("out/dials-bin/share/metainfo/dev.byard.dials-bin.metainfo.xml"),
    )
    .unwrap();
    assert!(!info.contains("project_license"), "{info}");
    assert!(info.contains("<summary>dials</summary>"), "{info}");
    // The icon is named by the id, and written under it.
    assert_eq!(key(&keys, "Icon"), Some("dev.byard.dials-bin"));
    assert!(
        root.join("out/dials-bin/share/icons/hicolor/16x16/apps/dev.byard.dials-bin.png")
            .is_file()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// An app with no icon has no `Icon` key and no icon files to point at.
#[test]
fn a_linux_bundle_without_an_icon_writes_none() {
    let root = workspace("linux-noicon");
    let manifest = root.join("app/byard.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replace("icon = \"icon.png\"\n", "")).unwrap();
    let (ok, text) = bundle(&root, "linux");
    assert!(ok, "{text}");
    let prefix = root.join("out/dials-bin");
    let keys = desktop_keys(&prefix.join("share/applications/com.example.dials.desktop"));
    assert_eq!(key(&keys, "Icon"), None);
    assert!(!prefix.join("share/icons").exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// Whether `tool` is installed: a CI image may not have the validators.
fn installed(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok()
}

/// What the template writes passes the validators the desktop's own tooling
/// ships. A tool that is not installed is skipped, not failed.
#[test]
fn the_entries_pass_the_freedesktop_validators() {
    let root = workspace("linux-validate");
    let (ok, text) = bundle(&root, "linux");
    assert!(ok, "{text}");
    let share = root.join("out/dials-bin/share");
    let mut ran = 0;
    if installed("desktop-file-validate") {
        ran += 1;
        let out = Command::new("desktop-file-validate")
            .env("LC_ALL", "C")
            .arg(share.join("applications/com.example.dials.desktop"))
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && said.trim().is_empty(), "{said}");
        // The bundle command ran it too, and found nothing to say.
        assert!(!text.contains("desktop-file-validate"), "{text}");
    }
    if installed("appstreamcli") {
        ran += 1;
        let out = Command::new("appstreamcli")
            .env("LC_ALL", "C")
            .args(["validate", "--no-net"])
            .arg(share.join("metainfo/com.example.dials.metainfo.xml"))
            .output()
            .unwrap();
        // A homepage is not something `[bundle]` can say, so its absence is
        // a warning; an error is what a malformed file would be.
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(!said.lines().any(|l| l.starts_with("E:")), "{said}");
    }
    if ran == 0 {
        eprintln!("skipped: neither desktop-file-validate nor appstreamcli is installed");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A validator that objects is heard, and one that is absent is not an error.
#[test]
fn the_bundle_prints_what_a_validator_objects_to() {
    let root = workspace("linux-warn");
    let manifest = root.join("app/byard.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace(
            "[\"Utility\", \"Calculator\"]",
            "[\"Utility\", \"Weather\"]",
        ),
    )
    .unwrap();
    let (ok, text) = bundle(&root, "linux");
    assert!(ok, "a warning does not fail the bundle: {text}");
    if installed("desktop-file-validate") {
        assert!(
            text.contains("warning: desktop-file-validate:") && text.contains("Weather"),
            "{text}"
        );
    }

    // With no tools on the path, the same bundle is written, in silence.
    let none = root.join("empty-path");
    std::fs::create_dir_all(&none).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .env("PATH", &none)
        .arg("bundle")
        .arg(root.join("app"))
        .arg("--bin")
        .arg(root.join("dials-bin"))
        .arg("--out")
        .arg(root.join("out"))
        .args(["--layout", "linux"])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    assert!(!said.contains("warning:"), "{said}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Every file under `dir`, as its `/`-separated path from `dir`.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = format!("{rel}{}", entry.file_name().to_string_lossy());
            if entry.path().is_dir() {
                walk(&entry.path(), &format!("{name}/"), out);
            } else {
                out.push(name);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort();
    out
}

#[test]
fn the_archive_lists_the_files_of_the_tree() {
    let root = workspace("linux-archive");
    let (ok, text) = bundle_with(&root, "linux", &["--archive"]);
    assert!(ok, "{text}");
    let name = format!("dials-bin-1.4.0-linux-{}.tar.gz", std::env::consts::ARCH);
    let archive = root.join("out").join(&name);
    assert!(
        archive.is_file(),
        "{name} in {:?}",
        tree(&root.join("out")).len()
    );
    assert!(text.contains(&name), "{text}");

    let prefix = root.join("out/dials-bin");
    let bytes = std::fs::read(&archive).unwrap();
    let unpacked = root.join("unpacked");
    let mut listed = byard_project::archive::unpack(&bytes, &unpacked).unwrap();
    listed.sort();
    assert_eq!(listed, tree(&prefix));
    for path in &listed {
        assert_eq!(
            std::fs::read(unpacked.join(path)).unwrap(),
            std::fs::read(prefix.join(path)).unwrap(),
            "{path}"
        );
    }
    // The program is runnable once unpacked, and nothing else is.
    let mut tar = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&bytes[..]), &mut tar).unwrap();
    let mut at = 0;
    let mut modes = Vec::new();
    while tar[at..at + 512].iter().any(|&b| b != 0) {
        let field = |a: usize, b: usize| {
            String::from_utf8_lossy(&tar[at + a..at + b])
                .trim_end_matches('\0')
                .to_string()
        };
        let size = usize::from_str_radix(field(124, 136).trim(), 8).unwrap();
        modes.push((field(0, 100), field(100, 108)));
        at += 512 + size.div_ceil(512) * 512;
    }
    for (path, mode) in &modes {
        let expect = if path.starts_with("bin/") {
            "0000755"
        } else {
            "0000644"
        };
        assert_eq!(mode, expect, "{path}");
    }
    assert_eq!(modes.len(), listed.len());

    // The system's own `tar` reads the same list, when there is one.
    if let Ok(out) = Command::new("tar").arg("-tzf").arg(&archive).output() {
        let mut theirs: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        theirs.sort();
        assert_eq!(theirs, listed);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The archive is a Linux layout's, and says so rather than guessing.
#[test]
fn an_archive_of_another_layout_is_refused() {
    let root = workspace("archive-dir");
    let (ok, text) = bundle_with(&root, "dir", &["--archive"]);
    assert!(!ok, "{text}");
    assert!(text.contains("--layout linux"), "{text}");
    assert!(!root.join("out").exists(), "nothing was written");
    let _ = std::fs::remove_dir_all(&root);
}

/// Without `--archive` there is none.
#[test]
fn a_linux_bundle_has_no_archive_unless_asked() {
    let root = workspace("linux-noarchive");
    let (ok, text) = bundle(&root, "linux");
    assert!(ok, "{text}");
    assert_eq!(
        std::fs::read_dir(root.join("out")).unwrap().count(),
        1,
        "only the tree"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The project in the prefix, at `../share/<exe>/project` from the binary
/// where the program looks for it, loads and checks clean as the app will
/// load it.
#[test]
fn the_project_in_a_linux_prefix_checks_clean() {
    let root = workspace("linux-resolves");
    let (ok, text) = bundle(&root, "linux");
    assert!(ok, "{text}");
    let prefix = root.join("out/dials-bin");
    let project = prefix.join("bin/../share/dials-bin/project");
    // `byard check` on the tree's copy, as the app resolves it.
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(&project)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    let _ = std::fs::remove_dir_all(&root);
}
