//! The `byard` façade crate: the single dependency an application adds to write
//! the Rust half of a two-layer Byard app (RFC-0001 §1, RFC-0028 §2).
//!
//! It re-exports the controller boundary from `byard-core` and the
//! `#[byard_controller]` attribute from `byard-macro`, so the macro's generated
//! code, which references `::byard::bridge::*`, resolves against the *app's*
//! dependency graph (`app → byard → byard-core`, a forward edge; INV-1 holds:
//! `byard-macro` still has no dependency on core/compiler).
//!
//! ```ignore
//! use byard::bridge::HostValue;
//!
//! #[byard::byard_controller]
//! #[derive(Clone)]
//! struct WeatherApi { base_url: String }
//!
//! #[byard::byard_controller]
//! impl WeatherApi {
//!     async fn forecast(&self, city: String) -> Result<HostValue, HostValue> {
//!         Ok(HostValue::Record(vec![("city".into(), HostValue::Str(city))]))
//!     }
//! }
//! ```

pub mod app;

pub use app::App;

/// Re-exported so an app's `main` can name the error `App::run` returns
/// without depending on `byard-core` directly.
pub use byard_core::ByardError;

/// The controller boundary: [`HostValue`](bridge::HostValue), the
/// [`Controller`](bridge::Controller) trait, [`ControllerRegistry`](bridge::ControllerRegistry),
/// the `From`/`IntoHostValue` conversions, and the reply/timer wire types.
pub use byard_core::bridge;

/// Marks a Rust struct as a byard controller (field metadata) and, on its `impl`
/// block, generates the async-method dispatch shim (`impl Controller`).
pub use byard_macro::byard_controller;

/// The native render extension ABI (RFC-0039): [`NativeView`](render::NativeView)
/// and the bounded [`RenderCtx`](render::RenderCtx) it draws through, plus the
/// catalog types `#[native_view]` generates.
pub use byard_core::render;

/// Marks a Rust struct as a native view (RFC-0039): generates its catalog entry
/// and the prop assignment behind it, leaving the drawing to the author.
pub use byard_macro::native_view;
