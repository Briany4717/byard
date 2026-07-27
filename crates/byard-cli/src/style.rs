//! Terminal output styling for every `byard` command (RFC-0030 §P1–P4).
//!
//! # No dependency
//!
//! About twenty escape sequences and a resolver. `owo-colors` or `anstyle`
//! would be defensible, but a CLI whose entire visual contract is this small
//! does not need a dependency tree for it, and the project's dependency budget
//! is better spent elsewhere (§P1).
//!
//! # Roles, not colours
//!
//! Call sites name a *role* — `p.err`, `p.metric` — never a colour, and the
//! codes are the **16 base ANSI colours**, never 24-bit RGB. A terminal's
//! palette is a preference the user set deliberately; a tool that hardcodes
//! `#FF5555` overrides it, and looks worse on a meaningful fraction of real
//! terminals in exchange for looking better in the author's screenshot. Using
//! the base sixteen also means the output is legible on light backgrounds
//! without a second theme (§P2).
//!
//! # Resolved once, branched never
//!
//! [`Palette::plain`] sets every field to `""`, so formatting code interpolates
//! unconditionally — `write!(f, "{}{msg}{}", p.err, p.reset)` — and pays one
//! empty-string interpolation when colour is off. There is no `if colour` at
//! any call site, which is the property that keeps a colour bug from being
//! expressible (§P3).

use std::io::IsTerminal;
use std::sync::OnceLock;

/// Semantic roles, resolved once per process.
///
/// Every field is either an ANSI SGR sequence or `""` — never anything in
/// between — so a call site cannot tell which mode it is in and cannot get it
/// wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// A phase completed successfully. SGR 32 (green).
    pub ok: &'static str,
    /// A diagnostic that does not fail the phase. SGR 33 (yellow).
    pub warn: &'static str,
    /// A phase failed. SGR 31 (red).
    pub err: &'static str,
    /// An action in progress, or a key chord. SGR 36 (cyan).
    pub accent: &'static str,
    /// A measured number. SGR 35 (magenta).
    pub metric: &'static str,
    /// Informational detail. SGR 2 (dim).
    pub dim: &'static str,
    /// SGR 1 (bold).
    pub bold: &'static str,
    /// SGR 0.
    pub reset: &'static str,
}

impl Palette {
    /// The 16-colour ANSI palette.
    #[must_use]
    pub const fn ansi() -> Self {
        Self {
            ok: "\x1b[32m",
            warn: "\x1b[33m",
            err: "\x1b[31m",
            accent: "\x1b[36m",
            metric: "\x1b[35m",
            dim: "\x1b[2m",
            bold: "\x1b[1m",
            reset: "\x1b[0m",
        }
    }

    /// Every role empty — the uncoloured mode.
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            ok: "",
            warn: "",
            err: "",
            accent: "",
            metric: "",
            dim: "",
            bold: "",
            reset: "",
        }
    }
}

/// The line prefixes of the output grammar (RFC-0030 §P4).
///
/// A fixed 5-column field, so every line's text starts at the same column and
/// a reader's eye can follow one edge down the output instead of re-finding it
/// per line. Degrades to ASCII with the rest of the glyph set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    /// Success prefix.
    pub ok: &'static str,
    /// Failure prefix.
    pub err: &'static str,
    /// Non-fatal diagnostic prefix.
    pub warn: &'static str,
    /// Informational detail prefix.
    pub info: &'static str,
    /// An action in progress.
    pub action: &'static str,
    /// A hot reload was applied.
    pub reload: &'static str,
    /// A key/value fact in a startup header.
    pub fact: &'static str,
    /// The statusline's "something is animating" indicator.
    pub dot_filled: &'static str,
    /// The statusline's "the scene has settled" indicator.
    pub dot_hollow: &'static str,
    /// The eight block-eighths, for the sparkline. Index 0 is the shortest.
    pub bars: [&'static str; 8],
}

impl Glyphs {
    /// The Unicode set.
    #[must_use]
    pub const fn unicode() -> Self {
        Self {
            ok: "ok",
            err: "err",
            warn: "warn",
            info: "·",
            action: "→",
            reload: "↻",
            fact: "▏",
            dot_filled: "●",
            dot_hollow: "○",
            bars: ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"],
        }
    }

    /// The ASCII fallback, for a terminal that has not told us it speaks
    /// UTF-8. Every string here is one byte per column, which is also what
    /// keeps the width arithmetic in `statusline.rs` honest.
    #[must_use]
    pub const fn ascii() -> Self {
        Self {
            ok: "ok",
            err: "err",
            warn: "warn",
            info: ".",
            action: ">",
            reload: "~",
            fact: "|",
            dot_filled: "*",
            dot_hollow: "o",
            bars: ["_", "_", ".", ".", "-", "-", "=", "#"],
        }
    }
}

static PALETTE: OnceLock<Palette> = OnceLock::new();
static GLYPHS: OnceLock<Glyphs> = OnceLock::new();

/// The process's resolved palette.
///
/// Precedence, following the de-facto standard: `CLICOLOR_FORCE=1` ≻ `NO_COLOR`
/// (set to any value) ≻ `TERM=dumb` ≻ [`IsTerminal`].
pub fn palette() -> &'static Palette {
    PALETTE.get_or_init(|| resolve_palette(&EnvVars))
}

/// The process's resolved glyph set: Unicode when `LC_ALL`, `LC_CTYPE` or
/// `LANG` says UTF-8, ASCII otherwise.
pub fn glyphs() -> &'static Glyphs {
    GLYPHS.get_or_init(|| resolve_glyphs(&EnvVars))
}

/// The environment, behind a trait so the resolution rules are testable
/// without mutating the process's own environment — which is a data race in a
/// threaded test binary and is `unsafe` on current Rust.
trait Env {
    fn var(&self, key: &str) -> Option<String>;
    fn stderr_is_terminal(&self) -> bool;
}

struct EnvVars;

impl Env for EnvVars {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn stderr_is_terminal(&self) -> bool {
        // Stderr rather than stdout: the statusline lives there (§P5), and a
        // piped stdout with an attached terminal is the common `byard dev >
        // log` case, which should still be coloured on screen.
        std::io::stderr().is_terminal()
    }
}

fn resolve_palette(env: &dyn Env) -> Palette {
    if env.var("CLICOLOR_FORCE").is_some_and(|v| v != "0") {
        return Palette::ansi();
    }
    if env.var("NO_COLOR").is_some() {
        return Palette::plain();
    }
    if env.var("TERM").is_some_and(|t| t == "dumb") {
        return Palette::plain();
    }
    if env.stderr_is_terminal() {
        Palette::ansi()
    } else {
        Palette::plain()
    }
}

fn resolve_glyphs(env: &dyn Env) -> Glyphs {
    let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|k| env.var(k))
        .any(|v| {
            v.to_ascii_uppercase().contains("UTF-8") || v.to_ascii_uppercase().contains("UTF8")
        });
    if utf8 {
        Glyphs::unicode()
    } else {
        Glyphs::ascii()
    }
}

// ── The output grammar (§P4) ───────────────────────────────────────────────

/// Composed line width, and the column the duration is right-aligned to.
///
/// Composed, not detected: `ioctl(TIOCGWINSZ)` needs `unsafe`, and the
/// workspace sets `unsafe_code = "deny"` — a duration column does not justify
/// an exception to a workspace-wide soundness invariant, and `terminal_size`
/// would be a dependency for one integer (§Q7). A message longer than this
/// simply pushes its duration right rather than wrapping, which is the
/// failure mode worth having.
const WIDTH: usize = 80;

/// A completed phase: `ok  message                         12ms`.
pub fn ok(message: &str, duration: Option<std::time::Duration>) {
    line(palette().ok, glyphs().ok, message, duration);
}

/// A failed phase.
pub fn err(message: &str) {
    line(palette().err, glyphs().err, message, None);
}

/// A diagnostic that does not fail the phase.
pub fn warn(message: &str) {
    line(palette().warn, glyphs().warn, message, None);
}

/// Informational detail beneath a phase.
pub fn info(message: &str) {
    line(palette().dim, glyphs().info, message, None);
}

/// An action in progress.
pub fn action(message: &str) {
    line(palette().accent, glyphs().action, message, None);
}

/// A hot reload was applied.
pub fn reload(message: &str) {
    line(palette().accent, glyphs().reload, message, None);
}

/// A key/value fact in a startup header: `▏ Entry  src/main.byd`.
pub fn fact(key: &str, value: &str) {
    let p = palette();
    crate::statusline::log(&format!(
        "{}{}{} {}{key:<10}{} {value}",
        p.dim,
        glyphs().fact,
        p.reset,
        p.dim,
        p.reset
    ));
}

/// The shared line shape: a 5-column prefix, the message, and — when a
/// duration is given — a right-aligned duration column.
fn line(colour: &str, glyph: &str, message: &str, duration: Option<std::time::Duration>) {
    // Through the statusline, not `println!` directly: a log write has to erase
    // the anchored line before it scrolls and repaint it afterwards, or the two
    // land on top of each other. With no statusline installed this *is* a
    // `println!` (RFC-0030 §P5).
    crate::statusline::log(&compose(palette(), colour, glyph, message, duration));
}

/// Composes one grammar line, so the shape is testable without capturing
/// stdout.
///
/// Every command routes through here, which is what makes "the duration column
/// is at the same place in `new` as in `build`" a property of the module rather
/// than a coincidence maintained by seven call sites.
fn compose(
    p: &Palette,
    colour: &str,
    glyph: &str,
    message: &str,
    duration: Option<std::time::Duration>,
) -> String {
    let prefix = format!("{colour}{glyph}{}", p.reset);
    // Padding is computed on the *glyph*, not the coloured prefix: the escape
    // sequences occupy no columns, and counting them would misalign every line
    // the moment colour is enabled.
    let pad = 5usize.saturating_sub(display_width(glyph));
    match duration {
        Some(d) => {
            let right = format_duration(d);
            let used = 5 + display_width(message);
            let gap = WIDTH.saturating_sub(used + right.len()).max(1);
            format!(
                "{prefix}{:pad$}{message}{:gap$}{}{right}{}",
                "", "", p.metric, p.reset
            )
        }
        None => format!("{prefix}{:pad$}{message}", ""),
    }
}

/// Milliseconds under a second, seconds with one decimal above it.
#[must_use]
pub fn format_duration(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// Display columns of `s`, counting a `char` as one column and skipping ANSI
/// escape sequences.
///
/// Deliberately not a full `wcwidth`: nothing this CLI prints is CJK or an
/// emoji, and a grapheme-width dependency for a duration column would be the
/// same trade §P1 already declined. It *is* escape-aware, because that is the
/// error that actually happens — a coloured prefix counted as its byte length
/// misaligns every row.
#[must_use]
pub fn display_width(s: &str) -> usize {
    let mut width = 0;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip to the end of the CSI sequence.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        width += 1;
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeEnv {
        vars: HashMap<&'static str, &'static str>,
        tty: bool,
    }

    impl FakeEnv {
        fn new(tty: bool) -> Self {
            Self {
                vars: HashMap::new(),
                tty,
            }
        }

        fn with(mut self, k: &'static str, v: &'static str) -> Self {
            self.vars.insert(k, v);
            self
        }
    }

    impl Env for FakeEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.vars.get(key).map(|s| (*s).to_string())
        }

        fn stderr_is_terminal(&self) -> bool {
            self.tty
        }
    }

    #[test]
    fn a_terminal_gets_colour_and_a_pipe_does_not() {
        assert_eq!(resolve_palette(&FakeEnv::new(true)), Palette::ansi());
        assert_eq!(resolve_palette(&FakeEnv::new(false)), Palette::plain());
    }

    #[test]
    fn no_color_wins_over_a_terminal() {
        // Any value, including the empty string — that is the NO_COLOR
        // contract, and honouring only `NO_COLOR=1` is the usual way to get it
        // wrong.
        let env = FakeEnv::new(true).with("NO_COLOR", "");
        assert_eq!(resolve_palette(&env), Palette::plain());
    }

    #[test]
    fn clicolor_force_wins_over_everything_including_a_pipe() {
        let env = FakeEnv::new(false)
            .with("NO_COLOR", "1")
            .with("TERM", "dumb")
            .with("CLICOLOR_FORCE", "1");
        assert_eq!(resolve_palette(&env), Palette::ansi());
    }

    #[test]
    fn clicolor_force_zero_does_not_force() {
        let env = FakeEnv::new(false).with("CLICOLOR_FORCE", "0");
        assert_eq!(resolve_palette(&env), Palette::plain());
    }

    #[test]
    fn a_dumb_terminal_gets_no_colour() {
        let env = FakeEnv::new(true).with("TERM", "dumb");
        assert_eq!(resolve_palette(&env), Palette::plain());
    }

    #[test]
    fn the_plain_palette_is_entirely_empty() {
        // The property every call site depends on: interpolating a role costs
        // an empty string, so no call site needs an `if colour`.
        let p = Palette::plain();
        for role in [
            p.ok, p.warn, p.err, p.accent, p.metric, p.dim, p.bold, p.reset,
        ] {
            assert_eq!(role, "");
        }
    }

    #[test]
    fn glyphs_degrade_to_ascii_without_a_utf8_locale() {
        assert_eq!(resolve_glyphs(&FakeEnv::new(true)), Glyphs::ascii());
        assert_eq!(
            resolve_glyphs(&FakeEnv::new(true).with("LANG", "C")),
            Glyphs::ascii()
        );
        assert_eq!(
            resolve_glyphs(&FakeEnv::new(true).with("LANG", "en_US.UTF-8")),
            Glyphs::unicode()
        );
        assert_eq!(
            resolve_glyphs(&FakeEnv::new(true).with("LC_ALL", "C.utf8")),
            Glyphs::unicode()
        );
    }

    #[test]
    fn the_ascii_fallback_is_single_byte_throughout() {
        let g = Glyphs::ascii();
        let mut all: Vec<&str> = vec![
            g.ok,
            g.err,
            g.warn,
            g.info,
            g.action,
            g.reload,
            g.fact,
            g.dot_filled,
            g.dot_hollow,
        ];
        all.extend_from_slice(&g.bars);
        for s in all {
            assert!(
                s.is_ascii(),
                "the ASCII fallback contains a multi-byte glyph: {s:?}"
            );
            assert_eq!(
                s.len(),
                s.chars().count(),
                "and its byte length must equal its column count: {s:?}"
            );
        }
    }

    #[test]
    fn display_width_ignores_escape_sequences() {
        // The error that actually happens: a coloured prefix counted as its
        // byte length, misaligning every row the moment colour is on.
        assert_eq!(display_width("ok"), 2);
        assert_eq!(display_width("\x1b[32mok\x1b[0m"), 2);
        assert_eq!(display_width("\x1b[1m\x1b[31merr\x1b[0m"), 3);
        assert_eq!(display_width("·"), 1);
    }

    /// The seven commands' prefixes, in the shape each of them uses them.
    fn every_prefix(g: &Glyphs) -> [&'static str; 7] {
        [g.ok, g.err, g.warn, g.info, g.action, g.reload, g.fact]
    }

    #[test]
    fn the_duration_column_is_at_the_same_place_on_every_line() {
        // `new`, `check`, `build`, `get` and `dev` all emit completion lines
        // with different prefixes and different message lengths. If the column
        // moved between them the eye would have to re-find it per line, which
        // is the entire reason the grammar exists.
        use std::time::Duration;
        let p = Palette::plain();
        let g = Glyphs::unicode();
        let mut columns = Vec::new();
        for (glyph, message, ms) in [
            (g.ok, "3 files", 4u64),
            (g.ok, "0 errors", 24),
            (g.ok, "2 packages, byard.lock written", 999),
            (g.warn, "1 deprecated attribute", 7),
            (g.err, "2 errors", 120),
            (g.reload, "reloaded 12 view(s)", 3),
        ] {
            let out = compose(&p, "", glyph, message, Some(Duration::from_millis(ms)));
            let right = format_duration(Duration::from_millis(ms));
            let end = display_width(&out);
            assert!(
                out.ends_with(&right),
                "the duration must be the last thing on the line: {out:?}"
            );
            columns.push(end);
        }
        for (i, w) in columns.iter().enumerate() {
            assert_eq!(
                *w, WIDTH,
                "line {i} composed to {w} columns, not {WIDTH} — the duration column moved"
            );
        }
    }

    #[test]
    fn colour_never_moves_a_single_column() {
        // The property that makes the alignment above survive `CLICOLOR_FORCE`:
        // escapes occupy no columns, so the coloured and uncoloured renderings
        // of the same line must have identical *display* widths — and stripping
        // the escapes must give back the plain line byte for byte.
        use std::time::Duration;
        let g = Glyphs::unicode();
        for glyph in every_prefix(&g) {
            for duration in [None, Some(Duration::from_millis(42))] {
                let plain = compose(&Palette::plain(), "", glyph, "a message", duration);
                let ansi = compose(
                    &Palette::ansi(),
                    Palette::ansi().ok,
                    glyph,
                    "a message",
                    duration,
                );
                assert_eq!(
                    display_width(&plain),
                    display_width(&ansi),
                    "colour changed the width of a {glyph:?} line"
                );
                assert_eq!(strip_escapes(&ansi), plain, "colour changed the text");
            }
        }
    }

    #[test]
    fn no_color_and_a_pipe_produce_byte_identical_output() {
        // Two different reasons to turn colour off must not produce two
        // different renderings — a diff of two logs should be empty, not full
        // of invisible bytes.
        use std::time::Duration;
        let piped = resolve_palette(&FakeEnv::new(false));
        let refused = resolve_palette(&FakeEnv::new(true).with("NO_COLOR", "1"));
        let dumb = resolve_palette(&FakeEnv::new(true).with("TERM", "dumb"));
        let render = |p: &Palette| {
            compose(
                p,
                p.ok,
                Glyphs::unicode().ok,
                "0 errors",
                Some(Duration::from_millis(24)),
            )
        };
        assert_eq!(render(&piped), render(&refused));
        assert_eq!(render(&piped), render(&dumb));
        assert!(!render(&piped).contains('\x1b'));
    }

    /// Removes every CSI sequence — the test-side inverse of the palette.
    fn strip_escapes(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    #[test]
    fn durations_read_as_ms_below_a_second_and_seconds_above() {
        use std::time::Duration;
        assert_eq!(format_duration(Duration::from_millis(12)), "12ms");
        assert_eq!(format_duration(Duration::from_millis(999)), "999ms");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1.5s");
    }
}
