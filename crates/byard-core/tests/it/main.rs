//! Every integration test of `byard-core`, in one binary.
//!
//! One file per subject, as before, but compiled and linked once: each
//! `tests/*.rs` used to be its own executable linking the whole engine, and
//! relinking all of them was most of the time an edit spent before a test
//! could run.

mod decorated_texture_pipelines;
mod dev_surface_attribution;
mod error_overlay_layer;
mod incremental_scissor;
mod instrumentation;
mod native_view_paint;
mod paint_transform;
mod wgsl_validation;
