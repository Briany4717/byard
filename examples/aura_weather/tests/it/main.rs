//! Every integration test of `aura_weather`, in one binary.
//!
//! One file per subject, as before, but compiled and linked once: each
//! `tests/*.rs` used to be its own executable linking the whole engine, and
//! relinking all of them was most of the time an edit spent before a test
//! could run.

mod common;
mod forecast;
mod outlook_and_air;
mod shell;
mod trends;
