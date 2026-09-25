//! Dependency resolution, the global cache, and `byard.lock`
//! (RFC-0008 Pillar C, decisions D-H/D-I).
//!
//! The compiler's module resolver is pure, it asks a
//! [`PackageProvider`] for package sources and never touches the filesystem
//! policy itself. This module is that policy: manifest `[dependencies]` →
//! concrete directories (local `path` deps, or git checkouts pinned by the
//! lockfile in `~/.byard/cache`), plus the content-hashed lockfile that makes
//! builds reproducible.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Lowercase hex encoding of `bytes` (avoids a `format!`-per-byte allocation).
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

use byard_compiler::resolve::{PackageProvider, ROOT_PACKAGE, SourceFile};
use sha2::{Digest, Sha256};

use crate::manifest::{DepSource, Dependency, GitRef, Manifest, parse_dependencies};

// ── Cache ─────────────────────────────────────────────────────────────────────

/// The global user cache for fetched (immutable) packages: `~/.byard/cache`.
pub fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".byard").join("cache")
}

/// The cache directory for one pinned git source. The key hashes the URL and
/// the exact ref, so two pins of the same repo never collide.
#[must_use]
pub fn git_cache_path(name: &str, url: &str, reference: &GitRef) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    hasher.update([0]);
    hasher.update(reference.as_str().as_bytes());
    let digest = hasher.finalize();
    let short = hex_encode(&digest[..6]);
    cache_dir().join(format!("{name}-{short}"))
}

// ── Content hashing (D-I) ─────────────────────────────────────────────────────

/// Content-hashes a package directory: every `.byd` file plus `byard.toml`,
/// in sorted relative-path order (path, NUL, length, bytes). Deterministic
/// across machines, the lockfile pin.
///
/// A package that declares assets (`[assets.fonts]`, RFC-0008 pillar D) has
/// those files hashed too, under their declared paths, so a swapped font is a
/// checksum mismatch like a swapped source file. A package that declares none
/// hashes exactly as it did before assets existed, which is what keeps every
/// lockfile written until then valid.
pub fn package_checksum(root: &Path) -> Result<String, String> {
    let mut files = collect_byd_files(root)?;
    let manifest = root.join("byard.toml");
    if manifest.exists() {
        files.push(("byard.toml".to_string(), manifest));
    }
    for (rel, path) in declared_assets(root)? {
        if !files.iter().any(|(r, _)| *r == rel) {
            files.push((rel, path));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    for (rel, path) in files {
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    let digest = hasher.finalize();
    Ok(format!("sha256:{}", hex_encode(&digest)))
}

/// The asset files a package's manifest declares, as `(normalised relative
/// path, absolute path)`. Only fonts are declared by path today.
pub fn declared_assets(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let manifest = root.join("byard.toml");
    if !manifest.exists() {
        return Ok(Vec::new());
    }
    let src =
        std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let table: toml::Table = src
        .parse()
        .map_err(|e: toml::de::Error| format!("{}: {e}", manifest.display()))?;
    let Some(fonts) = table
        .get("assets")
        .and_then(|a| a.get("fonts"))
        .and_then(toml::Value::as_table)
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (family, path) in fonts {
        let Some(path) = path.as_str() else {
            return Err(format!(
                "{}: [assets.fonts] `{family}` must be a string path",
                manifest.display()
            ));
        };
        if !crate::manifest::is_inside_package(path) {
            return Err(format!(
                "{}: [assets.fonts] `{family}` = {path:?} is outside the package",
                manifest.display()
            ));
        }
        let rel = path.trim_start_matches("./").replace('\\', "/");
        out.push((rel, root.join(path)));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    Ok(out)
}

/// Every file a published package carries, as `(relative path, absolute
/// path)`: its manifest, its sources, its declared assets, and a README and
/// licence when present. Exactly what [`package_checksum`] covers, plus the
/// documentation, which is shipped but not pinned.
pub fn publishable_files(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut files = collect_byd_files(root)?;
    files.push(("byard.toml".to_string(), root.join("byard.toml")));
    files.extend(declared_assets(root)?);
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|e| format!("{}: {e}", root.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let upper = name.to_ascii_uppercase();
        if path.is_file() && (upper.starts_with("README") || upper.starts_with("LICENSE")) {
            files.push((name, path));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files.dedup_by(|a, b| a.0 == b.0);
    for (rel, path) in &files {
        if !path.is_file() {
            return Err(format!(
                "`{rel}` is declared but `{}` does not exist",
                path.display()
            ));
        }
    }
    Ok(files)
}

/// Collects `(relative path, absolute path)` for every `.byd` under `root`
/// (preferring `root/src/` when it exists), sorted for determinism. Hidden
/// directories, `target/`, and `.byard/` are skipped.
fn collect_byd_files(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let scan_root = if root.join("src").is_dir() {
        root.join("src")
    } else {
        root.to_path_buf()
    };
    let mut out = Vec::new();
    walk(&scan_root, &mut out)?;
    // Relative paths are computed against `root` for display/hashing.
    let mut with_rel: Vec<(String, PathBuf)> = out
        .into_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            (rel, p)
        })
        .collect();
    with_rel.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(with_rel)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Deterministic traversal: sorted so checksums and file order never
    // depend on readdir order.
    let mut paths: Vec<PathBuf> = entries
        .map(|e| e.map(|e| e.path()).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            if name.starts_with('.') || name == "target" {
                continue;
            }
            walk(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "byd") {
            out.push(path);
        }
    }
    Ok(())
}

// ── Lockfile (D-I) ────────────────────────────────────────────────────────────

/// One pinned package in `byard.lock`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockedPackage {
    /// The package name (its `use` name).
    pub name: String,
    /// `path+<rel>` or `git+<url>#<ref>`.
    pub source: String,
    /// The exact commit a git ref resolved to (empty for path sources).
    pub commit: String,
    /// `sha256:<hex>` over the package contents.
    pub checksum: String,
}

/// The parsed `byard.lock`. Written only by `byard get` (D-I); builds resolve
/// *from* it, never from manifest ranges.
#[derive(Clone, Debug, Default)]
pub struct Lockfile {
    /// Every locked package, one entry per resolved dependency.
    pub packages: Vec<LockedPackage>,
}

impl Lockfile {
    /// Reads `byard.lock` from `project_root`, if present.
    pub fn read(project_root: &Path) -> Result<Option<Self>, String> {
        let path = project_root.join("byard.lock");
        if !path.exists() {
            return Ok(None);
        }
        let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let table: toml::Table = src
            .parse()
            .map_err(|e: toml::de::Error| format!("byard.lock: {e}"))?;
        let mut packages = Vec::new();
        if let Some(toml::Value::Array(entries)) = table.get("package") {
            for entry in entries {
                let get = |key: &str| {
                    entry
                        .get(key)
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .ok_or_else(|| format!("byard.lock: package entry missing `{key}`"))
                };
                packages.push(LockedPackage {
                    name: get("name")?,
                    source: get("source")?,
                    commit: entry
                        .get("commit")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    checksum: get("checksum")?,
                });
            }
        }
        Ok(Some(Self { packages }))
    }

    /// Writes `byard.lock` under `project_root`, sorted by package name.
    pub fn write(&self, project_root: &Path) -> Result<(), String> {
        let mut packages = self.packages.clone();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out = String::from(
            "# Generated by `byard get`, do not edit by hand (RFC-0008 D-I).\n\
             # Commit this file: it pins every dependency to exact content.\n\
             version = 1\n",
        );
        for p in &packages {
            let _ = writeln!(
                out,
                "\n[[package]]\nname = {:?}\nsource = {:?}",
                p.name, p.source
            );
            if !p.commit.is_empty() {
                let _ = writeln!(out, "commit = {:?}", p.commit);
            }
            let _ = writeln!(out, "checksum = {:?}", p.checksum);
        }
        let path = project_root.join("byard.lock");
        std::fs::write(&path, out).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The locked entry for `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&LockedPackage> {
        self.packages.iter().find(|p| p.name == name)
    }
}

/// The lockfile source string of a dependency.
#[must_use]
pub fn source_string(dep: &Dependency) -> String {
    match &dep.source {
        DepSource::Path(p) => format!("path+{}", p.display()),
        DepSource::Git { url, reference } => match reference {
            GitRef::Rev(r) => format!("git+{url}#rev={r}"),
            GitRef::Tag(t) => format!("git+{url}#tag={t}"),
        },
        DepSource::Registry { registry, version } => {
            format!("registry+{}#{}@{version}", registry.display(), dep.name)
        }
    }
}

// ── Dependency root resolution ────────────────────────────────────────────────

/// Resolves one dependency to its package root directory. `declarer_root` is
/// the directory of the manifest that declared it (path deps are relative to
/// their declarer, exactly like Cargo).
pub fn dep_root(declarer_root: &Path, dep: &Dependency) -> Result<PathBuf, String> {
    match &dep.source {
        DepSource::Path(rel) => {
            let root = declarer_root.join(rel);
            let root = root
                .canonicalize()
                .map_err(|e| format!("dependency `{}`: {}: {e}", dep.name, root.display()))?;
            if !root.is_dir() {
                return Err(format!(
                    "dependency `{}`: `{}` is not a directory",
                    dep.name,
                    root.display()
                ));
            }
            Ok(root)
        }
        DepSource::Registry { registry, version } => {
            let root = registry_cache_path(&dep.name, &declarer_root.join(registry), version);
            if root.is_dir() {
                Ok(root)
            } else {
                Err(format!(
                    "dependency `{}` ({}@{version} from `{}`) is not in the cache yet\n\
                     hint: run `byard get` to fetch dependencies",
                    dep.name,
                    dep.name,
                    registry.display()
                ))
            }
        }
        DepSource::Git { url, reference } => {
            let root = git_cache_path(&dep.name, url, reference);
            if root.is_dir() {
                Ok(root)
            } else {
                Err(format!(
                    "dependency `{}` ({url}#{}) is not in the cache yet\n\
                     hint: run `byard get` to fetch dependencies",
                    dep.name,
                    reference.as_str()
                ))
            }
        }
    }
}

// ── Git acquisition (D-H) ─────────────────────────────────────────────────────

/// Fetches a pinned git source into `dest`, returning the exact commit it
/// resolved to. Shells out to the system `git`, the same trust boundary as
/// Cargo's git dependencies.
pub fn fetch_git(url: &str, reference: &GitRef, dest: &Path) -> Result<String, String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let run = |args: &[&str], cwd: Option<&Path>| -> Result<String, String> {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        let out = cmd
            .output()
            .map_err(|e| format!("failed to run git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git {} failed:\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };

    match reference {
        GitRef::Tag(tag) => {
            run(
                &[
                    "clone",
                    "--depth",
                    "1",
                    "--branch",
                    tag,
                    url,
                    &dest.to_string_lossy(),
                ],
                None,
            )?;
        }
        GitRef::Rev(rev) => {
            run(&["clone", url, &dest.to_string_lossy()], None)?;
            run(&["checkout", "--detach", rev], Some(dest))?;
        }
    }
    run(&["rev-parse", "HEAD"], Some(dest))
}

// ── Registry (D-H) ────────────────────────────────────────────────────────────

/// The cache directory for one published package version. Keyed by the
/// registry's location too, so the same name and version from two
/// registries never share a directory.
#[must_use]
pub fn registry_cache_path(name: &str, registry: &Path, version: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(registry.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    cache_dir()
        .join("registry")
        .join(format!("{name}-{version}-{}", hex_encode(&digest[..6])))
}

/// One published version in a registry's `index.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    /// The package's `[package] name`.
    pub name: String,
    /// The exact version published.
    pub version: String,
    /// [`package_checksum`] of the package: the value a lockfile pins, the
    /// same whether the package arrived from git, a path or here.
    pub checksum: String,
    /// The archive, relative to the registry root.
    pub archive: String,
    /// `sha256:` of the archive bytes, checked before unpacking.
    pub archive_sha256: String,
}

/// A registry's `index.toml`: every published version.
#[derive(Clone, Debug, Default)]
pub struct RegistryIndex {
    /// Every published version, one entry each.
    pub entries: Vec<IndexEntry>,
}

impl RegistryIndex {
    /// Reads `index.toml` under `registry`; an absent file is an empty index.
    pub fn read(registry: &Path) -> Result<Self, String> {
        let path = registry.join("index.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let table: toml::Table = src
            .parse()
            .map_err(|e: toml::de::Error| format!("{}: {e}", path.display()))?;
        let mut entries = Vec::new();
        if let Some(toml::Value::Array(items)) = table.get("package") {
            for item in items {
                let get = |key: &str| {
                    item.get(key)
                        .and_then(toml::Value::as_str)
                        .map(str::to_string)
                        .ok_or_else(|| format!("{}: an entry is missing `{key}`", path.display()))
                };
                entries.push(IndexEntry {
                    name: get("name")?,
                    version: get("version")?,
                    checksum: get("checksum")?,
                    archive: get("archive")?,
                    archive_sha256: get("archive_sha256")?,
                });
            }
        }
        Ok(Self { entries })
    }

    /// Writes `index.toml` under `registry`, sorted by name then version.
    pub fn write(&self, registry: &Path) -> Result<(), String> {
        let mut entries = self.entries.clone();
        entries.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
        let mut out = String::from(
            "# A Byard package registry (RFC-0008). Written by `byard publish`.\n\
             version = 1\n",
        );
        for e in &entries {
            let _ = writeln!(
                out,
                "\n[[package]]\nname = {:?}\nversion = {:?}\nchecksum = {:?}\narchive = {:?}\narchive_sha256 = {:?}",
                e.name, e.version, e.checksum, e.archive, e.archive_sha256
            );
        }
        let path = registry.join("index.toml");
        std::fs::write(&path, out).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The entry for `name` at exactly `version`.
    #[must_use]
    pub fn get(&self, name: &str, version: &str) -> Option<&IndexEntry> {
        self.entries
            .iter()
            .find(|e| e.name == name && e.version == version)
    }
}

/// `sha256:<hex>` of some bytes.
#[must_use]
pub fn sha256_tag(bytes: &[u8]) -> String {
    format!("sha256:{}", hex_encode(&Sha256::digest(bytes)))
}

/// Fetches a published version into the cache and returns its root,
/// verifying the archive's hash before unpacking and the package's checksum
/// after, both against the registry's index. Already cached is not refetched:
/// a published version is immutable.
pub fn fetch_registry(name: &str, registry: &Path, version: &str) -> Result<PathBuf, String> {
    let dest = registry_cache_path(name, registry, version);
    if dest.is_dir() {
        return Ok(dest);
    }
    let index = RegistryIndex::read(registry)?;
    let entry = index.get(name, version).ok_or_else(|| {
        let known: Vec<&str> = index
            .entries
            .iter()
            .filter(|e| e.name == name)
            .map(|e| e.version.as_str())
            .collect();
        format!(
            "`{name}@{version}` is not in the registry at `{}`{}",
            registry.display(),
            if known.is_empty() {
                String::new()
            } else {
                format!(" (published: {})", known.join(", "))
            }
        )
    })?;
    let archive_path = registry.join(&entry.archive);
    let bytes =
        std::fs::read(&archive_path).map_err(|e| format!("{}: {e}", archive_path.display()))?;
    let actual = sha256_tag(&bytes);
    if actual != entry.archive_sha256 {
        return Err(format!(
            "`{name}@{version}`: the archive does not match the index\nindexed: {}\nfound:   {actual}",
            entry.archive_sha256
        ));
    }
    // Unpacked beside the destination and renamed into place, so a failure
    // part way through never leaves a directory that looks fetched.
    let staging = dest.with_extension("partial");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    crate::archive::unpack(&bytes, &staging).map_err(|e| format!("`{name}@{version}`: {e}"))?;
    let unpacked = package_checksum(&staging)?;
    if unpacked != entry.checksum {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "`{name}@{version}`: the unpacked package does not match the index\nindexed: {}\nfound:   {unpacked}",
            entry.checksum
        ));
    }
    std::fs::rename(&staging, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    Ok(dest)
}

// ── The filesystem PackageProvider ────────────────────────────────────────────

/// Resolves package names to on-disk sources for the module resolver:
/// the root project's `[dependencies]` first, then each package's own
/// manifest for transitive deps. Roots are memoized so a diamond dependency
/// loads once.
pub struct FsProvider {
    /// Package name → resolved root directory.
    roots: BTreeMap<String, PathBuf>,
    /// Root project directory (declarer of the root `[dependencies]`).
    project_root: PathBuf,
    /// The root project's declared dependencies.
    project_deps: Vec<Dependency>,
    /// The lockfile, when present, used to verify fetched content (D-I).
    lock: Option<Lockfile>,
}

impl FsProvider {
    /// A provider for `manifest`'s dependencies, verifying fetched content
    /// against `lock` when there is one.
    #[must_use]
    pub fn new(manifest: &Manifest, lock: Option<Lockfile>) -> Self {
        Self {
            roots: BTreeMap::new(),
            project_root: manifest.project_root.clone(),
            project_deps: manifest.dependencies.clone(),
            lock,
        }
    }

    /// The resolved root directories seen so far (for the dev watcher: `path`
    /// deps are watched, cache checkouts are not, D-J).
    #[must_use]
    pub fn resolved_roots(&self) -> &BTreeMap<String, PathBuf> {
        &self.roots
    }

    /// The `[dependencies]` visible to `dependent`.
    fn deps_of(&self, dependent: &str) -> Result<(PathBuf, Vec<Dependency>), String> {
        if dependent == ROOT_PACKAGE {
            return Ok((self.project_root.clone(), self.project_deps.clone()));
        }
        let root = self
            .roots
            .get(dependent)
            .cloned()
            .ok_or_else(|| format!("internal: package `{dependent}` not yet resolved"))?;
        let manifest_path = root.join("byard.toml");
        if !manifest_path.exists() {
            return Ok((root, Vec::new()));
        }
        let src = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let table: toml::Table = src
            .parse()
            .map_err(|e: toml::de::Error| format!("{}: {e}", manifest_path.display()))?;
        let deps = match table.get("dependencies") {
            Some(d) => parse_dependencies(d)?,
            None => Vec::new(),
        };
        Ok((root, deps))
    }
}

impl PackageProvider for FsProvider {
    fn package_files(&mut self, dependent: &str, package: &str) -> Result<Vec<SourceFile>, String> {
        let (declarer_root, deps) = self.deps_of(dependent)?;
        let dep = deps.iter().find(|d| d.name == package).ok_or_else(|| {
            format!(
                "not declared in `[dependencies]` of {}\n\
                 hint: run `byard add {package} --path <dir>` or `--git <url> --tag <tag>`",
                if dependent == ROOT_PACKAGE {
                    "byard.toml".to_string()
                } else {
                    format!("package `{dependent}`")
                }
            )
        })?;

        let root = dep_root(&declarer_root, dep)?;

        // Verify fetched content against the lock pin (D-I), for every source
        // that is immutable once fetched. Path deps float
        // (they are the cooperative-dev loophole, like Cargo's path deps).
        if matches!(
            dep.source,
            DepSource::Git { .. } | DepSource::Registry { .. }
        ) {
            if let Some(locked) = self.lock.as_ref().and_then(|l| l.get(package)) {
                let actual = package_checksum(&root)?;
                if actual != locked.checksum {
                    return Err(format!(
                        "checksum mismatch for `{package}` (cache `{}`)\n\
                         locked:  {}\n\
                         found:   {actual}\n\
                         hint: the cache is corrupt; delete it and run `byard get`",
                        root.display(),
                        locked.checksum
                    ));
                }
            }
        }

        self.roots.insert(package.to_string(), root.clone());

        let files = collect_byd_files(&root)?;
        if files.is_empty() {
            return Err(format!(
                "package `{package}` at `{}` contains no `.byd` files",
                root.display()
            ));
        }
        files
            .into_iter()
            .map(|(rel, path)| {
                let source = std::fs::read_to_string(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                Ok(SourceFile {
                    name: format!("{package}/{rel}"),
                    source,
                })
            })
            .collect()
    }
}

// ── Project source collection ─────────────────────────────────────────────────

/// Collects the root project's `.byd` files: the entry first, then every
/// sibling (recursive under the entry's directory), sorted, the root package
/// is one namespace (RFC-0008), so sibling views need no `use`.
pub fn project_source_files(manifest: &Manifest) -> Result<Vec<SourceFile>, String> {
    let entry_dir = manifest
        .entry
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();

    let entry_src = std::fs::read_to_string(&manifest.entry)
        .map_err(|e| format!("{}: {e}", manifest.entry.display()))?;
    let entry_name = manifest.entry.file_name().map_or_else(
        || "main.byd".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );

    let mut files = vec![SourceFile {
        name: entry_name,
        source: entry_src,
    }];

    // A bare `.byd` entry (`byard check foo.byd`) compiles only itself; sibling
    // files in the same directory are unrelated (they may be other examples with
    // their own `View Main`). A real project treats every sibling as one
    // namespace (RFC-0008 Pillar B).
    if manifest.single_file {
        return Ok(files);
    }

    let mut siblings = Vec::new();
    walk(&entry_dir, &mut siblings)?;
    let mut rels: Vec<(String, PathBuf)> = siblings
        .into_iter()
        .filter(|p| p != &manifest.entry)
        .map(|p| {
            let rel = p
                .strip_prefix(&entry_dir)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            (rel, p)
        })
        .collect();
    rels.sort_by(|a, b| a.0.cmp(&b.0));

    for (rel, path) in rels {
        let source =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        files.push(SourceFile { name: rel, source });
    }
    Ok(files)
}

/// Resolves the whole program for `manifest`: project sources + dependencies.
/// The shared entry point `byard check` and `byard dev` both use, so the two
/// commands can never disagree about what the program *is*.
pub fn resolve_project(
    manifest: &Manifest,
) -> Result<(byard_compiler::resolve::ResolvedProgram, FsProvider), String> {
    let lock = Lockfile::read(&manifest.project_root)?;
    let mut provider = FsProvider::new(manifest, lock);
    let files = project_source_files(manifest)?;
    let program = byard_compiler::resolve::resolve_program(files, &mut provider);
    Ok((program, provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_is_deterministic_and_content_sensitive() {
        let dir = std::env::temp_dir().join(format!("byard-deps-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("byard.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::write(dir.join("src/a.byd"), "View A() { Text(\"a\") }").unwrap();

        let first = package_checksum(&dir).unwrap();
        let again = package_checksum(&dir).unwrap();
        assert_eq!(first, again, "same content ⇒ same hash");
        assert!(first.starts_with("sha256:"));

        std::fs::write(dir.join("src/a.byd"), "View A() { Text(\"b\") }").unwrap();
        let changed = package_checksum(&dir).unwrap();
        assert_ne!(first, changed, "changed content ⇒ changed hash");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_dependency_resolves_end_to_end() {
        // A real on-disk project + package, through the full provider chain:
        // manifest → FsProvider → resolver → canonical views (RFC-0008 e2e).
        let dir = std::env::temp_dir().join(format!("byard-e2e-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("app")).unwrap();
        std::fs::create_dir_all(dir.join("kit/src")).unwrap();

        std::fs::write(
            dir.join("app/byard.toml"),
            "[project]\nname = \"app\"\nentry = \"main.byd\"\n\n\
             [dependencies]\nkit = { path = \"../kit\" }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("app/main.byd"),
            "use kit as k\nView App() { k.Chip() }",
        )
        .unwrap();
        std::fs::write(dir.join("kit/byard.toml"), "[package]\nname = \"kit\"\n").unwrap();
        std::fs::write(
            dir.join("kit/src/chip.byd"),
            "View Chip(label: Str = \"chip\") { Text(label) }",
        )
        .unwrap();

        let manifest = Manifest {
            project_root: dir.join("app"),
            entry: dir.join("app/main.byd"),
            name: "app".into(),
            dependencies: vec![Dependency {
                name: "kit".into(),
                source: DepSource::Path(PathBuf::from("../kit")),
            }],
            single_file: false,
            vector_includes: Vec::new(),
            theme: byard_compiler::interp::theme::Theme::byard_base(),
            dev: crate::manifest::DevConfig::default(),
        };
        let (program, provider) = resolve_project(&manifest).unwrap();
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        let names: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["App", "kit.Chip"]);
        assert!(provider.resolved_roots().contains_key("kit"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lockfile_round_trips() {
        let dir = std::env::temp_dir().join(format!("byard-lock-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let lock = Lockfile {
            packages: vec![LockedPackage {
                name: "material".into(),
                source: "git+https://example.com/mat#tag=v1".into(),
                commit: "abc123".into(),
                checksum: "sha256:00ff".into(),
            }],
        };
        lock.write(&dir).unwrap();
        let read = Lockfile::read(&dir).unwrap().unwrap();
        assert_eq!(read.packages, lock.packages);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A lockfile written before packages could ship assets must still match
    /// the packages it pinned: a package that declares no assets hashes to
    /// exactly what it always did. The expected value was computed from the
    /// documented algorithm (path, NUL, little-endian length, bytes, in path
    /// order) independently of this code, and checked against it before the
    /// algorithm learned about assets.
    #[test]
    fn an_asset_free_package_hashes_as_it_always_did() {
        let dir = std::env::temp_dir().join(format!("byard-hash-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("byard.toml"),
            "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/a.byd"), "View A() { Box {} }\n").unwrap();
        assert_eq!(
            package_checksum(&dir).unwrap(),
            "sha256:dd130c6b14c3ba2be36f02204ec479a1d3efb94d0574d8ff5b31fcce4c884956"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
