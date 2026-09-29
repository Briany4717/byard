//! `byard`, the Byard UI framework CLI.
//!
//! See RFC-0006 for the full design rationale.

#![allow(clippy::missing_errors_doc)]

use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod capabilities;
use byard_project::{archive, deps, manifest};
mod commands;
mod hud;
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
        /// One line per diagnostic, with no caret block beneath it, the
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
    /// Render the project to a PNG, headlessly (no window).
    Shot {
        /// The project directory, `byard.toml` or `.byd`. Defaults to the
        /// current project.
        path: Option<PathBuf>,
        /// Where to write the PNG.
        #[arg(short, long, value_name = "PNG", default_value = "shot.png")]
        out: PathBuf,
        /// Logical size, `WIDTHxHEIGHT`.
        #[arg(long, default_value = "440x880", value_parser = parse_size)]
        size: (u32, u32),
        /// Device pixels per logical pixel.
        #[arg(long, default_value_t = 2.0)]
        scale: f32,
        /// Milliseconds of app time to run first, so animations settle.
        #[arg(long, value_name = "MS", default_value_t = 1500)]
        at: u32,
        /// Force the dark scheme.
        #[arg(long, conflicts_with = "light")]
        dark: bool,
        /// Force the light scheme.
        #[arg(long)]
        light: bool,
        /// Tap a logical point before the picture, `X,Y`; repeatable.
        #[arg(long, value_name = "X,Y", value_parser = parse_point)]
        tap: Vec<(f32, f32)>,
    },
    /// Publish the package in this directory to a registry (RFC-0008).
    Publish {
        /// The package directory. Defaults to the current directory.
        package: Option<PathBuf>,
        /// The registry directory. Defaults to `BYARD_REGISTRY`.
        #[arg(long, value_name = "DIR")]
        registry: Option<PathBuf>,
    },
    /// Remove generated artifacts and caches under `.byard/` (RFC-0009 §5).
    Clean {
        /// Path to a `.byd` file or project dir. Defaults to `byard.toml`.
        file: Option<PathBuf>,
    },
}

/// `WIDTHxHEIGHT`, for `byard shot --size`.
fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s
        .split_once('x')
        .ok_or("expected WIDTHxHEIGHT, like 440x880")?;
    Ok((
        w.trim()
            .parse()
            .map_err(|_| format!("`{w}` is not a width"))?,
        h.trim()
            .parse()
            .map_err(|_| format!("`{h}` is not a height"))?,
    ))
}

/// `X,Y`, for `byard shot --tap`.
fn parse_point(s: &str) -> Result<(f32, f32), String> {
    let (x, y) = s.split_once(',').ok_or("expected X,Y, like 120,840")?;
    Ok((
        x.trim()
            .parse()
            .map_err(|_| format!("`{x}` is not a number"))?,
        y.trim()
            .parse()
            .map_err(|_| format!("`{y}` is not a number"))?,
    ))
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
        Command::Shot {
            path,
            out,
            size,
            scale,
            at,
            dark,
            light,
            tap,
        } => commands::shot::run(&commands::shot::ShotArgs {
            path: path.as_deref(),
            out: &out,
            size,
            scale,
            at_ms: at,
            dark: if dark {
                Some(true)
            } else if light {
                Some(false)
            } else {
                None
            },
            taps: &tap,
        }),
        Command::Publish { package, registry } => {
            commands::publish::run(&commands::publish::PublishArgs {
                package: package.as_deref(),
                registry: registry.as_deref(),
            })
        }
    };
    if let Err(e) = result {
        // An empty message is a silent failure sentinel (e.g. `check` already
        // printed rustc-style diagnostics), just set the exit code.
        if !e.is_empty() {
            style::err(&e);
        }
        std::process::exit(1);
    }
}
