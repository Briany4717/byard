//! What an app's `main` can know about an error (RFC-0043): the views that did
//! not compile, as a value, and a way to get at the typed errors that
//! [`ByardError`] carries boxed.

use std::fmt;

use byard_compiler::CompileError;
use byard_compiler::diagnostics::{self, Style};
use byard_compiler::resolve::SourceMap;
use byard_core::ByardError;
use byard_project::ProjectError;

/// The diagnostics of a program that does not compile, with the source they
/// point into.
///
/// It is what [`ByardError::Compile`] holds. [`App::run`](crate::App::run) and
/// [`App::shot`](crate::App::shot) have already printed
/// [`render`](Self::render) to stderr by the time an app has one, so its
/// [`Display`](fmt::Display) is a single line: how many there were.
///
/// ```ignore
/// use byard::ByardErrorExt as _;
///
/// match byard::App::new(env!("CARGO_MANIFEST_DIR")).run() {
///     Ok(()) => {}
///     // Already printed; `report` has each diagnostic with its file and span.
///     Err(e) if e.as_compile().is_some() => std::process::exit(2),
///     Err(e) => {
///         eprintln!("{e}");
///         std::process::exit(1)
///     }
/// }
/// ```
#[derive(Clone)]
pub struct CompileReport {
    errors: Vec<CompileError>,
    source_map: SourceMap,
}

impl CompileReport {
    /// A report of `errors`, which point into `source_map`.
    #[must_use]
    pub fn new(errors: Vec<CompileError>, source_map: SourceMap) -> Self {
        Self { errors, source_map }
    }

    /// Every diagnostic, in the order `byard check` prints them.
    #[must_use]
    pub fn diagnostics(&self) -> &[CompileError] {
        &self.errors
    }

    /// The files the diagnostics' spans point into: use
    /// [`SourceMap::locate`] for a diagnostic's file and local span.
    #[must_use]
    pub fn source_map(&self) -> &SourceMap {
        &self.source_map
    }

    /// How many of the diagnostics are errors; the rest are warnings.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.errors.iter().filter(|e| !e.is_warning()).count()
    }

    /// The full text, as `byard check` prints it and without colour: each
    /// diagnostic's `file:line:col: error[kind]: message` line, then the
    /// source line with a caret under the span.
    #[must_use]
    pub fn render(&self) -> String {
        diagnostics::render(&self.errors, &self.source_map, Style::PLAIN)
    }

    /// Writes [`render`](Self::render) to stderr, coloured when stderr is a
    /// terminal, the way `byard check` does.
    pub(crate) fn eprint(&self) {
        // `eprintln!`, so a test harness that captures output captures this
        // too; std ignores a closed stderr rather than panicking on it.
        eprintln!(
            "{}",
            diagnostics::render(&self.errors, &self.source_map, Style::for_stderr())
        );
    }
}

/// The one-line summary that goes under the diagnostics `run` printed.
impl fmt::Display for CompileReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn count(n: usize, noun: &str) -> String {
            format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
        }
        let errors = self.error_count();
        let warnings = self.errors.len() - errors;
        match (errors, warnings) {
            (e, 0) => write!(f, "{}", count(e, "error")),
            (0, w) => write!(f, "{}", count(w, "warning")),
            (e, w) => write!(f, "{} and {}", count(e, "error"), count(w, "warning")),
        }?;
        write!(f, " in the project's views (shown above)")
    }
}

/// Not the source: a debug print of an error should fit on a line.
impl fmt::Debug for CompileReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompileReport")
            .field(
                "diagnostics",
                &self
                    .errors
                    .iter()
                    .map(CompileError::headline)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl std::error::Error for CompileReport {}

/// Gets the typed errors back out of a [`ByardError`].
///
/// [`ByardError::Project`] and [`ByardError::Compile`] hold a boxed error,
/// because the types behind them live in crates that depend on the engine's.
/// This names them again; bring the trait into scope to call it.
pub trait ByardErrorExt {
    /// The [`ProjectError`] behind a [`ByardError::Project`]: a missing
    /// manifest, a dependency that cannot be fetched, and so on. `None` for
    /// any other error.
    fn as_project(&self) -> Option<&ProjectError>;

    /// The [`CompileReport`] behind a [`ByardError::Compile`]. `None` for any
    /// other error.
    fn as_compile(&self) -> Option<&CompileReport>;
}

impl ByardErrorExt for ByardError {
    fn as_project(&self) -> Option<&ProjectError> {
        match self {
            Self::Project(e) => e.downcast_ref(),
            _ => None,
        }
    }

    fn as_compile(&self) -> Option<&CompileReport> {
        match self {
            Self::Compile(e) => e.downcast_ref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use byard_compiler::Span;

    use super::*;

    fn error() -> CompileError {
        CompileError::Project {
            span: Span::new(0, 0),
            message: "boom".to_string(),
        }
    }

    fn warning() -> CompileError {
        CompileError::StrokeInFusionGroup {
            span: Span::new(0, 0),
            param: "stroke".to_string(),
        }
    }

    /// The line under the diagnostics counts what it counted, in the plural
    /// where the count is.
    #[test]
    fn the_summary_counts_errors_and_warnings() {
        let say = |errors| CompileReport::new(errors, SourceMap::default()).to_string();
        assert_eq!(
            say(vec![error()]),
            "1 error in the project's views (shown above)"
        );
        assert_eq!(
            say(vec![error(), error()]),
            "2 errors in the project's views (shown above)"
        );
        assert_eq!(
            say(vec![warning()]),
            "1 warning in the project's views (shown above)"
        );
        assert_eq!(
            say(vec![error(), warning(), warning()]),
            "1 error and 2 warnings in the project's views (shown above)"
        );
    }

    /// `ByardErrorExt` gives back what went in, and only for its own variant.
    #[test]
    fn the_extension_names_the_boxed_types() {
        let compile = ByardError::compile(CompileReport::new(vec![error()], SourceMap::default()));
        assert_eq!(
            compile.as_compile().map(CompileReport::error_count),
            Some(1)
        );
        assert!(compile.as_project().is_none());

        let project = ByardError::from(ProjectError::NoManifest {
            searched_from: "/somewhere".into(),
        });
        assert!(matches!(
            project.as_project(),
            Some(ProjectError::NoManifest { .. })
        ));
        assert!(project.as_compile().is_none());

        assert!(ByardError::UnsupportedBackend.as_project().is_none());
        assert!(ByardError::Platform("x".into()).as_compile().is_none());
    }

    /// A debug print of a report is the headlines, not the source it points
    /// into.
    #[test]
    fn debug_does_not_dump_the_source() {
        let report = CompileReport::new(vec![error()], SourceMap::default());
        assert_eq!(
            format!("{report:?}"),
            "CompileReport { diagnostics: [\"boom\"], .. }"
        );
    }
}
