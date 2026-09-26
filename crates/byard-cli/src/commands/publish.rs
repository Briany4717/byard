//! `byard publish`, write a package into a registry (RFC-0008 D-H).
//!
//! A publish is a deterministic archive of exactly what a package is made of
//! (its manifest, its sources, its declared assets, and a README and licence
//! when present) plus one line in the registry's `index.toml` naming the
//! archive, its hash, and the package checksum a lockfile pins. Nothing about
//! resolution changes: a registry source is fetched, verified against that
//! checksum and cached, and from there it is a package like any other.
//!
//! Versions are immutable. Publishing the same content again is a no-op;
//! publishing different content under a version that exists is an error.

use std::path::{Path, PathBuf};

use crate::deps::{IndexEntry, RegistryIndex, package_checksum, publishable_files, sha256_tag};
use crate::style;

/// What `byard publish` was asked to do.
pub struct PublishArgs<'a> {
    /// The package directory; the current directory when absent.
    pub package: Option<&'a Path>,
    /// The registry directory; `BYARD_REGISTRY` when absent.
    pub registry: Option<&'a Path>,
}

pub fn run(args: &PublishArgs<'_>) -> Result<(), String> {
    let started = std::time::Instant::now();
    let root = match args.package {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let registry = match args.registry {
        Some(r) => r.to_path_buf(),
        None => std::env::var_os("BYARD_REGISTRY")
            .map(PathBuf::from)
            .ok_or(
                "no registry to publish to\nhint: pass `--registry <dir>` or set BYARD_REGISTRY",
            )?,
    };
    let published = publish(&root, &registry)?;
    style::ok(
        &format!(
            "{}@{} -> {} ({})",
            published.name,
            published.version,
            registry.join(&published.archive).display(),
            published.checksum
        ),
        Some(started.elapsed()),
    );
    Ok(())
}

/// Publishes the package at `root` into `registry`, returning its index entry.
pub fn publish(root: &Path, registry: &Path) -> Result<IndexEntry, String> {
    let manifest_path = root.join("byard.toml");
    let src = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let table: toml::Table = src
        .parse()
        .map_err(|e: toml::de::Error| format!("{}: {e}", manifest_path.display()))?;
    let package = table
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or("byard.toml: only a package can be published, and this has no [package] table")?;
    let field = |key: &str| {
        package
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("byard.toml: [package] needs `{key}` to be published"))
    };
    let (name, version) = (field("name")?, field("version")?);

    let files = publishable_files(root)?
        .into_iter()
        .map(|(rel, path)| {
            std::fs::read(&path)
                .map(|bytes| (rel, bytes))
                .map_err(|e| format!("{}: {e}", path.display()))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let archive = crate::archive::pack(&files)?;
    let entry = IndexEntry {
        archive: format!("{name}/{version}.tar.gz"),
        archive_sha256: sha256_tag(&archive),
        checksum: package_checksum(root)?,
        name,
        version,
    };

    let mut index = RegistryIndex::read(registry)?;
    if let Some(existing) = index.get(&entry.name, &entry.version) {
        if existing.checksum == entry.checksum {
            return Ok(existing.clone());
        }
        return Err(format!(
            "`{}@{}` is already published with different content; versions are immutable\n\
             hint: bump `version` in [package]",
            entry.name, entry.version
        ));
    }
    let archive_path = registry.join(&entry.archive);
    if let Some(parent) = archive_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&archive_path, &archive)
        .map_err(|e| format!("{}: {e}", archive_path.display()))?;
    index.entries.push(entry.clone());
    index.write(registry)?;
    Ok(entry)
}
