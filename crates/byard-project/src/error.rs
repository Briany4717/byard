//! What can go wrong reading a project (RFC-0043 §1).
//!
//! Every fallible function in this crate returns [`ProjectError`], so a caller
//! can tell a missing manifest from an unsatisfiable dependency from a full
//! disk without matching on the text of a message. The text is still the
//! point for a person: each variant's [`Display`](fmt::Display) reads as a
//! sentence, with the `hint:` line where there is one to give.
//!
//! The workspace writes its `Display`s by hand (`ByardError`, `AtlasError`)
//! and so does this: a derive macro would be one more proc-macro on every
//! build's path for no gain at run time.

use std::fmt;
use std::path::{Path, PathBuf};

/// A failure reading, resolving or fetching a project.
///
/// `#[non_exhaustive]`: a variant may be added without breaking a `match`
/// that already has a `_` arm.
#[non_exhaustive]
#[derive(Debug)]
pub enum ProjectError {
    /// No `byard.toml`, and no `main.byd` to stand in for one, where the
    /// search began.
    NoManifest {
        /// The directory the search began in (a directory that was named, or
        /// the working directory).
        searched_from: PathBuf,
    },

    /// A `byard.toml` that cannot be used: it does not parse, or a table in
    /// it says something that cannot be honoured.
    Manifest {
        /// The manifest the message is about.
        path: PathBuf,
        /// The 1-based line the problem is on, when the TOML parser found it.
        line: Option<usize>,
        /// What is wrong, in full: the message names the table and key.
        message: String,
    },

    /// A file or directory could not be read or written.
    Io {
        /// What was being opened.
        path: PathBuf,
        /// The operating system's answer.
        source: std::io::Error,
    },

    /// No set of dependencies satisfies the manifests (RFC-0008 D-K).
    Resolve(ResolveError),

    /// A dependency could not be got: a git clone, a registry request, a
    /// package that is not in the cache yet.
    Fetch {
        /// The package, when the failure is about one; `None` when it is
        /// about a registry as a whole (its index).
        package: Option<String>,
        /// Where it comes from, as the manifest wrote it: a git URL, a
        /// registry, a directory.
        source_desc: String,
        /// What went wrong, in full.
        message: String,
    },

    /// `byard.lock` cannot be read, or what it pins does not match.
    Lock {
        /// The lockfile.
        path: PathBuf,
        /// What is wrong, in full.
        message: String,
    },

    /// A package archive is malformed or refused.
    Archive {
        /// The file inside the archive the problem is about, or the
        /// directory it was being unpacked into; empty when neither fits.
        path: PathBuf,
        /// What is wrong, in full.
        message: String,
    },
}

impl ProjectError {
    /// A manifest problem with no line to point at.
    pub(crate) fn manifest(path: &Path, message: impl Into<String>) -> Self {
        Self::Manifest {
            path: path.to_path_buf(),
            line: None,
            message: message.into(),
        }
    }

    /// A manifest that does not parse. `src` is the text `error` was found
    /// in, which gives the line. The message leads with the file's path,
    /// because the parser's own text names a line and not a file, and an app
    /// started from `cargo run` has no command line to say which.
    pub(crate) fn toml(path: &Path, src: &str, error: &toml::de::Error) -> Self {
        Self::toml_message(path, src, error, format!("{}: {error}", path.display()))
    }

    fn toml_message(path: &Path, src: &str, error: &toml::de::Error, message: String) -> Self {
        let line = error
            .span()
            .map(|span| src[..span.start.min(src.len())].matches('\n').count() + 1);
        Self::Manifest {
            path: path.to_path_buf(),
            line,
            message,
        }
    }

    /// A file that could not be read or written.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// A lockfile problem.
    pub(crate) fn lock(path: &Path, message: impl Into<String>) -> Self {
        Self::Lock {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }

    /// An archive problem.
    pub(crate) fn archive(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self::Archive {
            path: path.into(),
            message: message.into(),
        }
    }

    /// A failure to get a dependency.
    pub(crate) fn fetch(
        package: Option<&str>,
        source_desc: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Fetch {
            package: package.map(str::to_string),
            source_desc: source_desc.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoManifest { searched_from } => write!(
                f,
                "no byard.toml found and no main.byd in `{}`\n\
                 hint: run `byard new <name>` to create a project, or pass a file path",
                searched_from.display()
            ),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Resolve(e) => write!(f, "{e}"),
            Self::Manifest { message, .. }
            | Self::Fetch { message, .. }
            | Self::Lock { message, .. }
            | Self::Archive { message, .. } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ProjectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Resolve(e) => Some(e),
            _ => None,
        }
    }
}

/// So an app's `main` can `?` a project function: `ByardError::Project` holds
/// it, and `byard::ByardErrorExt::as_project` hands it back.
impl From<ProjectError> for byard_core::ByardError {
    fn from(e: ProjectError) -> Self {
        Self::project(e)
    }
}

impl From<ResolveError> for ProjectError {
    fn from(e: ResolveError) -> Self {
        Self::Resolve(e)
    }
}

/// Why `byard get` could not choose versions (RFC-0008 D-K).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// No set of versions meets every requirement. The explanation is
    /// `PubGrub`'s derivation: the chain of requirements that made it so, and
    /// the versions each registry package has published.
    Unsatisfiable {
        /// The derivation, one step per line.
        explanation: String,
    },

    /// A requirement that cannot be solved as written.
    Requirement {
        /// The dependency.
        package: String,
        /// The requirement as the manifest wrote it.
        written: String,
        /// What is wrong with it, finishing the sentence "`written` ...".
        problem: String,
    },

    /// A package name asked for from two registries. The namespace is flat:
    /// a name means one package.
    TwoRegistries {
        /// The package.
        package: String,
        /// The registry it was first asked for from.
        first: String,
        /// The second.
        second: String,
    },

    /// An index names a registry directory that a server cannot reach.
    UnreachableRegistry {
        /// The index that names it.
        index: String,
        /// The directory it names.
        directory: String,
    },

    /// A package's own version does not fit the solver's version type.
    Version {
        /// The package.
        package: String,
        /// Its version.
        version: String,
        /// What is wrong with it.
        problem: String,
    },

    /// The solver failed for a reason that is not a conflict, or the
    /// caller broke its contract.
    Failed {
        /// What happened.
        message: String,
    },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsatisfiable { explanation } => write!(
                f,
                "no set of versions satisfies every requirement:\n{explanation}"
            ),
            Self::Requirement {
                package,
                written,
                problem,
            } => write!(f, "dependency `{package}`: `{written}` {problem}"),
            Self::TwoRegistries {
                package,
                first,
                second,
            } => write!(
                f,
                "`{package}` is asked for from two registries, `{first}` and `{second}`; \
                 a package name means one package"
            ),
            Self::UnreachableRegistry { index, directory } => write!(
                f,
                "the index at `{index}` names the registry directory `{directory}`, \
                 which a server cannot reach"
            ),
            Self::Version {
                package,
                version,
                problem,
            } => write!(f, "`{package}` {version}: {problem}"),
            Self::Failed { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ResolveError {}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::path::Path;

    use super::*;
    use crate::deps::{FsProvider, Lockfile, Registry, RegistryIndex};
    use crate::manifest::{Dependency, Manifest};

    type Packed<'a> = &'a [(String, Vec<u8>)];

    /// RFC-0043 acceptance: no public function of this crate answers with a
    /// `String` any more. Each line coerces the function to a pointer of the
    /// type it must have, so a signature that goes back to `String` stops
    /// this compiling.
    #[test]
    fn the_public_api_answers_with_project_error() {
        let _: fn(Option<&Path>) -> Result<Manifest, ProjectError> = Manifest::discover;
        let _: fn(&Manifest, &str) -> Result<(), ProjectError> = Manifest::require_app;
        let _: fn(&toml::Value, &Path) -> Result<Vec<Dependency>, ProjectError> =
            crate::manifest::parse_dependencies;
        let _: fn(&Manifest) -> Result<_, ProjectError> = crate::deps::resolve_project;
        let _: fn(&Manifest) -> Result<_, ProjectError> = crate::deps::project_source_files;
        let _: fn(&Manifest) -> Result<_, ProjectError> = crate::deps::owned_source_files;
        let _: fn(&Path) -> Result<_, ProjectError> = crate::deps::byd_files_under;
        let _: fn(&Path) -> Result<String, ProjectError> = crate::deps::package_checksum;
        let _: fn(&Path) -> Result<_, ProjectError> = crate::deps::declared_assets;
        let _: fn(&Path) -> Result<_, ProjectError> = crate::deps::publishable_files;
        let _: fn(&Path, &Dependency, Option<&Lockfile>) -> Result<_, ProjectError> =
            crate::deps::dep_root;
        let _: fn(&Path) -> Result<Option<Lockfile>, ProjectError> = Lockfile::read;
        let _: fn(&Lockfile, &Path) -> Result<(), ProjectError> = Lockfile::write;
        let _: fn(&Path) -> Result<RegistryIndex, ProjectError> = RegistryIndex::read;
        let _: fn(&str, &str) -> Result<RegistryIndex, ProjectError> = RegistryIndex::parse;
        let _: fn(&RegistryIndex, &Path) -> Result<(), ProjectError> = RegistryIndex::write;
        let _: fn(&Registry) -> Result<RegistryIndex, ProjectError> = Registry::index;
        let _: fn(&Registry, &str) -> Result<Vec<u8>, ProjectError> = Registry::read;
        let _: fn(&Registry, &crate::deps::IndexEntry) -> Result<_, ProjectError> =
            crate::deps::fetch_registry;
        let _: fn(&str, &str, &crate::manifest::GitRef, &Path) -> Result<String, ProjectError> =
            crate::deps::fetch_git;
        let _: fn(&[crate::solve::FixedPackage], Option<&Lockfile>) -> Result<_, ProjectError> =
            crate::solve::resolve;
        let _: fn(Packed<'_>) -> Result<Vec<u8>, ProjectError> = crate::archive::pack;
        let _: fn(&[u8], &Path) -> Result<Vec<String>, ProjectError> = crate::archive::unpack;
        // `FsProvider` answers the compiler's `PackageProvider`, whose error
        // is text by the compiler's own contract.
        let _ = std::mem::size_of::<FsProvider>();
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("byard-project-error-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A directory with neither a manifest nor a `main.byd` is `NoManifest`,
    /// and says which directory was searched.
    #[test]
    fn a_missing_manifest_names_the_directory_searched() {
        let dir = scratch("empty");
        let err = Manifest::discover(Some(&dir)).err().unwrap();
        let searched = dir.canonicalize().unwrap();
        match &err {
            ProjectError::NoManifest { searched_from } => assert_eq!(searched_from, &searched),
            other => panic!("expected NoManifest, got {other:?}"),
        }
        let text = err.to_string();
        assert!(text.contains(&searched.display().to_string()), "{text}");
        assert!(text.contains("hint: run `byard new"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path that is not there is an `Io` error, with the operating system's
    /// error as its source.
    #[test]
    fn a_missing_path_is_io_and_chains_its_source() {
        let dir = scratch("io");
        let missing = dir.join("nowhere");
        let err = Manifest::discover(Some(&missing)).err().unwrap();
        assert!(
            matches!(&err, ProjectError::Io { path, .. } if path == &missing),
            "{err:?}"
        );
        assert!(err.source().is_some());
        assert!(err.to_string().starts_with(&missing.display().to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A manifest that does not parse carries the line the parser found.
    #[test]
    fn a_manifest_that_does_not_parse_knows_its_line() {
        let dir = scratch("toml-line");
        std::fs::write(dir.join("main.byd"), "View Main() { Text(\"x\") }\n").unwrap();
        std::fs::write(
            dir.join("byard.toml"),
            "[project]\nname = \"x\"\n\nentry = = \"main.byd\"\n",
        )
        .unwrap();
        let err = Manifest::discover(Some(&dir)).err().unwrap();
        match err {
            ProjectError::Manifest { path, line, .. } => {
                assert!(path.ends_with("byard.toml"));
                assert_eq!(line, Some(4));
            }
            other => panic!("expected Manifest, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unsatisfiable requirement is `Resolve`, and its text keeps the
    /// solver's explanation.
    #[test]
    fn an_unsatisfiable_requirement_is_a_resolve_error() {
        let e: ProjectError = ResolveError::Unsatisfiable {
            explanation: "because x 1 depends on y 2".to_string(),
        }
        .into();
        assert!(matches!(
            e,
            ProjectError::Resolve(ResolveError::Unsatisfiable { .. })
        ));
        assert_eq!(
            e.to_string(),
            "no set of versions satisfies every requirement:\nbecause x 1 depends on y 2"
        );
        assert!(e.source().is_some());
    }
}
