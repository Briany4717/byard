//! `byard get`, fetch dependencies and write `byard.lock`
//! (RFC-0008 Pillar C, decisions D-H/D-I).
//!
//! The **only** command that may change the lockfile. Walks the dependency
//! graph transitively (path deps in place, git deps into `~/.byard/cache`
//! pinned by `rev`/`tag`), lets the solver pick one version for every
//! registry requirement (D-K), content-hashes every package, and writes the
//! pins. A lock that already satisfies every requirement is kept as it is.

use crate::style;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::deps::{
    LockedPackage, Lockfile, dep_root, fetch_git, git_cache_path, package_checksum, source_string,
};
use crate::manifest::{DepSource, Dependency, Manifest, parse_dependencies};
use byard_project::solve::{self, FixedPackage};

pub fn run() -> Result<(), String> {
    let manifest = Manifest::discover(None)?;
    let started = std::time::Instant::now();
    style::action(&format!("resolving dependencies of `{}`", manifest.name));
    let previous = Lockfile::read(&manifest.project_root)?;

    // Path and git packages first: they are on disk (or fetched now), each
    // with exactly one version, and their manifests say what they require.
    let mut locked: Vec<LockedPackage> = Vec::new();
    let mut fixed = vec![FixedPackage {
        name: manifest.name.clone(),
        root: manifest.project_root.clone(),
        version: package_version(&manifest.project_root)?,
        dependencies: manifest.dependencies.clone(),
    }];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut next = 0;
    while next < fixed.len() {
        let (declarer_root, deps) = (fixed[next].root.clone(), fixed[next].dependencies.clone());
        next += 1;
        for dep in deps {
            if matches!(dep.source, DepSource::Registry { .. }) || !seen.insert(dep.name.clone()) {
                continue; // registry packages are the solver's; first path/git wins
            }
            let (root, commit) = ensure_present(&declarer_root, &dep)?;
            let checksum = package_checksum(&root)?;
            style::info(&format!(
                "{} {} ({})",
                if matches!(dep.source, DepSource::Path(_)) {
                    "path"
                } else {
                    "git "
                },
                dep.name,
                short(&checksum)
            ));
            locked.push(LockedPackage {
                name: dep.name.clone(),
                source: source_string(&dep),
                commit,
                version: String::new(),
                checksum,
            });
            fixed.push(FixedPackage {
                name: dep.name.clone(),
                version: package_version(&root)?,
                dependencies: package_deps(&root)?,
                root,
            });
        }
    }

    // Then one version for every registry requirement (RFC-0008 D-K).
    let resolution = solve::resolve(&fixed, previous.as_ref())?;
    if !resolution.read_index && !resolution.packages.is_empty() {
        style::info("registry packages unchanged: byard.lock still satisfies byard.toml");
    }
    for (package, _root) in resolution.packages {
        style::info(&format!(
            "reg  {} {} ({})",
            package.name,
            package.version,
            short(&package.checksum)
        ));
        locked.push(package);
    }

    let n = locked.len();
    Lockfile { packages: locked }.write(&manifest.project_root)?;
    style::ok(
        &format!(
            "locked {n} package{} -> byard.lock",
            if n == 1 { "" } else { "s" }
        ),
        Some(started.elapsed()),
    );
    Ok(())
}

/// The first twelve hex digits of a `sha256:` checksum.
fn short(checksum: &str) -> &str {
    checksum
        .split(':')
        .nth(1)
        .map_or("", |h| &h[..h.len().min(12)])
}

/// The `[package] version` of the manifest at `root`, or `0.0.0` without one:
/// what a registry requirement on a path or git package is checked against.
fn package_version(root: &Path) -> Result<semver::Version, String> {
    let path = root.join("byard.toml");
    let written = std::fs::read_to_string(&path)
        .ok()
        .and_then(|src| src.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("package")
                .and_then(|p| p.get("version"))
                .and_then(toml::Value::as_str)
                .map(str::to_string)
        });
    match written {
        None => Ok(semver::Version::new(0, 0, 0)),
        Some(v) => semver::Version::parse(&v)
            .map_err(|e| format!("{}: [package] version `{v}`: {e}", path.display())),
    }
}

/// Makes sure a dependency is on disk (fetching git sources into the cache),
/// returning its root and the exact commit (empty for path deps).
fn ensure_present(declarer_root: &Path, dep: &Dependency) -> Result<(PathBuf, String), String> {
    match &dep.source {
        DepSource::Path(_) => Ok((dep_root(declarer_root, dep, None)?, String::new())),
        DepSource::Registry { .. } => Err(format!(
            "internal: `{}` is a registry package, which the solver fetches",
            dep.name
        )),
        DepSource::Git { url, reference } => {
            let dest = git_cache_path(&dep.name, url, reference);
            let commit = if dest.is_dir() {
                // Already cached, pinned refs are immutable, no refetch.
                let out = std::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(&dest)
                    .output()
                    .map_err(|e| format!("failed to run git: {e}"))?;
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            } else {
                style::action(&format!(
                    "fetching {} ({url}#{})",
                    dep.name,
                    reference.as_str()
                ));
                fetch_git(url, reference, &dest)?
            };
            Ok((dest, commit))
        }
    }
}

/// The `[dependencies]` of a fetched package's own manifest, if any.
fn package_deps(root: &Path) -> Result<Vec<Dependency>, String> {
    let manifest_path = root.join("byard.toml");
    if !manifest_path.exists() {
        return Ok(Vec::new());
    }
    let src = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let table: toml::Table = src
        .parse()
        .map_err(|e: toml::de::Error| format!("{}: {e}", manifest_path.display()))?;
    match table.get("dependencies") {
        Some(d) => parse_dependencies(d),
        None => Ok(Vec::new()),
    }
}
