//! `byard add <name>` (alias: `byard install`), record a dependency in
//! `byard.toml`, then fetch and lock it (RFC-0008 Pillar C).
//!
//! Sources, in order of preference:
//!
//! - `byard add kit --path ../kit`, a local package (cooperative dev);
//! - `byard add material --git <url> --tag v0.1.0` (or `--rev <hash>`),
//!   a pinned git source (D-H);
//! - `byard add weather --registry <url|dir> [--version <req>]`, a registry
//!   package (D-K). Without `--version` the requirement is `^` the highest
//!   version published, as `cargo add` does;
//! - `byard add material`, bare names resolve through a small **built-in
//!   index** of well-known packages, a deliberate stopgap until the hosted
//!   registry (deferred by D-H) exists. The resolved git source is written
//!   into `byard.toml` explicitly, so the manifest never depends on the index.

use crate::style;
use std::path::Path;

use crate::manifest::Manifest;

/// Well-known package names → git URL. The stopgap registry (see module docs).
const BUILTIN_INDEX: &[(&str, &str)] = &[(
    "material",
    "https://github.com/byard-framework/byard-material",
)];

pub struct AddArgs<'a> {
    pub name: &'a str,
    pub path: Option<&'a Path>,
    pub git: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub rev: Option<&'a str>,
    pub registry: Option<&'a str>,
    pub version: Option<&'a str>,
}

pub fn run(args: &AddArgs) -> Result<(), String> {
    let manifest = Manifest::discover(None).map_err(|e| e.to_string())?;
    let manifest_path = manifest.project_root.join("byard.toml");
    if !manifest_path.exists() {
        return Err(
            "`byard add` needs a byard.toml (bare-file projects have no dependencies)\n\
             hint: run `byard new <name>` to create a project"
                .to_string(),
        );
    }

    validate_dep_name(args.name)?;

    // ── Decide the source ─────────────────────────────────────────────────────
    if args.version.is_some() && args.registry.is_none() {
        return Err("`--version` only applies to `--registry` sources".into());
    }
    let entry_toml = match (args.path, args.git) {
        _ if args.registry.is_some() => {
            if args.path.is_some() || args.git.is_some() || args.tag.is_some() || args.rev.is_some()
            {
                return Err("`--registry` cannot be combined with `--path` or `--git`".into());
            }
            registry_entry(
                &manifest.project_root,
                args.name,
                args.registry.unwrap_or_default(),
                args.version,
            )?
        }
        (Some(_), Some(_)) => return Err("`--path` and `--git` are mutually exclusive".into()),
        (Some(path), None) => {
            if args.tag.is_some() || args.rev.is_some() {
                return Err("`--tag`/`--rev` only apply to `--git` sources".into());
            }
            format!("{{ path = {:?} }}", path.to_string_lossy())
        }
        (None, Some(url)) => git_entry(url, args.tag, args.rev)?,
        (None, None) => {
            let Some((_, url)) = BUILTIN_INDEX.iter().find(|(n, _)| *n == args.name) else {
                return Err(format!(
                    "`{}` is not in the built-in index and no source was given\n\
                     hint: byard add {} --path <dir>\n\
                     hint: byard add {} --git <url> --tag <tag>",
                    args.name, args.name, args.name
                ));
            };
            style::info(&format!(
                "`{}` resolved via the built-in index -> {url}",
                args.name
            ));
            git_entry(url, args.tag, args.rev)?
        }
    };

    // ── Edit byard.toml preserving formatting ─────────────────────────────────
    let src = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let mut doc: toml_edit::DocumentMut = src
        .parse()
        .map_err(|e: toml_edit::TomlError| format!("byard.toml: {e}"))?;

    let dep_value: toml_edit::DocumentMut = format!("dep = {entry_toml}")
        .parse()
        .map_err(|e: toml_edit::TomlError| format!("internal: {e}"))?;
    let dep_item = dep_value["dep"].clone();

    if doc.get("dependencies").is_none() {
        doc["dependencies"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let already = doc["dependencies"].get(args.name).is_some();
    doc["dependencies"][args.name] = dep_item;

    std::fs::write(&manifest_path, doc.to_string())
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    style::ok(
        &format!(
            "{} `{}` in byard.toml -> {entry_toml}",
            if already { "updated" } else { "added" },
            args.name
        ),
        None,
    );

    // ── Fetch + lock (the `get` pass) ─────────────────────────────────────────
    super::get::run()
}

/// A `{ registry, version }` entry. Without a requirement, `^` the highest
/// version the registry has published, which is the newest one and every
/// compatible release after it.
fn registry_entry(
    project_root: &Path,
    name: &str,
    registry: &str,
    version: Option<&str>,
) -> Result<String, String> {
    // A directory is written relative to the project, whatever the shell's
    // directory was and however it was spelled, so `byard.toml` and the lock
    // mean the same thing on another machine (RFC-0056).
    let registry = &portable_registry(project_root, registry)?;
    let requirement = if let Some(req) = version {
        semver::VersionReq::parse(req)
            .map_err(|e| format!("`--version {req}` is not a version requirement: {e}"))?;
        req.to_string()
    } else {
        let location = crate::manifest::RegistryLocation::parse(registry);
        let index = crate::deps::Registry::at(project_root, &location)
            .index()
            .map_err(|e| e.to_string())?;
        let newest = index
            .entries
            .iter()
            .filter(|e| e.name == name)
            .filter_map(|e| semver::Version::parse(&e.version).ok())
            .filter(|v| v.pre.is_empty())
            .max()
            .ok_or_else(|| format!("`{name}` has no version published in `{registry}`"))?;
        style::info(&format!("newest published: {name} {newest}"));
        format!("^{newest}")
    };
    Ok(format!(
        "{{ registry = {registry:?}, version = {requirement:?} }}"
    ))
}

/// `registry` as `byard.toml` should say it: a URL as given, a directory
/// relative to `project_root`. A directory on another root (another drive)
/// has no relative form and stays as given.
fn portable_registry(project_root: &Path, registry: &str) -> Result<String, String> {
    if matches!(
        crate::manifest::RegistryLocation::parse(registry),
        crate::manifest::RegistryLocation::Http(_)
    ) {
        return Ok(registry.to_string());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let at = normalised(&cwd.join(registry));
    let at = at.canonicalize().unwrap_or(at);
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    Ok(relative_to(&root, &at).map_or_else(
        || registry.to_string(),
        |rel| rel.to_string_lossy().replace('\\', "/"),
    ))
}

/// `path` without `.` and `x/..` parts, for a directory that may not exist yet.
fn normalised(path: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `target` as a path from `base`, both absolute: `../registry`, `.`.
fn relative_to(base: &Path, target: &Path) -> Option<std::path::PathBuf> {
    use std::path::Component;
    let (b, t): (Vec<_>, Vec<_>) = (base.components().collect(), target.components().collect());
    let common = b.iter().zip(&t).take_while(|(x, y)| x == y).count();
    // Nothing shared but the root: another drive, or no relative form worth
    // writing.
    if common == 0
        || b[common..]
            .iter()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return None;
    }
    let mut out = std::path::PathBuf::new();
    for _ in &b[common..] {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Some(out)
}

fn git_entry(url: &str, tag: Option<&str>, rev: Option<&str>) -> Result<String, String> {
    match (tag, rev) {
        (Some(_), Some(_)) => Err("`--tag` and `--rev` are mutually exclusive".into()),
        (Some(tag), None) => Ok(format!("{{ git = {url:?}, tag = {tag:?} }}")),
        (None, Some(rev)) => Ok(format!("{{ git = {url:?}, rev = {rev:?} }}")),
        (None, None) => {
            // No pin given: resolve the remote's default-branch HEAD to an
            // exact commit *now*, and record that (D-H: never a floating ref).
            let out = std::process::Command::new("git")
                .args(["ls-remote", url, "HEAD"])
                .output()
                .map_err(|e| format!("failed to run git: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "git ls-remote {url} failed:\n{}\n\
                     hint: pin explicitly with `--tag <tag>` or `--rev <hash>`",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            let stdout = String::from_utf8_lossy(&out.stdout);
            let rev = stdout
                .split_whitespace()
                .next()
                .ok_or("git ls-remote returned nothing; pin with `--tag`/`--rev`")?;
            style::info(&format!(
                "pinned to current HEAD {rev} (refs are always exact)"
            ));
            Ok(format!("{{ git = {url:?}, rev = {rev:?} }}"))
        }
    }
}

fn validate_dep_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid package name `{name}`: lowercase identifiers only \
             ([a-z][a-z0-9_]*), so it can be written after `use` in byld"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dep_names_must_be_byld_identifiers() {
        assert!(validate_dep_name("material").is_ok());
        assert!(validate_dep_name("liquid_glass2").is_ok());
        assert!(validate_dep_name("Material").is_err());
        assert!(validate_dep_name("my-kit").is_err());
        assert!(validate_dep_name("").is_err());
    }

    #[test]
    fn a_path_is_written_relative_to_the_project() {
        let rel = |base: &str, target: &str| relative_to(Path::new(base), Path::new(target));
        assert_eq!(
            rel("/w/app", "/w/registry"),
            Some(std::path::PathBuf::from("../registry"))
        );
        assert_eq!(
            rel("/w/app", "/w/app/vendor/reg"),
            Some(std::path::PathBuf::from("vendor/reg"))
        );
        assert_eq!(rel("/w/app", "/w/app"), Some(std::path::PathBuf::from(".")));
        assert_eq!(
            rel("/a/b/c", "/x"),
            Some(std::path::PathBuf::from("../../../x"))
        );
    }

    #[test]
    fn git_entry_requires_exclusive_pins() {
        assert!(git_entry("https://e.com", Some("v1"), Some("abc")).is_err());
        assert_eq!(
            git_entry("https://e.com", Some("v1"), None).unwrap(),
            "{ git = \"https://e.com\", tag = \"v1\" }"
        );
    }
}
