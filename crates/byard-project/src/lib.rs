//! A Byard project on disk (RFC-0008, RFC-0022): its `byard.toml`, the theme
//! and fonts it declares, its dependencies and their packages, the lockfile,
//! and the archives `byard publish` writes.
//!
//! One library rather than code inside the `byard` command-line tool, because
//! two things need it: the development runner, and a shipped application
//! (`byard::App::project`). A project that read its theme, fonts and packages
//! in `byard dev` and none of them once shipped would be two different
//! programs wearing one name.

pub mod archive;
pub mod deps;
pub mod manifest;
