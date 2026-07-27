//! `byard` — the Byard UI framework CLI.
//!
//! See RFC-0006 for the full design rationale.

#![allow(clippy::missing_errors_doc)]

use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod commands;
mod deps;
mod manifest;
mod statusline;
mod style;
mod telemetry_overlay;
mod trace;

#[derive(Parser)]
#[command(
    name = "byard",
    version,
    about = "The Byard UI framework CLI",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a new Byard project.
    New {
        /// Name of the project (used as the directory name).
        name: String,
    },
    /// Start the dev window with live hot-reload.
    Dev {
        /// Path to a `.byd` file. Defaults to `entry` in `byard.toml`.
        file: Option<PathBuf>,
        /// Deliver a deep-link URL at startup, as an OS intent would
        /// (RFC-0026): `--deep-link byard://item/42`.
        #[arg(long, value_name = "URL")]
        deep_link: Option<String>,
        /// Write a Chrome Trace Event file (RFC-0030 §V5). Open it in
        /// Perfetto, `chrome://tracing` or speedscope for a flame chart.
        #[arg(long, value_name = "PATH")]
        trace: Option<PathBuf>,
        /// Start with the expanded per-scope profile block instead of the
        /// one-line statusline (RFC-0030 §V1). Toggle at runtime with
        /// `Mod+Shift+P`.
        #[arg(long)]
        profile: bool,
    },
    /// Parse and validate without opening a window (CI-friendly).
    Check {
        /// One line per diagnostic, with no caret block beneath it — the
        /// pre-RFC-0030 shape, for scripts (RFC-0006 C3).
        #[arg(long)]
        short: bool,
        /// Path to a `.byd` file. Defaults to `entry` in `byard.toml`.
        file: Option<PathBuf>,
    },
    /// Bake the AOT vector atlas for the project (RFC-0009 §4).
    Build {
        /// Path to a `.byd` file or project dir. Defaults to `byard.toml`.
        file: Option<PathBuf>,
    },
    /// Add a dependency to byard.toml, then fetch and lock it (RFC-0008).
    #[command(alias = "install")]
    Add {
        /// Package name (a byld identifier, e.g. `material`).
        name: String,
        /// Use a local directory as the package source.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Use a git repository as the package source.
        #[arg(long)]
        git: Option<String>,
        /// Pin the git source to a tag.
        #[arg(long, conflicts_with = "rev")]
        tag: Option<String>,
        /// Pin the git source to an exact commit.
        #[arg(long)]
        rev: Option<String>,
    },
    /// Fetch dependencies and write byard.lock (the only lock writer).
    Get,
    /// Remove generated artifacts and caches under `.byard/` (RFC-0009 §5).
    Clean {
        /// Path to a `.byd` file or project dir. Defaults to `byard.toml`.
        file: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::New { name } => commands::new::run(&name),
        Command::Dev {
            file,
            deep_link,
            trace,
            profile,
        } => commands::dev::run(commands::dev::Options {
            file: file.as_deref(),
            deep_link: deep_link.as_deref(),
            trace: trace.as_deref(),
            profile,
        }),
        Command::Check { file, short } => commands::check::run(file.as_deref(), short),
        Command::Build { file } => commands::build::run(file.as_deref()),
        Command::Clean { file } => commands::clean::run(file.as_deref()),
        Command::Add {
            name,
            path,
            git,
            tag,
            rev,
        } => commands::add::run(&commands::add::AddArgs {
            name: &name,
            path: path.as_deref(),
            git: git.as_deref(),
            tag: tag.as_deref(),
            rev: rev.as_deref(),
        }),
        Command::Get => commands::get::run(),
    };
    if let Err(e) = result {
        // An empty message is a silent failure sentinel (e.g. `check` already
        // printed rustc-style diagnostics) — just set the exit code.
        if !e.is_empty() {
            style::err(&e);
        }
        std::process::exit(1);
    }
}
