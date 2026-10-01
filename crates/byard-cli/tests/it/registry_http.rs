//! A registry over HTTP, with version requirements (RFC-0008 D-K), end to end
//! through the real `byard` binary against a loopback server, never the
//! network.
//!
//! `byard publish` writes versions into a directory; a small server on
//! 127.0.0.1 serves that directory, which is all an HTTP registry is. The
//! server counts what it is asked, and can be told to fail every request, so
//! "the lock is honoured without contacting the server" is a measurement,
//! not an assumption.

use std::io::{BufRead, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-reghttp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Runs `byard` in `cwd` with `HOME` at `home`, so the cache is the test's.
fn byard(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_byard"))
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

/// A static file server over `root` on a free loopback port.
struct Server {
    url: String,
    requests: Arc<AtomicUsize>,
    failing: Arc<AtomicBool>,
}

impl Server {
    fn serve(root: PathBuf) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let failing = Arc::new(AtomicBool::new(false));
        let (count, fail) = (Arc::clone(&requests), Arc::clone(&failing));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                // Drain the headers; nothing in them matters here.
                let mut header = String::new();
                while reader.read_line(&mut header).is_ok_and(|n| n > 2) {
                    header.clear();
                }
                count.fetch_add(1, Ordering::SeqCst);
                let path = line.split_whitespace().nth(1).unwrap_or("/");
                let file = root.join(path.trim_start_matches('/'));
                let (status, body) = if fail.load(Ordering::SeqCst) {
                    ("500 Internal Server Error", Vec::new())
                } else {
                    match std::fs::read(&file) {
                        Ok(bytes) => ("200 OK", bytes),
                        Err(_) => ("404 Not Found", Vec::new()),
                    }
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        Self {
            url,
            requests,
            failing,
        }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// Publishes `name@version` into `registry`, with a view `name.Tag` that says
/// its own version, and `deps` as its `[dependencies]` table body.
fn publish(dir: &Path, registry: &Path, name: &str, version: &str, deps: &str) {
    let pkg = dir.join(format!("src-{name}-{version}"));
    write(
        &pkg.join("byard.toml"),
        &format!(
            "[package]\nname = \"{name}\"\nversion = \"{version}\"\n\n[dependencies]\n{deps}\n"
        ),
    );
    let view = name[..1].to_uppercase() + &name[1..];
    write(
        &pkg.join("src/tag.byd"),
        &format!("View {view}Tag() {{ Text(\"{name} {version}\") }}\n"),
    );
    let out = byard(
        dir,
        dir,
        &[
            "publish",
            pkg.to_str().unwrap(),
            "--registry",
            registry.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "publish {name}@{version}: {}",
        text(&out)
    );
}

/// An app depending on `deps` (the `[dependencies]` body) and using `name`'s view.
fn make_app(root: &Path, deps: &str, uses: &str) {
    write(
        &root.join("byard.toml"),
        &format!("[project]\nname = \"app\"\nentry = \"main.byd\"\n\n[dependencies]\n{deps}\n"),
    );
    let view = uses[..1].to_uppercase() + &uses[1..];
    write(
        &root.join("main.byd"),
        &format!("use {uses} as p\nView Main() {{ Column {{ p.{view}Tag() }} }}\n"),
    );
}

fn locked_version(app: &Path, name: &str) -> String {
    let lock = std::fs::read_to_string(app.join("byard.lock")).unwrap();
    let block = lock
        .split("[[package]]")
        .find(|b| b.contains(&format!("name = \"{name}\"")))
        .unwrap_or_else(|| panic!("`{name}` is locked:\n{lock}"));
    block
        .lines()
        .find_map(|l| l.strip_prefix("version = "))
        .map_or_else(
            || panic!("`{name}` has a version:\n{lock}"),
            |v| v.trim_matches('"').to_string(),
        )
}

#[test]
fn a_range_resolves_to_the_highest_matching_version() {
    let dir = scratch("range");
    let registry = dir.join("registry");
    for v in ["0.1.0", "0.2.0", "0.2.3", "0.3.0"] {
        publish(&dir, &registry, "weather", v, "");
    }
    let server = Server::serve(registry);
    let app = dir.join("app");
    make_app(
        &app,
        &format!(
            "weather = {{ registry = \"{}\", version = \"^0.2\" }}",
            server.url
        ),
        "weather",
    );

    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        locked_version(&app, "weather"),
        "0.2.3",
        "the highest 0.2.x, not 0.3.0"
    );
    assert!(server.requests() > 0, "the index came from the server");

    // `check` builds from the lock and the cache, and finds the package.
    let out = byard(&app, &dir, &["check"]);
    assert!(out.status.success(), "{}", text(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_existing_lock_is_honoured_without_contacting_the_server() {
    let dir = scratch("honoured");
    let registry = dir.join("registry");
    publish(&dir, &registry, "weather", "0.2.0", "");
    let server = Server::serve(registry.clone());
    let app = dir.join("app");
    make_app(
        &app,
        &format!(
            "weather = {{ registry = \"{}\", version = \"0.2\" }}",
            server.url
        ),
        "weather",
    );
    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));
    let lock = std::fs::read_to_string(app.join("byard.lock")).unwrap();

    // A newer matching version appears, and the server starts failing every
    // request: a lock that still satisfies byard.toml is kept, and nothing
    // is asked of the server at all.
    publish(&dir, &registry, "weather", "0.2.9", "");
    server.failing.store(true, Ordering::SeqCst);
    let before = server.requests();
    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(server.requests(), before, "no request reached the server");
    assert_eq!(
        std::fs::read_to_string(app.join("byard.lock")).unwrap(),
        lock
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two requirements on one package that no version meets together: the
/// failure is `PubGrub`'s explanation, naming both.
#[test]
fn an_unsatisfiable_set_explains_both_constraints() {
    let dir = scratch("conflict");
    let registry = dir.join("registry");
    publish(&dir, &registry, "units", "0.2.0", "");
    publish(&dir, &registry, "units", "0.3.0", "");
    // `forecast` names `units` in the registry it is published to, by the
    // same directory, which the index records as "this registry".
    publish(
        &dir,
        &registry,
        "forecast",
        "1.0.0",
        "units = { registry = \"../registry\", version = \"^0.3\" }",
    );
    let server = Server::serve(registry);
    let app = dir.join("app");
    make_app(
        &app,
        &format!(
            "units = {{ registry = \"{0}\", version = \"^0.2\" }}\n\
             forecast = {{ registry = \"{0}\", version = \"1\" }}",
            server.url
        ),
        "forecast",
    );

    let out = byard(&app, &dir, &["get"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("no set of versions"), "{said}");
    assert!(
        said.contains("forecast") && said.contains("units"),
        "both packages are named: {said}"
    );
    // The two incompatible ranges, as the solver writes them.
    assert!(said.contains(">=0.2.0, <0.3.0"), "the app's range: {said}");
    assert!(said.contains(">=0.3.0, <0.4.0"), "forecast's range: {said}");
    assert!(
        !app.join("byard.lock").exists(),
        "a failed solve writes no lock"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tampered_archive_fails_its_checksum() {
    let dir = scratch("tampered");
    let registry = dir.join("registry");
    publish(&dir, &registry, "weather", "0.2.0", "");
    // Someone swaps the archive on the server after publishing.
    std::fs::write(
        registry.join("weather/0.2.0.tar.gz"),
        b"not the published bytes",
    )
    .unwrap();
    let server = Server::serve(registry);
    let app = dir.join("app");
    make_app(
        &app,
        &format!(
            "weather = {{ registry = \"{}\", version = \"0.2\" }}",
            server.url
        ),
        "weather",
    );
    let out = byard(&app, &dir, &["get"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("does not match the index"), "{said}");
    assert!(!app.join("byard.lock").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A version requirement the manifest cannot mean is refused when it is read.
#[test]
fn a_malformed_requirement_is_a_manifest_error() {
    let dir = scratch("malformed");
    let app = dir.join("app");
    make_app(
        &app,
        "weather = { registry = \"http://127.0.0.1:9\", version = \"about two\" }",
        "weather",
    );
    let out = byard(&app, &dir, &["check"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("not a version requirement"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Adding a dependency solves again, and the solve keeps every version the
/// lock already chose that still fits: one new package does not quietly
/// upgrade the others.
#[test]
fn adding_a_dependency_keeps_the_others_where_they_were() {
    let dir = scratch("keeps");
    let registry = dir.join("registry");
    publish(&dir, &registry, "weather", "0.2.0", "");
    publish(&dir, &registry, "units", "1.0.0", "");
    let server = Server::serve(registry.clone());
    let app = dir.join("app");
    let weather = format!(
        "weather = {{ registry = \"{}\", version = \"0.2\" }}",
        server.url
    );
    make_app(&app, &weather, "weather");
    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));

    publish(&dir, &registry, "weather", "0.2.9", "");
    let units = format!(
        "units = {{ registry = \"{}\", version = \"1\" }}",
        server.url
    );
    make_app(&app, &format!("{weather}\n{units}"), "weather");
    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(locked_version(&app, "units"), "1.0.0");
    assert_eq!(
        locked_version(&app, "weather"),
        "0.2.0",
        "kept, not upgraded to 0.2.9"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A published package can depend only on registry packages: a path on the
/// publisher's disk is nowhere for whoever installs it.
#[test]
fn a_path_dependency_cannot_be_published() {
    let dir = scratch("pathdep");
    let pkg = dir.join("pkg");
    write(
        &pkg.join("byard.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n\n[dependencies]\nlocal = { path = \"../local\" }\n",
    );
    write(&pkg.join("src/a.byd"), "View A() { Text(\"a\") }\n");
    let out = byard(
        &dir,
        &dir,
        &[
            "publish",
            pkg.to_str().unwrap(),
            "--registry",
            dir.join("registry").to_str().unwrap(),
        ],
    );
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("only on registry packages"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A requirement no published version meets says so, and lists what there is.
#[test]
fn a_requirement_nothing_meets_lists_what_is_published() {
    let dir = scratch("unmet");
    let registry = dir.join("registry");
    publish(&dir, &registry, "weather", "0.1.0", "");
    publish(&dir, &registry, "weather", "0.1.4", "");
    let server = Server::serve(registry);
    let app = dir.join("app");
    make_app(
        &app,
        &format!(
            "weather = {{ registry = \"{}\", version = \"0.2\" }}",
            server.url
        ),
        "weather",
    );
    let out = byard(&app, &dir, &["get"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(
        said.contains("no version of weather in >=0.2.0, <0.3.0"),
        "the reason: {said}"
    );
    assert!(
        said.contains("weather: published 0.1.0, 0.1.4"),
        "the choices: {said}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The committed example, driven by the test server instead of the one its
/// comment starts by hand: the real `brand` package published, served, and
/// resolved by the example's own requirement.
#[test]
fn the_package_theme_http_example_resolves_and_checks() {
    let dir = scratch("example");
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/package_theme");
    let registry = dir.join("registry");
    let out = byard(
        &dir,
        &dir,
        &[
            "publish",
            examples.join("brand").to_str().unwrap(),
            "--registry",
            registry.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let server = Server::serve(registry);

    let app = dir.join("app_http");
    for file in ["byard.toml", "main.byd"] {
        let src = std::fs::read_to_string(examples.join("app_http").join(file)).unwrap();
        write(
            &app.join(file),
            &src.replace("http://127.0.0.1:8765", &server.url),
        );
    }
    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(locked_version(&app, "brand"), "0.1.0");
    let out = byard(&app, &dir, &["check"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("0 errors"), "{}", text(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `byard add --registry` without a version writes `^` the newest version
/// published, as `cargo add` does, and locks it.
#[test]
fn add_from_a_registry_requires_the_newest_compatible_release() {
    let dir = scratch("add");
    let registry = dir.join("registry");
    for v in ["0.1.0", "0.2.0", "0.2.3"] {
        publish(&dir, &registry, "weather", v, "");
    }
    let server = Server::serve(registry);
    let app = dir.join("app");
    make_app(&app, "", "weather");
    let out = byard(&app, &dir, &["add", "weather", "--registry", &server.url]);
    assert!(out.status.success(), "{}", text(&out));
    let manifest = std::fs::read_to_string(app.join("byard.toml")).unwrap();
    assert!(
        manifest.contains(&format!(
            "weather = {{ registry = \"{}\", version = \"^0.2.3\" }}",
            server.url
        )),
        "{manifest}"
    );
    assert_eq!(locked_version(&app, "weather"), "0.2.3");
    let out = byard(&app, &dir, &["check"]);
    assert!(out.status.success(), "{}", text(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The one directory under the test's registry cache, its single package.
fn cached_package(home: &Path) -> PathBuf {
    let dir = home.join(".byard/cache/registry");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    entries.pop().unwrap()
}

/// A cached package whose content was edited is not trusted: the solve
/// fetches it again rather than locking what `check` would then reject.
#[test]
fn a_tampered_cache_is_fetched_again() {
    let dir = scratch("cache");
    let registry = dir.join("registry");
    publish(&dir, &registry, "weather", "0.2.0", "");
    let server = Server::serve(registry);
    let app = dir.join("app");
    let dep = format!(
        "weather = {{ registry = \"{}\", version = \"0.2\" }}",
        server.url
    );
    make_app(&app, &dep, "weather");
    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));

    let cached = cached_package(&dir);
    write(
        &cached.join("src/tag.byd"),
        "View WeatherTag() { Text(\"edited\") }\n",
    );
    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));
    let restored = std::fs::read_to_string(cached.join("src/tag.byd")).unwrap();
    assert!(
        restored.contains("weather 0.2.0"),
        "fetched again: {restored}"
    );
    let out = byard(&app, &dir, &["check"]);
    assert!(out.status.success(), "{}", text(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The first publish into a registry that does not exist yet still records
/// a dependency on that registry as "this registry", and a dependency on a
/// directory that exists nowhere is refused rather than taken for it.
#[test]
fn a_first_publish_tells_this_registry_from_a_missing_one() {
    let dir = scratch("firstpub");
    let registry = dir.join("fresh-registry");
    publish(
        &dir,
        &registry,
        "forecast",
        "1.0.0",
        "units = { registry = \"../fresh-registry\", version = \"^0.3\" }",
    );
    let index = std::fs::read_to_string(registry.join("index.toml")).unwrap();
    assert!(
        index.contains("dependencies = [{ name = \"units\", version = \"^0.3\" }]"),
        "recorded as this registry: {index}"
    );

    let pkg = dir.join("other");
    write(
        &pkg.join("byard.toml"),
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\n\n[dependencies]\n\
         units = { registry = \"../nowhere\", version = \"1\" }\n",
    );
    write(&pkg.join("src/a.byd"), "View OtherTag() { Text(\"o\") }\n");
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
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("not the one being published to"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A path package with a `[package] version` and a manifest around it.
fn path_package(root: &Path, name: &str, version: &str, deps: &str) {
    write(
        &root.join("byard.toml"),
        &format!(
            "[package]\nname = \"{name}\"\nversion = \"{version}\"\n\n[dependencies]\n{deps}\n"
        ),
    );
    let view = name[..1].to_uppercase() + &name[1..];
    write(
        &root.join("src/tag.byd"),
        &format!("View {view}Tag() {{ Text(\"{name}\") }}\n"),
    );
}

/// With a lock in place, changing a path package to require a version of
/// another path package that it does not have is still the conflict a full
/// solve reports: the lock is not a way around a requirement.
#[test]
fn a_requirement_on_a_path_package_is_checked_even_with_a_lock() {
    let dir = scratch("fixedreq");
    path_package(&dir.join("kit"), "kit", "0.1.0", "");
    let widgets = dir.join("widgets");
    let on_kit =
        |req: &str| format!("kit = {{ registry = \"http://127.0.0.1:9\", version = \"{req}\" }}");
    path_package(&widgets, "widgets", "0.1.0", &on_kit("^0.1"));
    let app = dir.join("app");
    make_app(
        &app,
        "kit = { path = \"../kit\" }\nwidgets = { path = \"../widgets\" }",
        "widgets",
    );
    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "kit 0.1.0 meets ^0.1: {}", text(&out));

    path_package(&widgets, "widgets", "0.1.0", &on_kit("^0.2"));
    let out = byard(&app, &dir, &["get"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("kit"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A second declaration of a locked name, from a different registry, is
/// checked even though the name is already chosen: two sources for one name
/// is an error, lock or not. The lock comes from a path package's
/// declaration, and the app's own, from elsewhere, is the one met second.
#[test]
fn a_second_source_for_a_locked_name_is_not_waved_through() {
    let dir = scratch("twosources");
    let (reg_a, reg_b) = (dir.join("reg-a"), dir.join("reg-b"));
    publish(&dir, &reg_a, "units", "1.0.0", "");
    publish(&dir, &reg_b, "units", "1.0.0", "");
    let (a, b) = (Server::serve(reg_a), Server::serve(reg_b));
    path_package(
        &dir.join("widgets"),
        "widgets",
        "0.1.0",
        &format!("units = {{ registry = \"{}\", version = \"1\" }}", a.url),
    );
    let app = dir.join("app");
    let widgets = "widgets = { path = \"../widgets\" }";
    make_app(&app, widgets, "widgets");
    let out = byard(&app, &dir, &["get"]);
    assert!(out.status.success(), "{}", text(&out));

    let from_b = format!("units = {{ registry = \"{}\", version = \"1\" }}", b.url);
    make_app(&app, &format!("{widgets}\n{from_b}"), "widgets");
    let out = byard(&app, &dir, &["get"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("two registries"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}
