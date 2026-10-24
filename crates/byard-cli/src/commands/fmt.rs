//! `byard fmt [path]… [--check]`, format `.byd` files in place (RFC-0044).
//!
//! No options but `--check`: the layout is `byard_compiler::fmt`'s and is the
//! same one the language server applies on save. A file that does not parse,
//! or that the formatter cannot prove it keeps the meaning of, is reported
//! and left as it was; the others are still formatted.

use crate::deps::{byd_files_under, owned_source_files};
use crate::manifest::Manifest;
use crate::style;
use std::path::{Path, PathBuf};

/// Formats the project in the current directory, or `paths`: a `.byd` file,
/// a project directory (its entry directory and its path dependencies), or
/// any other directory (every `.byd` under it).
///
/// With `check`, writes nothing: lists the files that are not formatted on
/// stdout, one per line, and fails if there are any.
pub fn run(paths: &[PathBuf], check: bool) -> Result<(), String> {
    let started = std::time::Instant::now();
    let files = collect(paths)?;
    if files.is_empty() {
        return Err("no `.byd` files found".to_string());
    }

    // The files are canonical, so the directory they are shown against has to
    // be too: on Windows a canonical path is a verbatim `\\?\C:\...` one, which
    // a plain `current_dir` is never a prefix of.
    let cwd = std::env::current_dir()
        .ok()
        .map(|c| c.canonicalize().unwrap_or(c));
    let shown = |p: &Path| -> String {
        let shown = cwd
            .as_deref()
            .and_then(|c| p.strip_prefix(c).ok())
            .unwrap_or(p)
            .display()
            .to_string();
        without_verbatim_prefix(shown)
    };

    let (mut changed, mut failed) = (0usize, 0usize);
    for file in &files {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                crate::statusline::log_stderr(&format!("{}: error: {e}", shown(file)));
                failed += 1;
                continue;
            }
        };
        match byard_compiler::fmt::format(&source) {
            Ok(formatted) if formatted == source => {}
            Ok(formatted) => {
                changed += 1;
                if check {
                    println!("{}", shown(file));
                } else if let Err(e) = std::fs::write(file, formatted) {
                    crate::statusline::log_stderr(&format!("{}: error: {e}", shown(file)));
                    failed += 1;
                    changed -= 1;
                }
            }
            Err(e) => {
                crate::statusline::log_stderr(&format!("{}:{e}", shown(file)));
                failed += 1;
            }
        }
    }

    let n = files.len();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    // On stderr, so that what `--check` prints on stdout is only the files.
    if failed > 0 {
        crate::statusline::log_stderr(&format!(
            "error: {failed} file{} could not be formatted",
            plural(failed)
        ));
    }
    if check && changed > 0 {
        crate::statusline::log_stderr(&format!(
            "error: {changed} of {n} file{} not formatted",
            plural(n)
        ));
    }
    if failed > 0 || (check && changed > 0) {
        // The diagnostics are printed; signal failure without a second message.
        return Err(String::new());
    }
    let summary = if check || changed == 0 {
        format!("{n} file{} formatted", plural(n))
    } else {
        format!("{n} file{}, {changed} changed", plural(n))
    };
    style::ok(&summary, Some(started.elapsed()));
    Ok(())
}

/// The files to format, each once, in the order they were asked for.
fn collect(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |found: Vec<PathBuf>| {
        for f in found {
            if !out.contains(&f) {
                out.push(f);
            }
        }
    };
    if paths.is_empty() {
        add(owned_source_files(&Manifest::discover(None)?)?);
        return Ok(out);
    }
    for path in paths {
        let path = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if path.is_dir() {
            if path.join("byard.toml").exists() {
                add(owned_source_files(&Manifest::discover(Some(&path))?)?);
            } else {
                add(byd_files_under(&path)?);
            }
        } else {
            add(vec![path]);
        }
    }
    Ok(out)
}

/// A path outside the current directory as a person writes it: Windows'
/// verbatim `\\?\C:\dir` becomes `C:\dir`. A verbatim UNC path
/// (`\\?\UNC\server\share`) is left as it is.
fn without_verbatim_prefix(shown: String) -> String {
    match shown.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_string(),
        _ => shown,
    }
}

#[cfg(test)]
mod shown_tests {
    use super::without_verbatim_prefix;

    #[test]
    fn a_verbatim_drive_path_is_shown_without_its_prefix() {
        assert_eq!(
            without_verbatim_prefix(r"\\?\C:\work\main.byd".to_string()),
            r"C:\work\main.byd"
        );
        assert_eq!(
            without_verbatim_prefix(r"\\?\UNC\host\share\a.byd".to_string()),
            r"\\?\UNC\host\share\a.byd"
        );
        assert_eq!(
            without_verbatim_prefix("src/main.byd".to_string()),
            "src/main.byd"
        );
    }
}
