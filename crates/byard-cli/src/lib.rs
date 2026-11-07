//! `byard`, the Byard UI framework CLI.
//!
//! The command is installed with `cargo install byard`: the `byard` crate's
//! binary calls [`main`]. This crate is that command as a library, so the
//! façade can carry it without a second copy, and so this repository's own
//! tests can run it as `byard-cli`.
//!
//! See RFC-0006 for the full design rationale.

#![allow(clippy::missing_errors_doc)]

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

mod capabilities;
use byard_project::{archive, deps, manifest};
mod commands;
mod hud;
mod iconfont;
mod statusline;
mod style;
mod telemetry_overlay;

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
    /// Format `.byd` files in place, with the one layout there is.
    Fmt {
        /// Files, or directories to format. A project directory covers its
        /// entry directory and its path dependencies; any other directory,
        /// every `.byd` under it. Defaults to the current project.
        paths: Vec<PathBuf>,
        /// Change nothing: list the files that are not formatted, and exit
        /// 1 if there are any.
        #[arg(long)]
        check: bool,
    },
    /// Bake the AOT vector atlas for the project (RFC-0009 §4).
    Build {
        /// Path to a `.byd` file or project dir. Defaults to `byard.toml`.
        file: Option<PathBuf>,
    },
    /// Package a built app with its project, to run where its sources are
    /// not: a `.app` on macOS, a directory elsewhere, or with `--layout
    /// linux` a freedesktop.org prefix with a launcher entry and icons.
    Bundle {
        /// The project directory or `byard.toml`. Defaults to the current
        /// project.
        path: Option<PathBuf>,
        /// The app's executable, built (`cargo build --release`).
        #[arg(long, value_name = "EXE")]
        bin: PathBuf,
        /// Where to write the bundle. Defaults to the project's
        /// `.byard/bundle/`.
        #[arg(short, long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// The package's shape: `app`, `dir` or `linux`. Defaults to `app`
        /// on macOS and `dir` elsewhere.
        #[arg(long, value_enum)]
        layout: Option<commands::bundle::Layout>,
        /// Also pack the Linux layout as `<exe>-<version>-linux-<arch>.tar.gz`
        /// beside it, to unpack into a prefix (`tar xzf ... -C ~/.local`).
        #[arg(long)]
        archive: bool,
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
        /// Use a registry, a directory or an `http(s)://` URL.
        #[arg(long)]
        registry: Option<String>,
        /// The version requirement for `--registry` (Cargo's rules, e.g.
        /// `^0.3`). Defaults to `^` the newest version published.
        #[arg(long)]
        version: Option<String>,
    },
    /// Fetch dependencies and write byard.lock (the only lock writer).
    Get,
    /// Render the project to a PNG, headlessly (no window).
    Shot {
        /// The project directory, `byard.toml` or `.byd`. Defaults to the
        /// current project.
        path: Option<PathBuf>,
        /// Draw this view instead of `Main`. It must take no required
        /// parameters.
        #[arg(long, value_name = "NAME")]
        view: Option<String>,
        /// Where to write the PNG. Defaults to `shot.png`, unless `--a11y` is
        /// given: then no picture is drawn unless this asks for one.
        #[arg(short, long, value_name = "PNG")]
        out: Option<PathBuf>,
        /// Write the accessibility tree to a text file (role, name, value,
        /// state and bounds, one node a line; a control a screen reader could
        /// not name is marked `(unnamed)`).
        #[arg(long, value_name = "FILE")]
        a11y: Option<PathBuf>,
        /// Logical size, `WIDTHxHEIGHT`.
        #[arg(long, default_value = "440x880", value_parser = parse_size)]
        size: (u32, u32),
        /// Device pixels per logical pixel.
        #[arg(long, default_value_t = 2.0)]
        scale: f32,
        /// Milliseconds of app time to run first, so animations settle.
        #[arg(long, value_name = "MS", default_value_t = 1500)]
        at: u32,
        /// Fix the wall clock `now()` reads at this ISO time, such as
        /// `2026-11-05T09:00:00`. `--at` still advances it.
        #[arg(long, value_name = "ISO")]
        now: Option<String>,
        /// Force the dark scheme.
        #[arg(long, conflicts_with = "light")]
        dark: bool,
        /// Force the light scheme.
        #[arg(long)]
        light: bool,
        /// Tap a logical point before the picture, `X,Y`; repeatable.
        #[arg(long, value_name = "X,Y", value_parser = parse_point)]
        tap: Vec<(f32, f32)>,
        /// Press at a point and keep the pointer down for the picture, `X,Y`,
        /// so `on pressed`, a press morph and a ripple show; use `--at` for
        /// how long it is held. Repeatable, in order with `--tap`.
        #[arg(long, value_name = "X,Y", value_parser = parse_point)]
        hold: Vec<(f32, f32)>,
        /// Move the pointer to a point without pressing, `X,Y`, so
        /// `on hover` shows. Repeatable, in order with `--tap`.
        #[arg(long, value_name = "X,Y", value_parser = parse_point)]
        hover: Vec<(f32, f32)>,
        /// Press a key before the picture, like `Left`, `shift+Left` or
        /// `mod+a`; repeatable. Applied in order with `--tap` and `--type`.
        #[arg(long, value_name = "KEY", value_parser = parse_key)]
        key: Vec<commands::shot::Key>,
        /// Type text before the picture; repeatable. Applied in order with
        /// `--tap` and `--key`.
        #[arg(long = "type", value_name = "TEXT")]
        type_text: Vec<String>,
        /// Let this many milliseconds of app time pass before the next input
        /// (or the picture); repeatable, in order with `--tap`.
        #[arg(long, value_name = "MS")]
        wait: Vec<u32>,
    },
    /// Run the project's scenario files, `tests/*.toml`, without a window or
    /// a GPU (RFC-0058).
    Test {
        /// The project directory or `byard.toml`. Defaults to the current
        /// project.
        path: Option<PathBuf>,
        /// Run only the tests whose `file::name` contains this text.
        #[arg(long, value_name = "TEXT")]
        filter: Option<String>,
        /// Also write each test's final picture to this directory, when
        /// there is a GPU.
        #[arg(long, value_name = "DIR")]
        shots: Option<PathBuf>,
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

/// A key chord, for `byard shot --key`.
fn parse_key(s: &str) -> Result<commands::shot::Key, String> {
    let (mods, name) = byard_core::KeyModifiers::parse_chord(s)?;
    Ok(commands::shot::Key { name, mods })
}

/// The taps, keys and typed text of `byard shot`, in the order they were
/// written on the command line. clap collects each flag into its own list, so
/// the order between lists is read back from the positions it recorded.
fn shot_steps(
    matches: &clap::ArgMatches,
    taps: &[(f32, f32)],
    holds: &[(f32, f32)],
    hovers: &[(f32, f32)],
    keys: &[commands::shot::Key],
    typed: &[String],
    waits: &[u32],
) -> Vec<commands::shot::Step> {
    use commands::shot::Step;
    let Some(shot) = matches.subcommand_matches("shot") else {
        return Vec::new();
    };
    let at = |id: &str| -> Vec<usize> {
        shot.indices_of(id)
            .map(Iterator::collect)
            .unwrap_or_default()
    };
    let mut steps: Vec<(usize, Step)> = Vec::new();
    steps.extend(
        at("tap")
            .into_iter()
            .zip(taps)
            .map(|(i, &(x, y))| (i, Step::Tap(x, y))),
    );
    steps.extend(
        at("hold")
            .into_iter()
            .zip(holds)
            .map(|(i, &(x, y))| (i, Step::Hold(x, y))),
    );
    steps.extend(
        at("hover")
            .into_iter()
            .zip(hovers)
            .map(|(i, &(x, y))| (i, Step::Hover(x, y))),
    );
    steps.extend(
        at("key")
            .into_iter()
            .zip(keys)
            .map(|(i, k)| (i, Step::Key(k.clone()))),
    );
    steps.extend(
        at("type_text")
            .into_iter()
            .zip(typed)
            .map(|(i, t)| (i, Step::Type(t.clone()))),
    );
    steps.extend(
        at("wait")
            .into_iter()
            .zip(waits)
            .map(|(i, &ms)| (i, Step::Wait(ms))),
    );
    steps.sort_by_key(|(i, _)| *i);
    steps.into_iter().map(|(_, s)| s).collect()
}

/// Runs the `byard` command with the process's arguments, and exits with its
/// status. What the `byard` binary's `main` is.
#[allow(clippy::too_many_lines)]
pub fn main() {
    let matches = <Cli as clap::CommandFactory>::command().get_matches();
    let cli =
        <Cli as clap::FromArgMatches>::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
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
        Command::Fmt { paths, check } => commands::fmt::run(&paths, check),
        Command::Build { file } => commands::build::run(file.as_deref()),
        Command::Bundle {
            path,
            bin,
            out,
            layout,
            archive,
        } => commands::bundle::run(
            path.as_deref(),
            &bin,
            out.as_deref(),
            layout.unwrap_or_else(commands::bundle::Layout::host),
            archive,
        ),
        Command::Clean { file } => commands::clean::run(file.as_deref()),
        Command::Add {
            name,
            path,
            git,
            tag,
            rev,
            registry,
            version,
        } => commands::add::run(&commands::add::AddArgs {
            name: &name,
            path: path.as_deref(),
            git: git.as_deref(),
            tag: tag.as_deref(),
            rev: rev.as_deref(),
            registry: registry.as_deref(),
            version: version.as_deref(),
        }),
        Command::Get => commands::get::run(),
        Command::Shot {
            path,
            view,
            out,
            a11y,
            size,
            scale,
            at,
            now,
            dark,
            light,
            tap,
            hold,
            hover,
            key,
            type_text,
            wait,
        } => commands::shot::run(&commands::shot::ShotArgs {
            path: path.as_deref(),
            view: view.as_deref(),
            out: match (out.as_deref(), a11y.is_some()) {
                (Some(out), _) => Some(out),
                (None, true) => None,
                (None, false) => Some(Path::new("shot.png")),
            },
            a11y: a11y.as_deref(),
            size,
            scale,
            at_ms: at,
            wall_clock: now.as_deref(),
            dark: if dark {
                Some(true)
            } else if light {
                Some(false)
            } else {
                None
            },
            steps: &shot_steps(&matches, &tap, &hold, &hover, &key, &type_text, &wait),
        }),
        Command::Test {
            path,
            filter,
            shots,
        } => commands::test::run(&commands::test::TestArgs {
            path: path.as_deref(),
            filter: filter.as_deref(),
            shots: shots.as_deref(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use commands::shot::Step;

    /// The steps `byard shot` would take for these arguments.
    fn steps_of(args: &[&str]) -> Vec<Step> {
        let matches = <Cli as clap::CommandFactory>::command()
            .try_get_matches_from(std::iter::once("byard").chain(args.iter().copied()))
            .expect("arguments clap accepts");
        let cli = <Cli as clap::FromArgMatches>::from_arg_matches(&matches).unwrap();
        let Command::Shot {
            tap,
            hold,
            hover,
            key,
            type_text,
            wait,
            ..
        } = cli.command
        else {
            panic!("a shot");
        };
        shot_steps(&matches, &tap, &hold, &hover, &key, &type_text, &wait)
    }

    fn key(chord: &str) -> Step {
        Step::Key(parse_key(chord).unwrap())
    }

    /// RFC-0041 §7: taps, keys and typed text run in the order they were
    /// written, whichever flag each one is, which clap alone would lose (it
    /// collects each flag into a list of its own).
    #[test]
    fn taps_keys_and_typed_text_run_in_the_order_they_were_written() {
        let steps = steps_of(&[
            "shot",
            "--tap",
            "200,120",
            "--type",
            "hello",
            "--key",
            "Left",
            "--key",
            "shift+Left",
            "--tap",
            "10,20",
            "--type",
            "!",
            "--key",
            "mod+a",
        ]);
        assert_eq!(
            steps,
            [
                Step::Tap(200.0, 120.0),
                Step::Type("hello".into()),
                key("Left"),
                key("shift+Left"),
                Step::Tap(10.0, 20.0),
                Step::Type("!".into()),
                key("mod+a"),
            ]
        );
    }

    /// RFC-0056: a held press and a hover are steps like a tap, in the order
    /// written.
    #[test]
    fn hold_and_hover_are_steps_in_the_order_written() {
        let steps = steps_of(&[
            "shot", "--hover", "5,6", "--hold", "200,120", "--tap", "1,2", "--hover", "7,8",
        ]);
        assert_eq!(
            steps,
            [
                Step::Hover(5.0, 6.0),
                Step::Hold(200.0, 120.0),
                Step::Tap(1.0, 2.0),
                Step::Hover(7.0, 8.0),
            ]
        );
    }

    /// RFC-0058: a wait is a step in the order written.
    #[test]
    fn a_wait_is_a_step_in_the_order_written() {
        assert_eq!(
            steps_of(&["shot", "--tap", "1,2", "--wait", "300", "--key", "Enter"]),
            [Step::Tap(1.0, 2.0), Step::Wait(300), key("Enter")]
        );
    }

    #[test]
    fn a_shot_with_no_steps_has_none() {
        assert_eq!(steps_of(&["shot"]).len(), 0);
    }

    #[test]
    fn a_key_names_the_platforms_key_and_what_is_held() {
        let Step::Key(k) = key("shift+Left") else {
            panic!("a key");
        };
        assert_eq!(k.name, "ArrowLeft");
        assert!(k.mods.shift && !k.mods.ctrl);
        assert!(parse_key("shift+Flibber").is_err());
    }
}
