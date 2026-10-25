//! # byard-core
//!
//! The engine core of the Byard UI framework.
//!
//! This crate contains the four subsystems that compose the rendering engine:
//!
//! - [`evaluator`], Reactive state (`Signal<T>`), per-view memory arenas (`ViewArena`),
//!   and dirty-flag collection.
//! - [`atlas`], Layout computation via Taffy and spatial hit-testing via a hash grid.
//! - [`encoder`], Multi-pipeline `wgpu` command dispatch. It draws `SolidBox`,
//!   `DecoratedBox` (gradients, shadows, borders), `Ripple`, `CanvasShape`,
//!   `CanvasFill` (filled paths), `TextureSampler` (images), `VectorMSDF`
//!   (icons and vector glyphs) and `TextGlyph` (text) in one UI pass, plus the
//!   backdrop blur and opacity-group passes, and any pipeline an app registers
//!   through [`render`] (RFC-0039).
//! - [`relay`], Thread management, double-buffered frame swap, and async I/O pool.
//!
//! Cross-subsystem communication goes exclusively through the types defined in
//! [`frame`]. No subsystem module imports from another subsystem directly.
//!
//! ```text
//! encoder  ──┐
//! atlas    ──┤─→  frame  ←─  relay
//! evaluator ─┘
//! ```

pub mod atlas;
pub mod bridge;
pub mod cap;
pub mod clipboard;
pub mod color;
pub mod encoder;
pub mod engine;
pub mod evaluator;
pub mod frame;
pub mod platform;
pub mod relay;
pub mod render;
pub mod telemetry;
pub mod text;

pub use clipboard::{Clipboard, MemoryClipboard};
pub use encoder::BoxInstance;
pub use encoder::text_glyph::TextLine;
pub use engine::{Census, Engine};
pub use platform::{
    EventKind, ImeEvent, InputEvent, InputPayload, KeyModifiers, PlatformHost, PointerButton,
    PointerState, WindowSize,
};

/// Trait representing a logic tick execution runner.
pub trait LogicRuntime {
    /// Evaluates a single logic tick, updating the `RenderFrame` and processing dirty target IDs.
    fn evaluate_tick(
        &mut self,
        frame: &mut frame::RenderFrame,
        input_events: &[InputEvent],
        dirty_targets: &[frame::TargetId],
    );

    /// Applies everything the async I/O pool completed since the last tick,
    /// **before** input is processed and before the pull (tick step 0,
    /// RFC-0028 §6). Each payload is type-erased; the implementation downcasts
    /// the shapes it understands and drops the rest (never a panic).
    ///
    /// Returns whether any result actually changed state. The relay uses that
    /// answer to decide whether to wake a `Wait`-mode render loop: a reply
    /// nobody was waiting for should not cost a repaint, and one that wrote a
    /// `var` must not go unseen (RFC-0029 §2).
    ///
    /// Defaulted to "nothing applied" so a runtime with no controllers, every
    /// existing one, is unaffected.
    fn apply_io_results(&mut self, _results: Vec<relay::IoResult>) -> bool {
        false
    }

    /// Whether the frame just produced is not the last word: something is
    /// still moving, or this tick changed state that only the next tick can
    /// show (an `on measure` that wrote a `var` after layout). The relay then
    /// wakes a `Wait`-mode render loop and ticks again without parking, as
    /// it does for input, so the change appears without the user having to
    /// move the mouse.
    ///
    /// Defaulted to `false`, so a runtime that never animates idles as
    /// before.
    fn wants_another_frame(&self) -> bool {
        false
    }
}

use std::fmt;

/// Errors produced by the Byard engine.
///
/// This enum is `#[non_exhaustive]`, new variants may be added in future
/// releases without breaking downstream code.
#[non_exhaustive]
pub enum ByardError {
    /// A render pipeline failed to compile during initialisation.
    PipelineCompilation {
        /// Name of the pipeline that failed (e.g. `"SolidBox"`).
        pipeline: String,
        /// The underlying error message from `wgpu`.
        reason: String,
    },

    /// The GPU backend does not meet Byard's minimum requirements.
    UnsupportedBackend,

    /// An error from the Atlas subsystem (layout, hit-testing).
    Layout(crate::atlas::AtlasError),

    /// An unrecoverable error occurred while acquiring or presenting a surface texture.
    ///
    /// Transient surface losses (e.g. window minimise/restore) are handled
    /// transparently by [`Engine::render_latest`] and never produce this variant.
    RenderSurface(String),

    /// [`glyphon`] failed to upload shaped glyphs to the GPU atlas.
    ///
    /// The inner string is the original [`glyphon::PrepareError`] message.
    TextPrepare(String),

    /// [`glyphon`] failed to record text draw calls into the render pass.
    ///
    /// The inner string is the original [`glyphon::RenderError`] message.
    TextRender(String),

    /// The OS refused to spawn a thread (e.g. [`relay::Relay`]'s logic thread).
    ///
    /// The inner string is the original [`std::io::Error`] message. Per
    /// RFC-0001 §8, thread-spawn failure is a recoverable condition, not a
    /// panic, the caller decides whether to retry or abort startup.
    ThreadSpawn(String),

    /// The async I/O Tokio runtime failed to initialise.
    ///
    /// The inner string is the original [`std::io::Error`] message produced
    /// by [`tokio::runtime::Builder::build`].
    RuntimeCreation(String),

    /// A [`platform::PlatformHost`] implementation failed to create its
    /// window, `wgpu` surface, or event loop.
    ///
    /// The inner string is the original platform-specific error message
    /// (e.g. from `winit::event_loop::EventLoop::new` or
    /// `wgpu::Instance::create_surface`). `byard-core` never constructs this
    /// variant itself, it exists so host crates (e.g. `byard-platform`)
    /// have a `ByardError` to return from [`platform::PlatformHost::on_resume`]
    /// without inventing their own error type.
    Platform(String),

    /// The project on disk cannot be used: no `byard.toml`, one that does not
    /// parse, a dependency that cannot be resolved or fetched (RFC-0043).
    ///
    /// Boxed because the type behind it, `byard_project::ProjectError`, is
    /// defined in a crate that depends on this one. The `byard` crate's
    /// `ByardErrorExt::as_project` gives it back by name, so code can tell a
    /// broken project from a broken GPU without matching on text.
    Project(Box<dyn std::error::Error + Send + Sync>),

    /// The project's views do not compile (RFC-0043).
    ///
    /// By the time an app sees this, `App::run` and `Shot::shot` have already
    /// printed the diagnostics, with the source line and the suggestion, so
    /// its text is one line saying how many there were. Boxed for the reason
    /// [`Project`](Self::Project) is; `ByardErrorExt::as_compile` gives back
    /// the `byard::CompileReport`, which holds each diagnostic with its file
    /// and span.
    Compile(Box<dyn std::error::Error + Send + Sync>),
}

impl ByardError {
    /// A [`Project`](Self::Project) error holding `error`.
    #[must_use]
    pub fn project(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Project(Box::new(error))
    }

    /// A [`Compile`](Self::Compile) error holding `report`.
    #[must_use]
    pub fn compile(report: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Compile(Box::new(report))
    }
}

impl fmt::Display for ByardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PipelineCompilation { pipeline, reason } => {
                write!(f, "pipeline '{pipeline}' failed to compile: {reason}")
            }
            Self::UnsupportedBackend => write!(f, "no compatible wgpu backend found"),
            Self::Layout(e) => write!(f, "{e}"),
            Self::RenderSurface(msg) => write!(f, "surface error: {msg}"),
            Self::TextPrepare(msg) => write!(f, "text prepare error: {msg}"),
            Self::TextRender(msg) => write!(f, "text render error: {msg}"),
            Self::ThreadSpawn(msg) => write!(f, "failed to spawn thread: {msg}"),
            Self::RuntimeCreation(msg) => write!(f, "failed to create async runtime: {msg}"),
            Self::Platform(msg) => write!(f, "platform host error: {msg}"),
            Self::Project(e) | Self::Compile(e) => write!(f, "{e}"),
        }
    }
}

/// What `fn main() -> Result<(), ByardError>` prints: the runtime shows an
/// error returned from `main` with `Debug`, not `Display`.
///
/// The two variants that carry a message written for a person
/// ([`Project`](ByardError::Project), [`Compile`](ByardError::Compile)) print
/// it as it reads, so `Error: 1 error in the project's views (shown above)`
/// is what lands under the diagnostics `App::run` printed, rather than the
/// enum's name wrapped round a boxed value. Every other variant is formatted
/// as `#[derive(Debug)]` formats it.
impl fmt::Debug for ByardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PipelineCompilation { pipeline, reason } => f
                .debug_struct("PipelineCompilation")
                .field("pipeline", pipeline)
                .field("reason", reason)
                .finish(),
            Self::UnsupportedBackend => f.write_str("UnsupportedBackend"),
            Self::Layout(e) => f.debug_tuple("Layout").field(e).finish(),
            Self::RenderSurface(m) => f.debug_tuple("RenderSurface").field(m).finish(),
            Self::TextPrepare(m) => f.debug_tuple("TextPrepare").field(m).finish(),
            Self::TextRender(m) => f.debug_tuple("TextRender").field(m).finish(),
            Self::ThreadSpawn(m) => f.debug_tuple("ThreadSpawn").field(m).finish(),
            Self::RuntimeCreation(m) => f.debug_tuple("RuntimeCreation").field(m).finish(),
            Self::Platform(m) => f.debug_tuple("Platform").field(m).finish(),
            Self::Project(e) | Self::Compile(e) => write!(f, "{e}"),
        }
    }
}

impl From<crate::atlas::AtlasError> for ByardError {
    fn from(e: crate::atlas::AtlasError) -> Self {
        Self::Layout(e)
    }
}

impl std::error::Error for ByardError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Layout(e) => Some(e),
            // Transparent: `Display` already is the inner error's text, so
            // the chain continues from the inner error's own source, or a
            // reporter would print the same message twice.
            Self::Project(e) | Self::Compile(e) => e.source(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byard_error_layout_chains_source() {
        use crate::atlas::AtlasError;
        use std::error::Error;

        let inner_msg = "backend exploded".to_string();
        let atlas_err = AtlasError::Backend(inner_msg);
        let byard_err: ByardError = atlas_err.into();

        assert!(
            byard_err.source().is_some(),
            "Layout variant must expose source"
        );
        assert_eq!(
            byard_err.to_string(),
            "layout backend error: backend exploded"
        );
    }

    #[test]
    fn byard_error_pipeline_has_no_source() {
        use std::error::Error;

        let err = ByardError::PipelineCompilation {
            pipeline: "SolidBox".to_string(),
            reason: "bad shader".to_string(),
        };
        assert!(err.source().is_none());
    }

    #[test]
    fn byard_error_unsupported_backend_display_and_no_source() {
        use std::error::Error;

        let err = ByardError::UnsupportedBackend;
        assert_eq!(err.to_string(), "no compatible wgpu backend found");
        assert!(err.source().is_none());
    }

    #[test]
    fn byard_error_render_surface_display_and_no_source() {
        use std::error::Error;

        let msg = "out of memory on present".to_string();
        let err = ByardError::RenderSurface(msg.clone());
        assert_eq!(err.to_string(), format!("surface error: {msg}"));
        assert!(err.source().is_none());
    }

    #[test]
    fn byard_error_pipeline_display() {
        let err = ByardError::PipelineCompilation {
            pipeline: "SolidBox".to_string(),
            reason: "bad shader".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "pipeline 'SolidBox' failed to compile: bad shader"
        );
    }

    #[test]
    fn byard_error_is_debug() {
        // Debug must not panic for any variant.
        let _ = format!("{:?}", ByardError::UnsupportedBackend);
        let _ = format!(
            "{:?}",
            ByardError::PipelineCompilation {
                pipeline: "x".into(),
                reason: "y".into(),
            }
        );
        let _ = format!("{:?}", ByardError::RenderSurface("z".into()));
    }

    /// A stand-in for a type a higher crate defines: a message, and an
    /// error under it.
    #[derive(Debug)]
    struct Outer;
    impl fmt::Display for Outer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "1 error in the project's views (shown above)")
        }
    }
    impl std::error::Error for Outer {}

    /// RFC-0043: the new variants read as the message their payload carries;
    /// they are transparent, so the source chain skips them.
    #[test]
    fn project_and_compile_read_as_their_payload() {
        use std::error::Error;

        let project = ByardError::project(std::io::Error::other("no manifest here"));
        assert_eq!(project.to_string(), "no manifest here");
        // Transparent: the text is the inner error's, so the chain skips it.
        assert!(project.source().is_none());
        assert!(matches!(project, ByardError::Project(_)));

        let compile = ByardError::compile(Outer);
        assert_eq!(
            compile.to_string(),
            "1 error in the project's views (shown above)"
        );
        assert!(matches!(compile, ByardError::Compile(_)));
        // The boxed value is the one that went in: a caller can downcast it.
        let ByardError::Compile(inner) = &compile else {
            unreachable!()
        };
        assert!(inner.downcast_ref::<Outer>().is_some());
    }

    /// `fn main() -> Result<(), ByardError>` prints `Debug`. For the two
    /// variants made for a person that is the message, as it reads; every
    /// other variant is formatted as `#[derive(Debug)]` formats it.
    #[test]
    fn debug_is_the_message_for_project_and_compile_and_derived_for_the_rest() {
        let compile = ByardError::compile(Outer);
        assert_eq!(
            format!("{compile:?}"),
            "1 error in the project's views (shown above)"
        );
        let project = ByardError::project(std::io::Error::other("no manifest here"));
        assert_eq!(format!("{project:?}"), "no manifest here");

        assert_eq!(
            format!("{:?}", ByardError::UnsupportedBackend),
            "UnsupportedBackend"
        );
        assert_eq!(
            format!("{:?}", ByardError::Platform("window".into())),
            "Platform(\"window\")"
        );
        assert_eq!(
            format!("{:#?}", ByardError::RenderSurface("lost".into())),
            "RenderSurface(\n    \"lost\",\n)"
        );
        assert_eq!(
            format!(
                "{:?}",
                ByardError::PipelineCompilation {
                    pipeline: "SolidBox".into(),
                    reason: "bad".into(),
                }
            ),
            "PipelineCompilation { pipeline: \"SolidBox\", reason: \"bad\" }"
        );
    }

    /// Errors are returned from every frame's `Result`; the two boxed variants
    /// hold a fat pointer each, which fits in the largest variant that was
    /// already there (two `String`s), so the enum is no bigger for them.
    #[test]
    fn the_new_variants_do_not_grow_the_error() {
        assert_eq!(
            std::mem::size_of::<ByardError>(),
            2 * std::mem::size_of::<String>()
        );
    }
}
