//! Package asset distribution and a registry source (RFC-0008 pillar D, D-H),
//! end to end through the real `byard` binary, with no network.
//!
//! A package ships a font and a theme. The first half is that its font
//! resolves relative to *the package*, and `extends` brings its theme with
//! its typefaces. The second half is `byard publish` into a directory
//! registry and a consumer that depends on the published version: the lock
//! pins the same checksum a path source of the same package gets, and the
//! consumer checks clean against the unpacked copy.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-pkgdist-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Runs `byard` in `cwd` with `HOME` pointed at `home`, so the package cache
/// lives in the scratch directory and never in the real one.
fn byard(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("BYARD_REGISTRY")
        .output()
        .expect("run byard")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A package, `brand`, that ships a variable font and a theme whose type
/// scale uses it. Its font lives under the package; nothing is copied to the
/// consumer.
/// Makes `pkg` a commit and a tag `v0.1.0` of a repository in `work`, with
/// what a package repository carries besides the package (a gallery app, CI
/// files, a font nobody declares), and returns its `file://` URL.
fn git_repo(pkg: &Path, work: &Path) -> String {
    let _ = std::fs::remove_dir_all(work);
    copy_dir(pkg, work);
    write(&work.join("README.md"), "# brand\n");
    write(&work.join("LICENSE"), "MIT\n");
    write(&work.join("gallery/main.byd"), "View Main() { Box {} }\n");
    write(&work.join(".github/workflows/ci.yml"), "name: ci\n");
    write(&work.join("fonts/Unused.ttf"), "not declared");
    write(&work.join("notes.txt"), "not part of the package");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(work)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}: {}", text(&out));
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "brand"]);
    git(&["tag", "v0.1.0"]);
    format!("file://{}", work.display())
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn make_package(root: &Path) {
    let font = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/assets/fonts/SpaceGrotesk-Variable.ttf");
    std::fs::create_dir_all(root.join("fonts")).unwrap();
    std::fs::copy(font, root.join("fonts/Display.ttf")).unwrap();
    write(
        &root.join("byard.toml"),
        r##"[package]
name = "brand"
version = "0.1.0"

[assets.fonts]
Display = "fonts/Display.ttf"

[theme]
name = "brand"

[theme.color.light]
primary = "#123456"

[theme.typography]
headline = { size = 30, family = "Display", weight = "bold" }
"##,
    );
    write(
        &root.join("src/badge.byd"),
        "View Badge() { Text(\"brand\") #[font: \"brand/Display\"] }\n",
    );
}

/// A consumer with no fonts of its own, using the package's font directly,
/// through the extended theme's typography, and through the package's view.
fn make_app(root: &Path, dependency: &str, font: &str) {
    write(
        &root.join("byard.toml"),
        &format!(
            "[project]\nname = \"app\"\nentry = \"main.byd\"\n\n\
             [dependencies]\n{dependency}\n\n[theme]\nextends = \"brand\"\n"
        ),
    );
    write(
        &root.join("main.byd"),
        &format!(
            "use brand as b\n\
             View Main() {{\n\
                 inject Theme as t\n\
                 Column {{\n\
                     Text(\"direct\") #[font: \"{font}\"]\n\
                     Text(\"themed\") #[typo: t.headline, color: t.primary]\n\
                     b.Badge()\n\
                 }}\n\
             }}\n"
        ),
    );
}

#[test]
fn a_package_font_resolves_relative_to_the_package() {
    let dir = scratch("path");
    make_package(&dir.join("brand"));
    make_app(
        &dir.join("app"),
        "brand = { path = \"../brand\" }",
        "brand/Display",
    );
    assert!(
        !dir.join("app/fonts").exists(),
        "the consumer ships no fonts"
    );

    let out = byard(&dir.join("app"), &dir, &["check"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("0 errors"), "{}", text(&out));

    // The control: a family the package does not ship is still an error,
    // so the clean check above is the font resolving and not a check that
    // looks at nothing.
    make_app(
        &dir.join("app"),
        "brand = { path = \"../brand\" }",
        "brand/Missing",
    );
    let out = byard(&dir.join("app"), &dir, &["check"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("UnknownFontFamily"), "{}", text(&out));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn extends_must_name_a_dependency() {
    let dir = scratch("extends");
    make_package(&dir.join("brand"));
    make_app(
        &dir.join("app"),
        "brand = { path = \"../brand\" }",
        "brand/Display",
    );
    let manifest = std::fs::read_to_string(dir.join("app/byard.toml")).unwrap();
    write(
        &dir.join("app/byard.toml"),
        &manifest.replace("extends = \"brand\"", "extends = \"bran\""),
    );
    let out = byard(&dir.join("app"), &dir, &["check"]);
    assert!(!out.status.success());
    assert!(
        text(&out).contains("extends `bran`") && text(&out).contains("declared: brand"),
        "{}",
        text(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_published_package_is_a_registry_source_with_the_same_pin() {
    let dir = scratch("registry");
    let (pkg, registry) = (dir.join("brand"), dir.join("registry"));
    make_package(&pkg);

    let publish = |dir: &Path| {
        byard(
            dir,
            dir,
            &[
                "publish",
                pkg.to_str().unwrap(),
                "--registry",
                registry.to_str().unwrap(),
            ],
        )
    };
    let out = publish(&dir);
    assert!(out.status.success(), "{}", text(&out));
    let archive = registry.join("brand/0.1.0.tar.gz");
    let first = std::fs::read(&archive).expect("the archive is written");
    assert!(registry.join("index.toml").exists());

    // Same content again: a no-op, and the same bytes.
    let out = publish(&dir);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        std::fs::read(&archive).unwrap(),
        first,
        "publishing is deterministic"
    );

    // Different content under the same version: refused.
    write(&pkg.join("src/extra.byd"), "View Extra() { Box {} }\n");
    let out = publish(&dir);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("immutable"), "{}", text(&out));
    std::fs::remove_file(pkg.join("src/extra.byd")).unwrap();

    // The same package as a git source, locked: the reference pin (a path
    // source is not pinned, RFC-0056).
    let git_url = git_repo(&pkg, &dir.join("brand.git-work"));
    make_app(
        &dir.join("by_git"),
        &format!("brand = {{ git = \"{git_url}\", tag = \"v0.1.0\" }}"),
        "brand/Display",
    );
    let out = byard(&dir.join("by_git"), &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));
    let path_lock = std::fs::read_to_string(dir.join("by_git/byard.lock")).unwrap();

    // And as a registry source.
    make_app(
        &dir.join("by_registry"),
        "brand = { registry = \"../registry\", version = \"0.1.0\" }",
        "brand/Display",
    );
    let out = byard(&dir.join("by_registry"), &dir, &["check"]);
    assert!(!out.status.success(), "not fetched yet");
    assert!(text(&out).contains("byard get"), "{}", text(&out));

    let out = byard(&dir.join("by_registry"), &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));
    let reg_lock = std::fs::read_to_string(dir.join("by_registry/byard.lock")).unwrap();
    assert!(reg_lock.contains("source = \"registry+"), "{reg_lock}");
    let checksum = |lock: &str| {
        lock.lines()
            .find(|l| l.starts_with("checksum"))
            .map(str::to_string)
            .unwrap()
    };
    assert_eq!(
        checksum(&reg_lock),
        checksum(&path_lock),
        "a published package pins the same checksum as its git source"
    );

    // The consumer checks clean against the unpacked copy, font included.
    let out = byard(&dir.join("by_registry"), &dir, &["check"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("0 errors"), "{}", text(&out));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The committed example: an app with no fonts or colours of its own, whose
/// design system comes from a local package, checks clean.
#[test]
fn the_package_theme_example_checks_clean() {
    let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/package_theme/app");
    let home = scratch("example");
    let out = byard(&app, &home, &["check"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("0 errors"), "{}", text(&out));
    let _ = std::fs::remove_dir_all(&home);
}

/// A package that fails its own `byard check` is not published: a version is
/// immutable, and nobody can use it.
#[test]
fn publish_refuses_a_package_that_fails_check() {
    let dir = scratch("publish-check");
    let (pkg, registry) = (dir.join("brand"), dir.join("registry"));
    make_package(&pkg);
    write(
        &pkg.join("src/bad.byd"),
        "View Bad() { Box #[bg: prmary] {} }\n",
    );
    let publish = byard(
        &dir,
        &dir,
        &[
            "publish",
            pkg.to_str().unwrap(),
            "--registry",
            registry.to_str().unwrap(),
        ],
    );
    assert!(!publish.status.success(), "{}", text(&publish));
    assert!(
        text(&publish).contains("nothing published"),
        "{}",
        text(&publish)
    );
    assert!(text(&publish).contains("bad.byd"), "{}", text(&publish));
    assert!(!registry.join("brand/0.1.0.tar.gz").exists());
    assert!(!registry.join("index.toml").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A consumer that uses a package's views without extending its theme gets
/// told where the token lives, not only that the theme lacks it.
#[test]
fn a_token_only_a_dependencys_theme_declares_says_so() {
    let dir = scratch("theme-hint");
    let pkg = dir.join("brand");
    make_package(&pkg);
    write(
        &pkg.join("byard.toml"),
        &std::fs::read_to_string(pkg.join("byard.toml"))
            .unwrap()
            .replace(
                "primary = \"#123456\"",
                "primary = \"#123456\"\nbrand_glow = \"#00FF88\"",
            ),
    );
    write(
        &pkg.join("src/badge.byd"),
        "View Badge() {\n    inject Theme as t\n    Box #[bg: t.brandGlow] {}\n}\n",
    );
    let app = dir.join("app");
    make_app(&app, "brand = { path = \"../brand\" }", "brand/Display");
    let manifest = std::fs::read_to_string(app.join("byard.toml")).unwrap();
    // Without the `[theme] extends`, the package's own token is unknown.
    write(
        &app.join("byard.toml"),
        &manifest.replace("[theme]\nextends = \"brand\"\n", ""),
    );
    write(
        &app.join("main.byd"),
        "use brand as b\nView Main() {\n    b.Badge()\n}\n",
    );
    let out = byard(&app, &dir, &["check"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains(
            "`brandGlow` is declared by package `brand`'s theme; add `[theme] extends = \"brand\"`, or declare it yourself"
        ),
        "{}",
        text(&out)
    );
    // Already extending another package: only one `extends` is allowed, so
    // the advice is to declare it or to extend this one instead.
    let other = dir.join("other");
    write(
        &other.join("byard.toml"),
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\n\n[theme]\nname = \"other\"\n",
    );
    write(&other.join("src/lib.byd"), "View Nothing() { Box {} }\n");
    write(
        &app.join("byard.toml"),
        "[project]\nname = \"app\"\nentry = \"main.byd\"\n\n[dependencies]\n\
         brand = { path = \"../brand\" }\nother = { path = \"../other\" }\n\n\
         [theme]\nextends = \"other\"\n",
    );
    let out = byard(&app, &dir, &["check"]);
    assert!(
        text(&out).contains(
            "`brandGlow` is declared by package `brand`'s theme, but this theme extends `other`; declare `brandGlow` yourself, or extend `brand` instead"
        ),
        "{}",
        text(&out)
    );
    write(
        &app.join("byard.toml"),
        &manifest.replace("[theme]\nextends = \"brand\"\n", ""),
    );
    // A token nobody declares keeps the plain message.
    write(
        &pkg.join("src/badge.byd"),
        "View Badge() {\n    inject Theme as t\n    Box #[bg: t.nobodyHasThis] {}\n}\n",
    );
    let out = byard(&app, &dir, &["check"]);
    assert!(
        text(&out).contains("declares no token `nobodyHasThis`"),
        "{}",
        text(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0056: a path dependency is the files on disk, so the lock lists it and
/// does not pin it; editing the package leaves `check` and the lock as they
/// were, and a lock an older `byard` wrote, with a pin, still reads.
#[test]
fn a_path_dependency_is_listed_in_the_lock_but_not_pinned() {
    let dir = scratch("pathlock");
    make_package(&dir.join("brand"));
    make_app(
        &dir.join("app"),
        "brand = { path = \"../brand\" }",
        "brand/Display",
    );
    let app = dir.join("app");
    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));
    let lock = std::fs::read_to_string(app.join("byard.lock")).unwrap();
    assert!(lock.contains("source = \"path+../brand\""), "{lock}");
    assert!(!lock.contains("checksum"), "a path is not pinned:\n{lock}");

    // An edit to the dependency: `check` sees it, and the lock is still right.
    write(
        &dir.join("brand/src/extra.byd"),
        "View Extra() { Text(\"new\") }\n",
    );
    write(
        &app.join("main.byd"),
        &std::fs::read_to_string(app.join("main.byd"))
            .unwrap()
            .replace("b.Badge()", "b.Badge()\n b.Extra()"),
    );
    let out = byard(&app, &dir, &["check"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        std::fs::read_to_string(app.join("byard.lock")).unwrap(),
        lock,
        "check never writes the lock"
    );

    // A lock written before, with the pin of a content since edited.
    std::fs::write(
        app.join("byard.lock"),
        "version = 1\n\n[[package]]\nname = \"brand\"\nsource = \"path+../brand\"\n\
         checksum = \"sha256:00\"\n",
    )
    .unwrap();
    let out = byard(&app, &dir, &["check"]);
    assert!(out.status.success(), "{}", text(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0056: the cached copy of a git dependency is what `publish` archives
/// and nothing else, it has the checksum its registry copy has, and it is
/// reused (with its commit) on the next `get`.
#[test]
fn a_git_dependency_caches_only_what_publish_archives() {
    let dir = scratch("gitcache");
    let (pkg, registry) = (dir.join("brand"), dir.join("registry"));
    make_package(&pkg);
    write(&pkg.join("README.md"), "# brand\n");
    write(&pkg.join("LICENSE"), "MIT\n");
    let url = git_repo(&pkg, &dir.join("brand-git"));
    let out = byard(
        &dir,
        &dir,
        &[
            "publish",
            pkg.to_str().unwrap(),
            "--registry",
            registry.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", text(&out));

    let app = dir.join("app");
    make_app(
        &app,
        &format!("brand = {{ git = \"{url}\", tag = \"v0.1.0\" }}"),
        "brand/Display",
    );
    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));
    let cache = dir.join(".byard/cache");
    let cached: Vec<PathBuf> = std::fs::read_dir(&cache)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("brand-")
        })
        .collect();
    assert_eq!(
        cached.len(),
        1,
        "one checkout, nothing half-made: {cached:?}"
    );
    let mut files = Vec::new();
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    walk(&cached[0], &cached[0], &mut files);
    files.sort();
    assert_eq!(
        files,
        [
            "LICENSE",
            "README.md",
            "byard.toml",
            "fonts/Display.ttf",
            "src/badge.byd"
        ],
        "exactly what `byard publish` archives"
    );
    let lock = std::fs::read_to_string(app.join("byard.lock")).unwrap();
    assert!(lock.contains("commit = \""), "{lock}");

    // The same content has the same checksum from a registry.
    make_app(
        &dir.join("by_registry"),
        "brand = { registry = \"../registry\", version = \"0.1.0\" }",
        "brand/Display",
    );
    let out = byard(&dir.join("by_registry"), &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));
    let pin = |lock: &str| {
        lock.lines()
            .find(|l| l.starts_with("checksum"))
            .map(str::to_string)
    };
    let reg_lock = std::fs::read_to_string(dir.join("by_registry/byard.lock")).unwrap();
    assert!(pin(&lock).is_some());
    assert_eq!(pin(&lock), pin(&reg_lock));

    // Cached: the next get fetches nothing and keeps the commit; check builds.
    let again = byard(&app, &dir, &["get"]);
    assert!(again.status.success(), "{}", text(&again));
    assert!(!text(&again).contains("fetching"), "{}", text(&again));
    assert_eq!(
        std::fs::read_to_string(app.join("byard.lock")).unwrap(),
        lock
    );
    let out = byard(&app, &dir, &["check"]);
    assert!(out.status.success(), "{}", text(&out));
    let _ = std::fs::remove_dir_all(&dir);
}
