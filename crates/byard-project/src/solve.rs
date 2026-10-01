//! Picking versions for registry requirements (RFC-0008 D-K).
//!
//! `byard get` hands this the dependency graph it has on disk (the project,
//! its path and git packages, each with the requirements it declares) and
//! gets back one exact version for every registry package. The solver is
//! `PubGrub`: its failure is a derivation, so "no versions satisfy" comes with
//! the chain of requirements that made it so, which is the only kind of
//! answer a user can act on.
//!
//! Three rules shape it:
//!
//! - **The lock is preferred.** A version already in `byard.lock` that still
//!   satisfies every requirement is chosen over a newer one, so adding one
//!   dependency does not quietly upgrade the others. A lock that satisfies
//!   everything, with every package already cached, is honoured without
//!   reading any index at all.
//! - **Path and git packages are fixed.** Each is a package with exactly one
//!   version (its `[package] version`, or `0.0.0`), so a registry package
//!   that asks for it by a range it does not satisfy is a conflict with a
//!   name, not a silent override.
//! - **One source per name.** The namespace is flat; a name asked for from
//!   two registries is an error before solving.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use pubgrub::{
    DefaultStringReporter, Dependencies, DependencyConstraints, DependencyProvider,
    PackageResolutionStatistics, PubGrubError, Ranges, Reporter as _, SemanticVersion,
};

use crate::deps::{
    IndexEntry, LockedPackage, Lockfile, Registry, RegistryIndex, fetch_registry, package_checksum,
    registry_cache_path, registry_source,
};
use crate::manifest::{DepSource, Dependency, RegistryLocation, parse_dependencies};

type Range = Ranges<SemanticVersion>;

/// A package `byard get` already has on disk: the project itself, or a path or
/// git dependency.
#[derive(Clone, Debug)]
pub struct FixedPackage {
    /// Its name; the project's is the project name.
    pub name: String,
    /// The directory its manifest is in, which its registry locations are
    /// relative to.
    pub root: PathBuf,
    /// Its `[package] version`, or `0.0.0` when it declares none.
    pub version: semver::Version,
    /// What it declares in `[dependencies]`.
    pub dependencies: Vec<Dependency>,
}

/// One registry package `byard get` should fetch and lock.
#[derive(Clone, Debug)]
pub struct Chosen {
    /// The registry it comes from.
    pub registry: Registry,
    /// Its index entry at the chosen version.
    pub entry: IndexEntry,
    /// Its lockfile source string.
    pub source: String,
}

/// What a resolution produced.
#[derive(Debug)]
pub struct Resolution {
    /// The lock entries of every registry package, its root on disk beside it.
    pub packages: Vec<(LockedPackage, PathBuf)>,
    /// Whether any index was read; `false` when the lock was honoured as is.
    pub read_index: bool,
}

/// Resolves every registry requirement of `fixed`, whose first element is the
/// project. Fetches what it chose into the cache.
pub fn resolve(fixed: &[FixedPackage], previous: Option<&Lockfile>) -> Result<Resolution, String> {
    if let Some(packages) = from_lock(fixed, previous) {
        return Ok(Resolution {
            packages,
            read_index: false,
        });
    }
    let root = fixed
        .first()
        .ok_or("internal: resolve needs the project as its first package")?;
    let mut sources = Sources::default();
    let mut graph = build_graph(fixed, &mut sources)?;
    if let Some(lock) = previous {
        for p in &lock.packages {
            if let Ok(v) = semver::Version::parse(&p.version) {
                graph.locked.insert(p.name.clone(), version_of(&v));
            }
        }
    }

    let solution = pubgrub::resolve(&graph, root.name.clone(), version_of(&root.version)).map_err(
        |e| match e {
            // Not collapsed: "there is no version of X in that range" is
            // the most useful step of the explanation, and collapsing drops
            // exactly that one.
            PubGrubError::NoSolution(tree) => {
                let report = DefaultStringReporter::report(&tree);
                format!(
                    "no set of versions satisfies every requirement:\n{report}{}",
                    graph.published_versions(&report)
                )
            }
            other => format!("resolving dependencies failed: {other}"),
        },
    )?;

    let mut packages = Vec::new();
    for (name, version) in solution {
        let Some(entry) = graph.entries.get(&(name.clone(), version)) else {
            continue; // the project, or a path or git package
        };
        let (registry, source) = sources.of(&name);
        let root = fetch_registry(&registry, entry)?;
        packages.push((
            LockedPackage {
                name: name.clone(),
                source,
                commit: String::new(),
                version: entry.version.clone(),
                checksum: entry.checksum.clone(),
            },
            root,
        ));
    }
    packages.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    Ok(Resolution {
        packages,
        read_index: true,
    })
}

/// The whole graph the solver reads: every fixed package with its one
/// version, and every published version of every registry package reachable
/// from them, each with what it requires.
fn build_graph(fixed: &[FixedPackage], sources: &mut Sources) -> Result<Graph, String> {
    let mut graph = Graph::default();

    // The fixed packages: one version each, their requirements as declared.
    let fixed_names: BTreeMap<&str, &FixedPackage> =
        fixed.iter().map(|p| (p.name.as_str(), p)).collect();
    let mut pending: Vec<String> = Vec::new();
    for pkg in fixed {
        let mut deps = DependencyConstraints::default();
        for dep in &pkg.dependencies {
            let range = match &dep.source {
                DepSource::Path(_) | DepSource::Git { .. } => Range::full(),
                DepSource::Registry { registry, version } => {
                    if !fixed_names.contains_key(dep.name.as_str()) {
                        let at = Registry::at(&pkg.root, registry);
                        sources.claim(&dep.name, at, registry_source(registry, &dep.name))?;
                        pending.push(dep.name.clone());
                    }
                    requirement(&dep.name, version)?
                }
            };
            deps.insert(dep.name.clone(), range);
        }
        graph.add(&pkg.name, version_of(&pkg.version), deps);
    }

    // Every registry package reachable from them, every published version.
    while let Some(name) = pending.pop() {
        if graph.versions.contains_key(&name) {
            continue;
        }
        let (registry, source) = sources.of(&name);
        let index = sources.index(&registry)?;
        let mut published = 0;
        for entry in index.entries.iter().filter(|e| e.name == name) {
            // A pre-release is never chosen by a plain requirement; skipped
            // rather than half-supported.
            let Some(version) = semver::Version::parse(&entry.version)
                .ok()
                .filter(|v| v.pre.is_empty())
            else {
                continue;
            };
            let mut deps = DependencyConstraints::default();
            for dep in dependencies_of(&registry, entry)? {
                let at = match &dep.registry {
                    None => registry.clone(),
                    Some(written) => located(&registry, written)?,
                };
                if !fixed_names.contains_key(dep.name.as_str()) {
                    let dep_source = match &dep.registry {
                        None => format!("registry+{}#{}", source_registry(&source), dep.name),
                        Some(written) => {
                            registry_source(&RegistryLocation::parse(written), &dep.name)
                        }
                    };
                    sources.claim(&dep.name, at, dep_source)?;
                    pending.push(dep.name.clone());
                }
                deps.insert(dep.name.clone(), requirement(&dep.name, &dep.version)?);
            }
            graph.add(&name, version_of(&version), deps);
            graph
                .entries
                .insert((name.clone(), version_of(&version)), entry.clone());
            published += 1;
        }
        if published == 0 {
            // Still a package, with no versions: the solver then explains
            // which requirement asked for it.
            graph.versions.entry(name.clone()).or_default();
        }
    }

    Ok(graph)
}

/// The lock, if it already satisfies every registry requirement reachable
/// from `fixed` with every package in the cache: then nothing is read from any
/// registry. `None` as soon as one requirement is new, changed or unmet.
fn from_lock(
    fixed: &[FixedPackage],
    previous: Option<&Lockfile>,
) -> Option<Vec<(LockedPackage, PathBuf)>> {
    let lock = previous?;
    let fixed_names: Vec<&str> = fixed.iter().map(|p| p.name.as_str()).collect();
    let mut chosen: BTreeMap<String, (LockedPackage, PathBuf)> = BTreeMap::new();
    // (declarer root, its dependencies), walked through cached packages.
    let mut queue: Vec<(PathBuf, Vec<Dependency>)> = fixed
        .iter()
        .map(|p| (p.root.clone(), p.dependencies.clone()))
        .collect();
    while let Some((declarer, deps)) = queue.pop() {
        for dep in deps {
            let DepSource::Registry { registry, version } = &dep.source else {
                continue;
            };
            if fixed_names.contains(&dep.name.as_str()) {
                continue;
            }
            let locked = lock.get(&dep.name)?;
            let req = semver::VersionReq::parse(version).ok()?;
            if !req.matches(&semver::Version::parse(&locked.version).ok()?) {
                return None;
            }
            if chosen.contains_key(&dep.name) {
                continue;
            }
            // A package declared from its own registry's cache entry names
            // that registry relatively, which says nothing about where the
            // lock got it from; only a top-level declaration is compared.
            let is_cached = declarer.starts_with(crate::deps::cache_dir());
            if !is_cached && locked.source != registry_source(registry, &dep.name) {
                return None;
            }
            let root = registry_cache_path(&dep.name, &locked.version, &locked.checksum);
            if !root.is_dir() || package_checksum(&root).ok()? != locked.checksum {
                return None;
            }
            queue.push((root.clone(), manifest_dependencies(&root).ok()?));
            chosen.insert(dep.name.clone(), (locked.clone(), root));
        }
    }
    Some(chosen.into_values().collect())
}

/// The `[dependencies]` of the package at `root`, if it has a manifest.
fn manifest_dependencies(root: &Path) -> Result<Vec<Dependency>, String> {
    let path = root.join("byard.toml");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let table: toml::Table = src
        .parse()
        .map_err(|e: toml::de::Error| format!("{}: {e}", path.display()))?;
    table
        .get("dependencies")
        .map_or_else(|| Ok(Vec::new()), parse_dependencies)
}

/// What a published version depends on: from its index entry, or, for an
/// entry written before indexes recorded it, from the manifest in its
/// archive. There a registry directory is taken as this registry, the only
/// reading of a relative path that survives being published.
fn dependencies_of(
    registry: &Registry,
    entry: &IndexEntry,
) -> Result<Vec<crate::deps::IndexDep>, String> {
    if let Some(deps) = &entry.dependencies {
        return Ok(deps.clone());
    }
    let root = fetch_registry(registry, entry)?;
    Ok(manifest_dependencies(&root)?
        .into_iter()
        .filter_map(|d| match d.source {
            DepSource::Registry { registry, version } => Some(crate::deps::IndexDep {
                name: d.name,
                version,
                registry: match registry {
                    RegistryLocation::Http(url) => Some(url),
                    RegistryLocation::Dir(_) => None,
                },
            }),
            _ => None,
        })
        .collect())
}

/// A registry an index entry names: a URL, or a directory relative to the
/// registry directory that holds the index.
fn located(parent: &Registry, written: &str) -> Result<Registry, String> {
    match (RegistryLocation::parse(written), parent) {
        (RegistryLocation::Http(url), _) => Ok(Registry::Http(url)),
        (RegistryLocation::Dir(rel), Registry::Dir(dir)) => Ok(Registry::Dir(dir.join(rel))),
        (RegistryLocation::Dir(rel), Registry::Http(url)) => Err(format!(
            "the index at `{url}` names the registry directory `{}`, which a server cannot reach",
            rel.display()
        )),
    }
}

/// The registry part of a `registry+<at>#<name>` source string.
fn source_registry(source: &str) -> &str {
    source
        .trim_start_matches("registry+")
        .rsplit_once('#')
        .map_or(source, |(at, _)| at)
}

/// `semver` to the solver's version type. Build metadata never orders.
fn version_of(v: &semver::Version) -> SemanticVersion {
    let part = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
    SemanticVersion::new(part(v.major), part(v.minor), part(v.patch))
}

/// A requirement, Cargo's semantics, as the solver's range.
fn requirement(name: &str, written: &str) -> Result<Range, String> {
    let req = semver::VersionReq::parse(written).map_err(|e| {
        format!("dependency `{name}`: `{written}` is not a version requirement: {e}")
    })?;
    req.comparators.iter().try_fold(Range::full(), |acc, c| {
        Ok(acc.intersection(&comparator(c).ok_or_else(|| {
            format!("dependency `{name}`: `{written}` uses a comparator byard does not support")
        })?))
    })
}

/// One comparator as a range. A partial version (`1`, `1.2`) means what Cargo
/// means by it: every version it does not rule out.
fn comparator(c: &semver::Comparator) -> Option<Range> {
    use semver::Op;
    let v = |major: u64, minor: u64, patch: u64| {
        SemanticVersion::new(
            u32::try_from(major).unwrap_or(u32::MAX),
            u32::try_from(minor).unwrap_or(u32::MAX),
            u32::try_from(patch).unwrap_or(u32::MAX),
        )
    };
    let (major, minor, patch) = (c.major, c.minor, c.patch);
    let low = v(major, minor.unwrap_or(0), patch.unwrap_or(0));
    // The first version past everything the written prefix names.
    let past_prefix = match (minor, patch) {
        (None, _) => v(major + 1, 0, 0),
        (Some(m), None) => v(major, m + 1, 0),
        (Some(m), Some(p)) => v(major, m, p + 1),
    };
    Some(match c.op {
        Op::Exact | Op::Wildcard => Range::between(low, past_prefix),
        Op::Greater => match (minor, patch) {
            (Some(_), Some(_)) => Range::strictly_higher_than(low),
            _ => Range::higher_than(past_prefix),
        },
        Op::GreaterEq => Range::higher_than(low),
        Op::Less => Range::strictly_lower_than(low),
        Op::LessEq => Range::strictly_lower_than(past_prefix),
        Op::Tilde => match minor {
            None => Range::between(low, v(major + 1, 0, 0)),
            Some(m) => Range::between(low, v(major, m + 1, 0)),
        },
        Op::Caret => {
            let upper = match (major, minor, patch) {
                (0, None, _) => v(1, 0, 0),
                (0, Some(0), None) => v(0, 1, 0),
                (0, Some(0), Some(p)) => v(0, 0, p + 1),
                (0, Some(m), _) => v(0, m + 1, 0),
                (maj, _, _) => v(maj + 1, 0, 0),
            };
            Range::between(low, upper)
        }
        _ => return None,
    })
}

/// Where each registry package comes from, and every index read so far.
#[derive(Default)]
struct Sources {
    /// Package name → (registry, lock source string).
    by_name: BTreeMap<String, (Registry, String)>,
    /// Indexes read, by registry label: each is read once per resolution.
    indexes: BTreeMap<String, RegistryIndex>,
}

impl Sources {
    /// Records that `name` comes from `registry`; asking for it from a
    /// second registry is an error, since the namespace is flat.
    fn claim(&mut self, name: &str, registry: Registry, source: String) -> Result<(), String> {
        match self.by_name.get(name) {
            Some((known, _)) if *known != registry => Err(format!(
                "`{name}` is asked for from two registries, `{}` and `{}`; \
                 a package name means one package",
                known.label(),
                registry.label()
            )),
            Some(_) => Ok(()),
            None => {
                self.by_name.insert(name.to_string(), (registry, source));
                Ok(())
            }
        }
    }

    fn of(&self, name: &str) -> (Registry, String) {
        self.by_name
            .get(name)
            .cloned()
            .unwrap_or_else(|| (Registry::Dir(PathBuf::new()), String::new()))
    }

    fn index(&mut self, registry: &Registry) -> Result<RegistryIndex, String> {
        let label = registry.label();
        if let Some(index) = self.indexes.get(&label) {
            return Ok(index.clone());
        }
        let index = registry.index()?;
        self.indexes.insert(label, index.clone());
        Ok(index)
    }
}

/// The whole graph in memory, as `PubGrub` reads it.
#[derive(Default)]
struct Graph {
    /// Package → version → what that version requires.
    versions: BTreeMap<String, BTreeMap<SemanticVersion, DependencyConstraints<String, Range>>>,
    /// The index entry behind each registry package version.
    entries: BTreeMap<(String, SemanticVersion), IndexEntry>,
    /// Versions the previous lock chose, preferred while they still fit.
    locked: BTreeMap<String, SemanticVersion>,
}

impl Graph {
    /// "published: …" lines for every registry package `report` names, so a
    /// requirement nothing meets comes with what there is to choose from.
    fn published_versions(&self, report: &str) -> String {
        let mut out = String::new();
        let mut by_name: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (name, version) in self.entries.keys() {
            by_name.entry(name).or_default().push(version.to_string());
        }
        for (name, versions) in &self.versions {
            let named = report
                .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
                .any(|word| word == name);
            if !named {
                continue;
            }
            match by_name.get(name.as_str()) {
                Some(published) => {
                    let _ = write!(out, "\n{name}: published {}", published.join(", "));
                }
                // A registry package nobody has published a version of.
                None if versions.is_empty() => {
                    let _ = write!(out, "\n{name}: no version is published");
                }
                // The project, or a path or git package: its one version is
                // already in the explanation.
                None => {}
            }
        }
        out
    }

    fn add(
        &mut self,
        name: &str,
        version: SemanticVersion,
        deps: DependencyConstraints<String, Range>,
    ) {
        self.versions
            .entry(name.to_string())
            .or_default()
            .insert(version, deps);
    }
}

impl DependencyProvider for Graph {
    type P = String;
    type V = SemanticVersion;
    type VS = Range;
    type M = String;
    type Err = Infallible;
    type Priority = (u32, Reverse<usize>);

    fn prioritize(
        &self,
        package: &String,
        range: &Range,
        statistics: &PackageResolutionStatistics,
    ) -> Self::Priority {
        // Fewest candidates first, as PubGrub's own in-memory provider does:
        // a package with one possible version settles everything below it.
        let count = self
            .versions
            .get(package)
            .map_or(0, |vs| vs.keys().filter(|v| range.contains(v)).count());
        if count == 0 {
            return (u32::MAX, Reverse(0));
        }
        (statistics.conflict_count(), Reverse(count))
    }

    fn choose_version(
        &self,
        package: &String,
        range: &Range,
    ) -> Result<Option<SemanticVersion>, Infallible> {
        let Some(versions) = self.versions.get(package) else {
            return Ok(None);
        };
        if let Some(locked) = self.locked.get(package) {
            if range.contains(locked) && versions.contains_key(locked) {
                return Ok(Some(*locked));
            }
        }
        Ok(versions.keys().rev().find(|v| range.contains(v)).copied())
    }

    fn get_dependencies(
        &self,
        package: &String,
        version: &SemanticVersion,
    ) -> Result<Dependencies<String, Range, String>, Infallible> {
        Ok(
            match self.versions.get(package).and_then(|vs| vs.get(version)) {
                Some(deps) => Dependencies::Available(deps.clone()),
                None => Dependencies::Unavailable("its dependencies are unknown".to_string()),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(req: &str, version: &str) -> bool {
        let v = semver::Version::parse(version).unwrap();
        requirement("x", req).unwrap().contains(&version_of(&v))
    }

    /// The range of every requirement agrees with `semver`'s own `matches`,
    /// which is Cargo's definition, over a grid of versions.
    #[test]
    fn requirements_mean_what_cargo_means() {
        let reqs = [
            "0.3",
            "^0.3",
            "^0.3.1",
            "^0.0.3",
            "^0.0",
            "^0",
            "1",
            "^1.2",
            "~1.2",
            "~1.2.3",
            "~1",
            "=1.2.3",
            "=1.2",
            "=1",
            ">1.2.3",
            ">1.2",
            ">1",
            ">=1.2",
            "<1.2.3",
            "<1.2",
            "<=1.2.3",
            "<=1.2",
            "<=1",
            "1.*",
            "1.2.*",
            ">=1.2, <1.5",
            "*",
        ];
        let mut versions = Vec::new();
        for major in 0..3 {
            for minor in 0..5 {
                for patch in 0..5 {
                    versions.push(format!("{major}.{minor}.{patch}"));
                }
            }
        }
        for req in reqs {
            let cargo = semver::VersionReq::parse(req).unwrap();
            for v in &versions {
                assert_eq!(
                    contains(req, v),
                    cargo.matches(&semver::Version::parse(v).unwrap()),
                    "{req} vs {v}"
                );
            }
        }
    }
}
