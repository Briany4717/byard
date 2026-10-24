//! `byard bundle --bin <exe>`: an app packaged to run on a machine that has
//! neither its sources nor the Byard tools.
//!
//! An app reads its project when it starts: the views, the manifest, fonts and
//! pictures. A binary built with `App::new(env!("CARGO_MANIFEST_DIR"))` looks
//! for them where it was compiled, and when that path is not there it looks
//! beside itself, which is where this command puts a copy:
//!
//! ```text
//! macOS (--layout app)                 elsewhere (--layout dir)
//! Name.app/Contents/                   name/
//!   Info.plist                           exe
//!   MacOS/exe                            project/
//!   Resources/icon.icns                    byard.toml, src/, fonts/, ...
//!   Resources/project/
//!     byard.toml, src/, fonts/, ...
//! ```
//!
//! `--layout linux` writes a freedesktop.org prefix instead (RFC-0045), which
//! `cp -r name/* ~/.local/` installs for one user and a distribution package
//! maps onto `/usr` as it is:
//!
//! ```text
//! name/
//!   bin/exe
//!   share/exe/project/                 byard.toml, src/, fonts/, ...
//!   share/applications/id.desktop
//!   share/icons/hicolor/NxN/apps/id.png
//!   share/metainfo/id.metainfo.xml
//! ```
//!
//! The copy is the project directory as it is, less what is not read at run
//! time: build output (`target/`), hidden entries (`.byard/`, `.git`), the Rust
//! half, which is already in the binary (`*.rs`, `Cargo.toml`, `Cargo.lock`),
//! and `byard.lock`. Dependencies are carried too. Each resolved package is
//! copied under the copy's `.byard/deps/` and every manifest that names it is
//! rewritten to that path, so the bundle needs neither `~/.byard/cache` nor
//! the `../` a path dependency named on the building machine.
//!
//! The bundle is then resolved as the app will resolve it, and a bundle that
//! does not is an error here rather than a blank window on someone else's
//! machine.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::deps::resolve_project;
use crate::manifest::{BundleConfig, Manifest};
use crate::style;

/// The shape of the package.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Layout {
    /// A macOS application bundle, `Name.app`.
    App,
    /// A directory holding the executable and `project/` beside it.
    Dir,
    /// A freedesktop.org prefix: `bin/`, the project under `share/<exe>/`, a
    /// `.desktop` entry, hicolor icons and `AppStream` metainfo.
    Linux,
}

impl Layout {
    /// The layout of the system this runs on.
    #[must_use]
    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Self::App
        } else {
            Self::Dir
        }
    }
}

/// What a bundle is called, the defaults filled in.
struct Naming {
    name: String,
    id: String,
    version: String,
}

pub fn run(
    file: Option<&Path>,
    bin: &Path,
    out: Option<&Path>,
    layout: Layout,
) -> Result<(), String> {
    if !bin.is_file() {
        return Err(format!(
            "--bin {}: there is no executable there\n\
             hint: build it first, e.g. `cargo build --release`",
            bin.display()
        ));
    }
    let (manifest, provider) = load(file)?;
    let exe = bin
        .file_name()
        .ok_or_else(|| format!("--bin {}: not a file", bin.display()))?
        .to_string_lossy()
        .into_owned();
    let naming = naming(&manifest, &exe, layout);
    let out = out.map_or_else(
        || manifest.project_root.join(".byard/bundle"),
        Path::to_path_buf,
    );
    let deps: Vec<(String, PathBuf)> = provider
        .resolved_roots()
        .iter()
        .map(|(name, root)| (name.clone(), root.clone()))
        .collect();

    let written = write_bundle(&manifest, &naming, &deps, bin, &out, layout)?;

    // Load the copy as the app will, with the checks the original passed.
    load(Some(&written.project)).map_err(|e| {
        format!(
            "the bundle's project does not load: {e}\n\
             hint: a bundle carries the project directory and its packages; \
             a file named by a path outside them stays behind"
        )
    })?;

    style::fact("Bundle", &written.root.display().to_string());
    style::fact("Name", &naming.name);
    style::fact("Id", &naming.id);
    style::fact("Version", &naming.version);
    if !deps.is_empty() {
        style::fact("Packages", &deps.len().to_string());
    }
    if manifest.bundle.id.is_none() {
        eprintln!(
            "note: no [bundle] id in byard.toml, so the app is `{}`; \
             set one before shipping, its settings are kept under it",
            naming.id
        );
    }
    Ok(())
}

/// The project at `path`, resolved, an app's, and free of errors.
fn load(path: Option<&Path>) -> Result<(Manifest, crate::deps::FsProvider), String> {
    let manifest = Manifest::discover(path)?;
    manifest.require_app("bundle")?;
    let (program, provider) = resolve_project(&manifest)?;
    if !program.errors.is_empty() {
        crate::commands::check::print_diagnostics(&program.errors, &program.source_map, false);
        return Err(format!(
            "{} error(s), fix them before bundling",
            program.errors.len()
        ));
    }
    Ok((manifest, provider))
}

/// The names a bundle carries. With no `[bundle] id` the id is a local one,
/// except in the Linux layout, where it names the `.desktop` file and has to
/// be what the window announces: `dev.byard.<executable>`.
fn naming(manifest: &Manifest, exe: &str, layout: Layout) -> Naming {
    let name = manifest
        .bundle
        .name
        .clone()
        .unwrap_or_else(|| manifest.name.clone());
    let id = manifest.bundle.id.clone().unwrap_or_else(|| {
        if layout == Layout::Linux {
            return format!("dev.byard.{exe}");
        }
        let part: String = manifest
            .name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        format!("local.byard.{part}")
    });
    let version = manifest
        .bundle
        .version
        .clone()
        .unwrap_or_else(|| "0.1.0".to_string());
    Naming { name, id, version }
}

/// Where a bundle was written.
struct Written {
    /// The bundle itself: `Name.app` or the directory.
    root: PathBuf,
    /// The project copy inside it.
    project: PathBuf,
}

fn write_bundle(
    manifest: &Manifest,
    naming: &Naming,
    deps: &[(String, PathBuf)],
    bin: &Path,
    out: &Path,
    layout: Layout,
) -> Result<Written, String> {
    let exe = bin
        .file_name()
        .ok_or_else(|| format!("--bin {}: not a file", bin.display()))?;
    let (root, exe_dir, project) = match layout {
        Layout::App => {
            let root = out.join(format!("{}.app", naming.name));
            let contents = root.join("Contents");
            (
                root,
                contents.join("MacOS"),
                contents.join("Resources/project"),
            )
        }
        Layout::Dir => {
            let root = out.join(&manifest.name);
            (root.clone(), root.clone(), root.join("project"))
        }
        Layout::Linux => {
            let root = out.join(&manifest.name);
            let project = root.join("share").join(exe).join("project");
            (root.clone(), root.join("bin"), project)
        }
    };
    // Only ever this command's own output: a bundle is rewritten whole.
    if root.exists() {
        std::fs::remove_dir_all(&root).map_err(|e| io(&root, &e))?;
    }
    std::fs::create_dir_all(&exe_dir).map_err(|e| io(&exe_dir, &e))?;
    std::fs::copy(bin, exe_dir.join(exe)).map_err(|e| io(bin, &e))?;

    copy_project(&manifest.project_root, &project)?;
    rewrite_dependencies(&project.join("byard.toml"), |name| {
        format!(".byard/deps/{name}")
    })?;
    for (name, dep_root) in deps {
        let to = project.join(".byard/deps").join(name);
        copy_project(dep_root, &to)?;
        rewrite_dependencies(&to.join("byard.toml"), |name| format!("../{name}"))?;
    }

    if layout == Layout::App {
        let resources = root.join("Contents/Resources");
        let icon_file = match &manifest.icon {
            Some(icon) => {
                let icns = icns_from(icon)?;
                let path = resources.join("icon.icns");
                std::fs::write(&path, icns).map_err(|e| io(&path, &e))?;
                Some("icon.icns")
            }
            None => None,
        };
        let plist = info_plist(naming, &exe.to_string_lossy(), icon_file);
        let path = root.join("Contents/Info.plist");
        std::fs::write(&path, plist).map_err(|e| io(&path, &e))?;
    }
    if layout == Layout::Linux {
        write_linux_entries(manifest, naming, &exe.to_string_lossy(), &root)?;
    }
    Ok(Written { root, project })
}

/// Writes `text` to `path`, its directory made.
fn write_file(path: &Path, text: impl AsRef<[u8]>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io(dir, &e))?;
    }
    std::fs::write(path, text).map_err(|e| io(path, &e))
}

/// What the desktop reads of a Linux prefix: the launcher entry.
fn write_linux_entries(
    manifest: &Manifest,
    naming: &Naming,
    exe: &str,
    root: &Path,
) -> Result<(), String> {
    let share = root.join("share");
    let desktop = desktop_entry(naming, &manifest.bundle, exe, manifest.icon.is_some());
    write_file(
        &share
            .join("applications")
            .join(format!("{}.desktop", naming.id)),
        desktop,
    )
}

/// A value of a `.desktop` file: a backslash and a line break are written
/// escaped, which is all its string type reserves.
fn desktop_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "")
}

/// One argument of an `Exec` line: bare when it is plain, else quoted with
/// the characters the entry specification reserves escaped (and then the
/// escapes escaped again, as a string value).
fn exec_argument(arg: &str) -> String {
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
    {
        return arg.to_string();
    }
    let mut quoted = String::from("\"");
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        if c == '%' {
            quoted.push('%');
        }
        quoted.push(c);
    }
    quoted.push('"');
    desktop_escape(&quoted)
}

/// The launcher entry. `Exec` is the bare executable name, found through
/// `PATH`, so the prefix can be anywhere on it; `StartupWMClass` is the id
/// the app's window announces, which is how a shell pairs the two.
fn desktop_entry(naming: &Naming, bundle: &BundleConfig, exe: &str, icon: bool) -> String {
    let categories = if bundle.categories.is_empty() {
        "Utility;".to_string()
    } else {
        bundle.categories.iter().fold(String::new(), |mut out, c| {
            let _ = write!(out, "{c};");
            out
        })
    };
    let mut out = String::from("[Desktop Entry]\nType=Application\n");
    let _ = writeln!(out, "Name={}", desktop_escape(&naming.name));
    if let Some(summary) = &bundle.summary {
        let _ = writeln!(out, "Comment={}", desktop_escape(summary));
    }
    let _ = writeln!(out, "Exec={}", exec_argument(exe));
    if icon {
        let _ = writeln!(out, "Icon={}", naming.id);
    }
    let _ = writeln!(out, "Terminal=false");
    let _ = writeln!(out, "Categories={categories}");
    let _ = writeln!(out, "StartupWMClass={}", naming.id);
    out
}

fn io(what: &Path, e: &std::io::Error) -> String {
    format!("{}: {e}", what.display())
}

/// Whether an entry of a project directory is left out of its copy. None of
/// it is a `.byd` the program is made of: the resolver skips hidden entries
/// and `target/` too, and Cargo's own directories (`tests/`, `benches/`,
/// `examples/`) are left out only from a crate whose views are in `src/`,
/// where the resolver does not look beyond.
fn left_out(name: &str, is_dir: bool, rust_crate: bool) -> bool {
    name.starts_with('.')
        || (is_dir && name == "target")
        || (is_dir && rust_crate && matches!(name, "tests" | "benches" | "examples"))
        || (!is_dir
            && (Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
                || matches!(name, "Cargo.toml" | "Cargo.lock" | "byard.lock")))
}

fn copy_project(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut entries: Vec<_> = std::fs::read_dir(from)
        .map_err(|e| format!("{}: {e}", from.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", from.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let rust_crate = from.join("Cargo.toml").is_file() && from.join("src").is_dir();
    for entry in entries {
        let path = entry.path();
        let is_dir = path.is_dir();
        let name = entry.file_name();
        if left_out(&name.to_string_lossy(), is_dir, rust_crate) {
            continue;
        }
        let dest = to.join(&name);
        if is_dir {
            copy_project(&path, &dest)?;
            // A directory of nothing but left-out files is not carried.
            let _ = std::fs::remove_dir(&dest);
        } else {
            std::fs::copy(&path, &dest).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Points every `[dependencies]` entry of the manifest at `path_of(name)`.
fn rewrite_dependencies(manifest: &Path, path_of: impl Fn(&str) -> String) -> Result<(), String> {
    let text =
        std::fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", manifest.display()))?;
    let Some(deps) = doc
        .get_mut("dependencies")
        .and_then(toml_edit::Item::as_table_like_mut)
    else {
        return Ok(());
    };
    let names: Vec<String> = deps.iter().map(|(k, _)| k.to_string()).collect();
    for name in names {
        let mut spec = toml_edit::InlineTable::new();
        spec.insert("path", path_of(&name).into());
        deps.insert(&name, toml_edit::value(spec));
    }
    std::fs::write(manifest, doc.to_string()).map_err(|e| format!("{}: {e}", manifest.display()))
}

/// The sizes an `.icns` holds and the type of each. Every one of these
/// accepts PNG data; the second of each pair of a size is the Retina version
/// of the one half its size (`ic11` is 16 pt at 2x).
const ICNS_SIZES: [(u32, &[u8; 4]); 10] = [
    (16, b"icp4"),
    (32, b"icp5"),
    (32, b"ic11"),
    (64, b"ic12"),
    (128, b"ic07"),
    (256, b"ic13"),
    (256, b"ic08"),
    (512, b"ic14"),
    (512, b"ic09"),
    (1024, b"ic10"),
];

/// A macOS icon file from the project's icon: the picture at every size up
/// to its own, so none is scaled up past what it holds.
fn icns_from(icon: &Path) -> Result<Vec<u8>, String> {
    let picture = image::open(icon).map_err(|e| format!("{}: {e}", icon.display()))?;
    let (w, h) = (picture.width(), picture.height());
    if w != h {
        return Err(format!(
            "{}: an app's icon is square, this one is {w}x{h}",
            icon.display()
        ));
    }
    let mut chunks = Vec::new();
    for (size, kind) in ICNS_SIZES {
        if size > w.max(16) {
            break;
        }
        let scaled = picture.resize_exact(size, size, image::imageops::FilterType::Lanczos3);
        let mut png = Vec::new();
        scaled
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        chunks.push((kind, png));
    }
    Ok(icns(&chunks))
}

/// The `.icns` container: `icns`, its length, then each chunk as its type,
/// its length (header included) and its data, lengths big-endian.
fn icns(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let len = |n: usize| u32::try_from(n).expect("an icon under 4 GiB").to_be_bytes();
    let body: usize = chunks.iter().map(|(_, data)| 8 + data.len()).sum();
    let mut out = Vec::with_capacity(8 + body);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&len(8 + body));
    for (kind, data) in chunks {
        out.extend_from_slice(*kind);
        out.extend_from_slice(&len(8 + data.len()));
        out.extend_from_slice(data);
    }
    out
}

fn info_plist(naming: &Naming, exe: &str, icon: Option<&str>) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut keys = vec![
        ("CFBundleName", escape(&naming.name)),
        ("CFBundleDisplayName", escape(&naming.name)),
        ("CFBundleIdentifier", escape(&naming.id)),
        ("CFBundleVersion", escape(&naming.version)),
        ("CFBundleShortVersionString", escape(&naming.version)),
        ("CFBundleExecutable", escape(exe)),
        ("CFBundlePackageType", "APPL".to_string()),
        ("CFBundleInfoDictionaryVersion", "6.0".to_string()),
    ];
    if let Some(icon) = icon {
        keys.push(("CFBundleIconFile", escape(icon)));
    }
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    for (key, value) in keys {
        let _ = write!(out, "    <key>{key}</key>\n    <string>{value}</string>\n");
    }
    out.push_str("    <key>NSHighResolutionCapable</key>\n    <true/>\n</dict>\n</plist>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icns_container_is_typed_and_sized() {
        let bytes = icns(&[(b"ic07", vec![1, 2, 3]), (b"ic08", vec![4])]);
        assert_eq!(&bytes[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
            8 + 11 + 9
        );
        assert_eq!(bytes.len(), 28);
        assert_eq!(&bytes[8..12], b"ic07");
        assert_eq!(u32::from_be_bytes(bytes[12..16].try_into().unwrap()), 11);
        assert_eq!(&bytes[16..19], &[1, 2, 3]);
        assert_eq!(&bytes[19..23], b"ic08");
    }

    #[test]
    fn what_a_copy_leaves_out() {
        for (name, dir) in [
            (".git", true),
            (".byard", true),
            ("target", true),
            ("lib.rs", false),
            ("Cargo.toml", false),
            ("byard.lock", false),
        ] {
            assert!(left_out(name, dir, false), "{name}");
        }
        for (name, dir) in [
            ("src", true),
            ("main.byd", false),
            ("byard.toml", false),
            ("target.png", false),
        ] {
            assert!(!left_out(name, dir, false), "{name}");
        }
        // `tests/` is Cargo's only in a crate with its views in `src/`.
        assert!(left_out("tests", true, true));
        assert!(!left_out("tests", true, false));
        assert!(!left_out("tests.byd", false, true));
    }

    #[test]
    fn the_plist_names_the_app_and_escapes_what_it_writes() {
        let naming = Naming {
            name: "Tom & Jerry".to_string(),
            id: "com.example.tj".to_string(),
            version: "2.0.1".to_string(),
        };
        let plist = info_plist(&naming, "tj", Some("icon.icns"));
        assert!(
            plist.contains("<string>Tom &amp; Jerry</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<key>CFBundleIdentifier</key>\n    <string>com.example.tj</string>")
        );
        assert!(plist.contains("<key>CFBundleExecutable</key>\n    <string>tj</string>"));
        assert!(plist.contains("<key>CFBundleIconFile</key>"));
        assert!(!info_plist(&naming, "tj", None).contains("CFBundleIconFile"));
    }

    fn naming_of(id: &str) -> Naming {
        Naming {
            name: "Tom & Jerry".to_string(),
            id: id.to_string(),
            version: "2.0.1".to_string(),
        }
    }

    #[test]
    fn the_desktop_entry_names_the_app_the_window_and_the_icon() {
        let bundle = BundleConfig {
            summary: Some("Cat\nand mouse".to_string()),
            categories: vec!["Game".to_string(), "Simulation".to_string()],
            ..BundleConfig::default()
        };
        let entry = desktop_entry(&naming_of("com.example.tj"), &bundle, "tj", true);
        for line in [
            "Name=Tom & Jerry",
            "Comment=Cat\\nand mouse",
            "Exec=tj",
            "Icon=com.example.tj",
            "Categories=Game;Simulation;",
            "StartupWMClass=com.example.tj",
        ] {
            assert!(entry.lines().any(|l| l == line), "{line}: {entry}");
        }
        // No icon, no `Icon`; no categories, `Utility`; no summary, no comment.
        let bare = desktop_entry(&naming_of("a.b"), &BundleConfig::default(), "tj", false);
        assert!(!bare.contains("Icon="), "{bare}");
        assert!(!bare.contains("Comment="), "{bare}");
        assert!(bare.contains("Categories=Utility;\n"), "{bare}");
    }

    #[test]
    fn an_exec_with_spaces_or_reserved_characters_is_quoted() {
        assert_eq!(exec_argument("aura-weather"), "aura-weather");
        assert_eq!(exec_argument("my app"), "\"my app\"");
        assert_eq!(exec_argument("a$b"), "\"a\\\\$b\"");
        assert_eq!(exec_argument("50%"), "\"50%%\"");
    }
}
