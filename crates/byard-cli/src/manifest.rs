//! `byard.toml` discovery and parsing (RFC-0006 §2, decision C1/C2;
//! RFC-0008 Pillar C, the `[dependencies]` table).
//!
//! Dependency entries are parsed **strictly**: a malformed entry is an error,
//! never a warning-and-drop (RFC-0008 explicitly reverses the C2
//! warn-on-unknown policy for this table, silently ignoring a dependency is
//! a reproducibility hazard).

use byard_compiler::interp::theme::{DeclaredFont, FontWeight, Theme, TypoToken};
use std::path::{Path, PathBuf};

/// How a dependency is acquired (RFC-0008 D-H: git + path first; a hosted
/// registry is deferred and layers onto the same lockfile later).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DepSource {
    /// A local package directory, relative to the declaring manifest.
    Path(PathBuf),
    /// A git repository pinned to an exact ref (`rev` or `tag`, D-H requires
    /// a pin; floating branches are not reproducible).
    Git {
        /// Clone URL.
        url: String,
        /// The pinned ref.
        reference: GitRef,
    },
    /// A published package in a registry directory (RFC-0008 D-H): the
    /// registry's `index.toml` names the archive and the checksum for each
    /// `name` and exact `version`. A directory rather than a URL for now,
    /// and the same layout served over HTTP is a registry.
    Registry {
        /// The registry directory, relative to the declaring manifest.
        registry: PathBuf,
        /// The exact version (no ranges: the solver is deferred, D-K).
        version: String,
    },
}

/// The pin of a git dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitRef {
    /// An exact commit hash.
    Rev(String),
    /// A tag name.
    Tag(String),
}

impl GitRef {
    /// The ref as written (for display and lockfile source strings).
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Rev(s) | Self::Tag(s) => s,
        }
    }
}

/// One `[dependencies]` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    /// The package name (the `use` name in `byld` source).
    pub name: String,
    /// Where it comes from.
    pub source: DepSource,
}

/// The `[dev]` table (RFC-0030 §V2), dev-runner surface only.
///
/// Every field has a default and an absent `[dev]` table is not an error, so a
/// manifest written before this existed keeps working unchanged. Nothing here
/// reaches a shipped application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevConfig {
    /// `frame_budget = "8ms"`, what the profile block's bars and the
    /// statusline's sparkline are drawn against.
    ///
    /// `None` means "the display's refresh interval", which is the default and
    /// the right one: the budget answers *"will this app drop frames on this
    /// machine"*, and on a 120 Hz panel that threshold is 8.3 ms. Bars drawn
    /// against a fixed 16.7 ms there would report a comfortable frame for one
    /// that visibly stutters, lying on exactly the hardware where the frame
    /// budget matters most (§Q3).
    ///
    /// Cross-machine comparability is a different job, served by `--trace` and
    /// by pinning this value in CI. Both are explicit, which is the point.
    pub frame_budget_ns: Option<u64>,
    /// `hud = true`, start with the in-window HUD visible.
    pub hud: bool,
    /// `statusline = false`, suppress the anchored line even on a TTY, for a
    /// terminal that renders it badly.
    pub statusline: bool,
}

impl Default for DevConfig {
    fn default() -> Self {
        Self {
            frame_budget_ns: None,
            hud: false,
            statusline: true,
        }
    }
}

/// Parsed project manifest (or a synthetic one for bare-file usage).
pub struct Manifest {
    pub project_root: PathBuf,
    /// Absolute path to the `.byd` entry file.
    pub entry: PathBuf,
    pub name: String,
    /// Declared dependencies (RFC-0008 Pillar C). Empty for bare-file usage.
    pub dependencies: Vec<Dependency>,
    /// True when this is a lone `.byd` entry (`byard check foo.byd`), not a
    /// project: only the entry file is compiled, sibling `.byd`s are ignored.
    /// A real project (a `byard.toml`) treats every sibling as one namespace.
    pub single_file: bool,
    /// `[assets.vectors] include`, the RFC-0009 §4 escape hatch: handles the
    /// AOT packer must bake even though no `VectorIcon("literal")` names them
    /// (e.g. a `VectorIcon(someVar)` resolved at runtime). Empty by default.
    pub vector_includes: Vec<String>,
    /// The resolved design-token theme (RFC-0022): the built-in `byard-base`
    /// with any `[theme]` / `[assets.fonts]` declarations from `byard.toml`
    /// layered on top. Bare-file usage gets `byard-base` unchanged.
    pub theme: Theme,
    /// The `[dev]` table (RFC-0030 §V2).
    pub dev: DevConfig,
}

impl Manifest {
    /// Discover the manifest by walking up from `CWD`, or fall back to
    /// `main.byd` in the current directory (C1).  If `override_path` is given,
    /// it is used as the entry file directly (no manifest required).
    pub fn discover(override_path: Option<&Path>) -> Result<Self, String> {
        if let Some(p) = override_path {
            let p = p
                .canonicalize()
                .map_err(|e| format!("{}: {e}", p.display()))?;
            // Pointing at a project (a directory, or its `byard.toml`) reads the
            // manifest, including `[dependencies]`, rather than treating the
            // path as a lone entry file. Only a `.byd` path is a bare entry.
            if p.is_dir() {
                let manifest = p.join("byard.toml");
                if manifest.exists() {
                    return Self::from_toml(&manifest);
                }
                let main = p.join("main.byd");
                if main.exists() {
                    return Ok(Self::bare_entry(main));
                }
                return Err(format!("`{}` has no byard.toml or main.byd", p.display()));
            }
            if p.file_name().is_some_and(|n| n == "byard.toml") {
                return Self::from_toml(&p);
            }
            return Ok(Self::bare_entry(p));
        }

        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;

        // Walk upward looking for byard.toml (C1).
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join("byard.toml");
            if candidate.exists() {
                return Self::from_toml(&candidate);
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }

        // CWD fallback: use main.byd if it exists.
        let fallback = cwd.join("main.byd");
        if fallback.exists() {
            let name = cwd
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("byard-project")
                .to_string();
            return Ok(Self {
                project_root: cwd.clone(),
                entry: fallback,
                name,
                dependencies: Vec::new(),
                single_file: false,
                vector_includes: Vec::new(),
                theme: Theme::byard_base(),
                dev: DevConfig::default(),
            });
        }

        Err("no byard.toml found and no main.byd in current directory\n\
             hint: run `byard new <name>` to create a project, or pass a file path"
            .to_string())
    }

    /// A manifest for a lone `.byd` entry file with no `[dependencies]`, the
    /// bare single-file path (`byard check foo.byd`). `entry` is assumed to
    /// already be canonicalized.
    fn bare_entry(entry: PathBuf) -> Self {
        let project_root = entry.parent().unwrap_or(Path::new(".")).to_path_buf();
        let name = project_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("byard-project")
            .to_string();
        Self {
            project_root,
            entry,
            name,
            dependencies: Vec::new(),
            single_file: true,
            vector_includes: Vec::new(),
            theme: Theme::byard_base(),
            dev: DevConfig::default(),
        }
    }

    fn from_toml(path: &Path) -> Result<Self, String> {
        let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let table: toml::Table = src.parse().map_err(|e: toml::de::Error| e.to_string())?;

        let project_root = path.parent().unwrap_or(Path::new(".")).to_path_buf();

        let project = table.get("project");

        // Warn on unknown top-level keys (C2: forward-compatible), except
        // `[dependencies]`, which is parsed strictly below, `[package]`,
        // reserved for package manifests, and `[assets]` (RFC-0009 §4).
        for key in table.keys() {
            if !matches!(
                key.as_str(),
                "project" | "dependencies" | "package" | "assets" | "theme" | "dev"
            ) {
                eprintln!("byard.toml: warning: unknown key `{key}` (ignored)");
            }
        }

        // `[assets.vectors] include = ["a.svg", ...]`, the AOT escape hatch.
        let vector_includes = table
            .get("assets")
            .and_then(|a| a.get("vectors"))
            .and_then(|v| v.get("include"))
            .and_then(toml::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let name = project
            .and_then(|p| p.get("name"))
            .and_then(|v| v.as_str())
            .map_or_else(
                || {
                    project_root
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("byard-project")
                        .to_string()
                },
                str::to_string,
            );

        let entry_rel = project
            .and_then(|p| p.get("entry"))
            .and_then(|v| v.as_str())
            .unwrap_or("main.byd");

        let entry = project_root.join(entry_rel);
        if !entry.exists() {
            return Err(format!(
                "entry file `{}` not found (set in byard.toml [project].entry)",
                entry.display()
            ));
        }

        let dependencies = match table.get("dependencies") {
            Some(deps) => parse_dependencies(deps)?,
            None => Vec::new(),
        };

        // RFC-0022: `[theme]` tokens + `[assets.fonts]` layer onto `byard-base`.
        let theme = parse_theme(&table, &project_root, &dependencies)?;
        // RFC-0030 §V2: the dev runner's own surface.
        let dev = parse_dev(&table)?;

        Ok(Self {
            project_root,
            entry,
            name,
            dependencies,
            single_file: false,
            vector_includes,
            theme,
            dev,
        })
    }
}

/// Parses the `[dev]` table (RFC-0030 §V2).
///
/// Strict, like `[dependencies]` and unlike the forward-compatible top level: a
/// misspelt `frame_bugdet` that was silently ignored would leave a developer
/// staring at bars drawn against a budget they thought they had changed, which
/// is worse than a config error.
fn parse_dev(table: &toml::Table) -> Result<DevConfig, String> {
    let mut dev = DevConfig::default();
    let Some(tbl) = table.get("dev").and_then(toml::Value::as_table) else {
        return Ok(dev);
    };
    for (key, value) in tbl {
        match key.as_str() {
            "frame_budget" => {
                let s = value.as_str().ok_or_else(|| {
                    "byard.toml: [dev] `frame_budget` must be a string like \"8ms\"".to_string()
                })?;
                dev.frame_budget_ns = Some(parse_duration_ns(s).ok_or_else(|| {
                    format!(
                        "byard.toml: [dev] `frame_budget = {s:?}` is not a duration \
                         (use `\"8ms\"`, `\"16.7ms\"`, `\"8300us\"` or `\"0.008s\"`)"
                    )
                })?);
            }
            "hud" => {
                dev.hud = value
                    .as_bool()
                    .ok_or_else(|| "byard.toml: [dev] `hud` must be a boolean".to_string())?;
            }
            "statusline" => {
                dev.statusline = value.as_bool().ok_or_else(|| {
                    "byard.toml: [dev] `statusline` must be a boolean".to_string()
                })?;
            }
            other => {
                return Err(format!("byard.toml: [dev]: unknown key `{other}`"));
            }
        }
    }
    Ok(dev)
}

/// Parses `"8ms"` / `"16.7ms"` / `"8300us"` / `"0.008s"` into nanoseconds.
///
/// A hand-rolled parser rather than a duration crate, for the same reason
/// `style.rs` has no colour crate: this is one function over four suffixes, and
/// the dependency budget is better spent elsewhere.
fn parse_duration_ns(s: &str) -> Option<u64> {
    let s = s.trim();
    // Longest suffix first: `ms` and `ns` both end in `s`, so a bare `s` arm
    // that ran earlier would parse `"8ms"` as `8m` seconds.
    let (number, scale) = [
        ("ms", 1_000_000.0),
        ("us", 1_000.0),
        ("\u{b5}s", 1_000.0),
        ("ns", 1.0),
        ("s", 1_000_000_000.0),
    ]
    .into_iter()
    .find_map(|(suffix, scale)| s.strip_suffix(suffix).map(|n| (n, scale)))?;
    let value: f64 = number.trim().parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((value * scale).round() as u64)
}

/// Parses the `[theme]` table and `[assets.fonts]` into a [`Theme`] layered over
/// the built-in `byard-base` (RFC-0022). Every malformed token is an **error**
/// (a silently-dropped theme token is a hard-to-debug visual regression); the
/// message names the exact `token`/scheme so the fix is obvious.
///
/// Tokens are declared `snake_case` here and canonicalized to the `camelCase`
/// byld reference form by [`Theme::set_color`] et al.
///
/// Packages come first (RFC-0008 pillar D): every dependency's
/// `[assets.fonts]` is read relative to *that package's* root and registered
/// as `"<package>/<Family>"`, and `extends = "<dependency>"` layers the
/// package's own `[theme]` under this one, so a design-system package can ship
/// its palette, type scale and typefaces together.
fn parse_theme(
    table: &toml::Table,
    project_root: &Path,
    dependencies: &[Dependency],
) -> Result<Theme, String> {
    let mut theme = Theme::byard_base();

    // The dependencies that are on disk, with their manifests. One that is
    // not (a git source before `byard get`) is skipped here: the module
    // resolver reports it with the fetch hint, and reporting it twice helps
    // nobody. Naming it in `extends` is the exception, handled below.
    let mut packages: Vec<(&str, String, toml::Table)> = Vec::new();
    for dep in dependencies {
        let Ok(root) = crate::deps::dep_root(project_root, dep) else {
            continue;
        };
        let Some(pkg) = read_package_manifest(&root)? else {
            continue;
        };
        // Fonts are named by the package's own `[package] name`, not by the
        // key this project gave the dependency: the package's views name
        // their fonts without knowing what a consumer calls them.
        let owner = pkg
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
            .unwrap_or(&dep.name)
            .to_string();
        let in_package = |e: String| format!("package `{owner}`: {e}");
        for (family, font) in load_fonts(&pkg, &root, true).map_err(in_package)? {
            theme.add_font(format!("{owner}/{family}"), font);
        }
        packages.push((dep.name.as_str(), owner, pkg));
    }

    if let Some(theme_tbl) = table.get("theme").and_then(toml::Value::as_table) {
        match theme_tbl.get("extends").map(toml::Value::as_str) {
            None | Some(Some("byard-base")) => {}
            Some(None) => {
                return Err(
                    "byard.toml: [theme] `extends` must be a string naming a dependency".into(),
                );
            }
            Some(Some(base)) => {
                if let Some((_, owner, pkg)) = packages.iter().find(|(n, ..)| *n == base) {
                    if let Some(pkg_theme) = pkg.get("theme").and_then(toml::Value::as_table) {
                        apply_theme_table(&mut theme, pkg_theme, Some(owner))
                            .map_err(|e| format!("package `{owner}`: {e}"))?;
                    }
                } else if dependencies.iter().any(|d| d.name == base) {
                    // Declared but not on disk yet (a registry or git source
                    // before `byard get`). Not an error here: `byard get`
                    // loads this manifest too, and failing would block the
                    // one command that fixes it. The module resolver reports
                    // the missing package, with the fetch hint, everywhere
                    // else.
                } else {
                    let declared: Vec<&str> =
                        dependencies.iter().map(|d| d.name.as_str()).collect();
                    return Err(format!(
                        "byard.toml: [theme] extends `{base}`, which is neither `byard-base` nor a \
                         dependency{}",
                        if declared.is_empty() {
                            String::new()
                        } else {
                            format!(" (declared: {})", declared.join(", "))
                        }
                    ));
                }
            }
        }
        apply_theme_table(&mut theme, theme_tbl, None)?;
    }

    // The project's own fonts, relative to the project.
    for (family, font) in load_fonts(table, project_root, false)? {
        theme.add_font(family, font);
    }

    Ok(theme)
}

/// A package's `byard.toml`, parsed; `None` when it has none.
fn read_package_manifest(root: &Path) -> Result<Option<toml::Table>, String> {
    let path = root.join("byard.toml");
    if !path.exists() {
        return Ok(None);
    }
    let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    src.parse()
        .map(Some)
        .map_err(|e: toml::de::Error| format!("{}: {e}", path.display()))
}

/// Applies one `[theme]` table's tokens onto `theme`. `package` names the
/// package the table came from, so a typography token's `family` resolves to
/// that package's own font when it declares one (`"Roboto"` inside
/// `material` means `"material/Roboto"`).
fn apply_theme_table(
    theme: &mut Theme,
    theme_tbl: &toml::Table,
    package: Option<&str>,
) -> Result<(), String> {
    if let Some(name) = theme_tbl.get("name").and_then(toml::Value::as_str) {
        theme.name = name.to_string();
    }
    // `transition = <ms>` (RFC-0016): how long a scheme flip cross-fades
    // the colour tokens. Absent is the cut every theme has always had.
    if let Some(v) = theme_tbl.get("transition") {
        let ms = as_number(v)
            .filter(|ms| *ms >= 0.0)
            .ok_or_else(|| {
                "byard.toml: [theme] `transition` must be a duration in milliseconds, like `transition = 250`"
                    .to_string()
            })?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            theme.transition_ms = ms.round() as u32;
        }
    }
    // `seed = "#RRGGBB"` (RFC-0022 §5): derive both schemes' colour roles
    // from one brand colour. Applied before `[theme.color.*]` below, so an
    // explicitly declared token always beats the derived one.
    if let Some(v) = theme_tbl.get("seed") {
        let rgb = v.as_str().and_then(parse_hex_color).ok_or_else(|| {
            "byard.toml: [theme] `seed` must be a hex colour string, like `seed = \"#6750A4\"`"
                .to_string()
        })?;
        theme.apply_seed(rgb);
    }
    // `extends` beyond the built-in `byard-base` (multi-level, cross-package)
    // is deferred (RFC-0022 unresolved question); anything else is accepted
    // and simply layers onto `byard-base`, the only built-in base today.

    // [theme.color.<scheme>], a table of `token = "#RRGGBB"`.
    if let Some(colors) = theme_tbl.get("color").and_then(toml::Value::as_table) {
        for (scheme, tokens) in colors {
            let tokens = tokens.as_table().ok_or_else(|| {
                format!(
                    "byard.toml: [theme.color.{scheme}] must be a table of `token = \"#RRGGBB\"`"
                )
            })?;
            for (token, value) in tokens {
                let hex = value.as_str().ok_or_else(|| {
                    format!("byard.toml: theme color `{scheme}.{token}` must be a string like \"#6750A4\"")
                })?;
                let rgb = parse_hex_color(hex).ok_or_else(|| {
                    format!("byard.toml: theme color `{scheme}.{token} = {hex:?}` is not a valid `#RRGGBB` / `#RGB` hex color")
                })?;
                theme.set_color(scheme, token, rgb);
            }
        }
    }

    // [theme.typography], `token = { size, family?, weight?, tracking?, line_height? }`.
    if let Some(typo) = theme_tbl.get("typography").and_then(toml::Value::as_table) {
        for (token, value) in typo {
            let mut tok = parse_typo_token(token, value)?;
            // Inside a package, a family the package ships is that
            // package's font, not whatever the consumer calls the same.
            if let (Some(pkg), Some(family)) = (package, tok.family.as_ref()) {
                let scoped = format!("{pkg}/{family}");
                if theme.has_font(&scoped) {
                    tok.family = Some(scoped);
                }
            }
            theme.set_typo(token, tok);
        }
    }

    // [theme.breakpoints], `name = <logical px>` (RFC-0016 responsive
    // variants). Strict, like every other token table: a breakpoint that
    // is not a number would make every `on width >= name` block silently
    // never apply.
    if let Some(bps) = theme_tbl.get("breakpoints").and_then(toml::Value::as_table) {
        for (name, value) in bps {
            let px = as_number(value).ok_or_else(|| {
                format!(
                    "byard.toml: breakpoint `{name}` must be a width in logical pixels, like `md = 600`"
                )
            })?;
            #[allow(clippy::cast_possible_truncation)]
            theme.set_breakpoint(name, px as f32);
        }
    }

    // [theme.shape], `token = <radius>`.
    if let Some(shapes) = theme_tbl.get("shape").and_then(toml::Value::as_table) {
        for (token, value) in shapes {
            let radius = as_number(value).ok_or_else(|| {
                format!("byard.toml: theme shape `{token}` must be a number (corner radius)")
            })?;
            #[allow(clippy::cast_possible_truncation)]
            theme.set_shape(token, radius as f32);
        }
    }

    Ok(())
}

/// Reads a manifest's `[assets.fonts]`, `family = "path/to/font.ttf"`
/// (RFC-0022 §3, RFC-0034), relative to `root`.
///
/// The bytes are read **here**, at manifest time, so an unreadable or
/// unparsable font file is a compile diagnostic naming the family and the
/// path (INV-4). It used to be deferred, and deferring it is what made a
/// declared family a name with nothing behind it.
///
/// A package's assets must live inside the package (`in_package`): it is
/// fetched and published as one directory, and a path that climbs out of it
/// would name a file on the author's machine and nowhere else.
fn load_fonts(
    table: &toml::Table,
    root: &Path,
    in_package: bool,
) -> Result<Vec<(String, DeclaredFont)>, String> {
    let Some(fonts) = table
        .get("assets")
        .and_then(|a| a.get("fonts"))
        .and_then(toml::Value::as_table)
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(fonts.len());
    for (family, path) in fonts {
        let path = path.as_str().ok_or_else(|| {
            format!("byard.toml: [assets.fonts] `{family}` must be a string path to a font file")
        })?;
        if in_package && !is_inside_package(path) {
            return Err(format!(
                "byard.toml: [assets.fonts] `{family}` = {path:?} is outside the package; \
                 a package's assets must live inside it"
            ));
        }
        out.push((family.clone(), load_font(family, path, root)?));
    }
    Ok(out)
}

/// Whether a declared asset path stays inside the directory it is relative
/// to: relative, and never climbing out with `..`.
pub fn is_inside_package(path: &str) -> bool {
    let p = Path::new(path);
    !p.is_absolute()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && p.components().all(|c| {
            matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

/// Reads one `[assets.fonts]` entry's bytes and resolves the family name the
/// face carries (RFC-0034 §Reference "Asset side").
///
/// Both failures are errors that name the family *and* the path, because the
/// author wrote one and the filesystem knows the other, and a message with
/// only one of them makes them go looking. A missing font must never surface
/// as a square box or as silence (INV-4): by the time text is painted, nobody
/// can tell a missing file from a font that simply looks like that.
fn load_font(family: &str, path: &str, project_root: &Path) -> Result<DeclaredFont, String> {
    let full = project_root.join(path);
    let bytes = std::fs::read(&full).map_err(|e| {
        format!(
            "byard.toml: [assets.fonts] `{family}` points at `{}`, which could not be read: {e}",
            full.display()
        )
    })?;
    let bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(bytes);
    let resolved = byard_core::text::family_name(&bytes).ok_or_else(|| {
        format!(
            "byard.toml: [assets.fonts] `{family}` points at `{}`, which is not a font file this build can read",
            full.display()
        )
    })?;
    Ok(DeclaredFont {
        path: path.to_string(),
        resolved: std::sync::Arc::from(resolved),
        bytes,
    })
}

/// Parses one `[theme.typography]` entry: an inline table `{ size, family?,
/// weight?, tracking?, line_height? }`. `size` is required.
fn parse_typo_token(token: &str, value: &toml::Value) -> Result<TypoToken, String> {
    let tbl = value.as_table().ok_or_else(|| {
        format!("byard.toml: typography token `{token}` must be a table like `{{ size = 22, weight = \"regular\" }}`")
    })?;
    for key in tbl.keys() {
        if !matches!(
            key.as_str(),
            "size" | "family" | "weight" | "tracking" | "line_height"
        ) {
            return Err(format!(
                "byard.toml: typography token `{token}`: unknown field `{key}`"
            ));
        }
    }
    let size = tbl
        .get("size")
        .and_then(as_number)
        .ok_or_else(|| format!("byard.toml: typography token `{token}` needs a numeric `size`"))?;
    #[allow(clippy::cast_possible_truncation)]
    let size = size as f32;
    let family = tbl
        .get("family")
        .and_then(toml::Value::as_str)
        .map(str::to_string);
    let weight = match tbl.get("weight") {
        Some(v) => {
            let s = v.as_str().ok_or_else(|| {
                format!("byard.toml: typography token `{token}`: `weight` must be a string")
            })?;
            FontWeight::parse(s).ok_or_else(|| {
                format!("byard.toml: typography token `{token}`: unknown weight `{s}` (use thin/light/regular/medium/semibold/bold/black)")
            })?
        }
        None => FontWeight::Regular,
    };
    #[allow(clippy::cast_possible_truncation)]
    let tracking = tbl.get("tracking").and_then(as_number).unwrap_or(0.0) as f32;
    #[allow(clippy::cast_possible_truncation)]
    let line_height = tbl
        .get("line_height")
        .and_then(as_number)
        .map_or_else(|| (size * 1.25).round(), |lh| lh as f32);
    Ok(TypoToken {
        family,
        size,
        weight,
        tracking,
        line_height,
    })
}

/// Reads a TOML value as a number, accepting either a float or an integer
/// (TOML distinguishes `12` from `12.0`; a token radius/size may be written
/// either way).
fn as_number(value: &toml::Value) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    value
        .as_float()
        .or_else(|| value.as_integer().map(|i| i as f64))
}

/// Parses a CSS-style hex color (`#RRGGBB` or `#RGB`, `#` optional) to a packed
/// `0x00RRGGBB` `i64`. Returns `None` for any malformed input.
fn parse_hex_color(s: &str) -> Option<i64> {
    let h = s.strip_prefix('#').unwrap_or(s);
    let expanded = match h.len() {
        // `#RGB` shorthand → duplicate each nibble.
        3 => h.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => h.to_string(),
        _ => return None,
    };
    i64::from_str_radix(&expanded, 16).ok()
}

/// Parses a `[dependencies]` table (RFC-0008 Pillar C). Every malformed entry
/// is an **error**, this table is never warn-and-ignore.
pub fn parse_dependencies(deps: &toml::Value) -> Result<Vec<Dependency>, String> {
    let table = deps.as_table().ok_or("`[dependencies]` must be a table")?;

    let mut out = Vec::with_capacity(table.len());
    for (name, value) in table {
        out.push(parse_dependency(name, value)?);
    }
    Ok(out)
}

fn parse_dependency(name: &str, value: &toml::Value) -> Result<Dependency, String> {
    let err = |msg: &str| format!("byard.toml: dependency `{name}`: {msg}");

    if value.as_str().is_some() {
        return Err(err(
            "a bare version string does not say which registry to use; \
             write `{ registry = \"…\", version = \"…\" }`, `{ git = \"…\", tag|rev = \"…\" }` \
             or `{ path = \"…\" }`",
        ));
    }
    let spec = value.as_table().ok_or_else(|| {
        err("must be a table like `{ path = \"…\" }` or `{ git = \"…\", tag = \"…\" }`")
    })?;

    for key in spec.keys() {
        if !matches!(
            key.as_str(),
            "path" | "git" | "rev" | "tag" | "registry" | "version"
        ) {
            return Err(err(&format!("unknown key `{key}`")));
        }
    }

    if let Some(registry) = spec.get("registry") {
        if ["path", "git", "rev", "tag"]
            .iter()
            .any(|k| spec.contains_key(*k))
        {
            return Err(err("`registry` cannot be combined with `path` or `git`"));
        }
        let registry = registry
            .as_str()
            .ok_or_else(|| err("`registry` must be a string"))?;
        let version = spec
            .get("version")
            .ok_or_else(|| err("a registry dependency needs an exact `version = \"…\"`"))?
            .as_str()
            .ok_or_else(|| err("`version` must be a string"))?;
        return Ok(Dependency {
            name: name.to_string(),
            source: DepSource::Registry {
                registry: PathBuf::from(registry),
                version: version.to_string(),
            },
        });
    }
    if spec.contains_key("version") {
        return Err(err("`version` only applies to `registry` sources"));
    }

    let path = spec.get("path").map(|v| {
        v.as_str()
            .map(PathBuf::from)
            .ok_or_else(|| err("`path` must be a string"))
    });
    let git = spec.get("git").map(|v| {
        v.as_str()
            .map(str::to_string)
            .ok_or_else(|| err("`git` must be a string"))
    });

    match (path, git) {
        (Some(_), Some(_)) => Err(err("`path` and `git` are mutually exclusive")),
        (Some(path), None) => {
            if spec.contains_key("rev") || spec.contains_key("tag") {
                return Err(err("`rev`/`tag` only apply to `git` sources"));
            }
            Ok(Dependency {
                name: name.to_string(),
                source: DepSource::Path(path?),
            })
        }
        (None, Some(url)) => {
            let rev = spec.get("rev").map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| err("`rev` must be a string"))
            });
            let tag = spec.get("tag").map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| err("`tag` must be a string"))
            });
            let reference = match (rev, tag) {
                (Some(_), Some(_)) => return Err(err("`rev` and `tag` are mutually exclusive")),
                (Some(rev), None) => GitRef::Rev(rev?),
                (None, Some(tag)) => GitRef::Tag(tag?),
                (None, None) => {
                    return Err(err(
                        "a git dependency must pin `rev = \"…\"` or `tag = \"…\"` (D-H: \
                         floating refs are not reproducible)",
                    ));
                }
            };
            Ok(Dependency {
                name: name.to_string(),
                source: DepSource::Git {
                    url: url?,
                    reference,
                },
            })
        }
        (None, None) => Err(err("needs a `path`, a `git` or a `registry` source")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deps(src: &str) -> Result<Vec<Dependency>, String> {
        let table: toml::Table = src.parse().unwrap();
        parse_dependencies(table.get("dependencies").unwrap())
    }

    #[test]
    fn path_and_pinned_git_dependencies_parse() {
        let parsed = deps(
            "[dependencies]\n\
             local = { path = \"../kit\" }\n\
             mat = { git = \"https://example.com/mat\", tag = \"v1.0.0\" }\n\
             exact = { git = \"https://example.com/x\", rev = \"abc123\" }\n",
        )
        .unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].source, DepSource::Path(PathBuf::from("../kit")));
        assert!(matches!(
            &parsed[2].source,
            DepSource::Git { reference: GitRef::Rev(r), .. } if r == "abc123"
        ));
    }

    #[test]
    fn unpinned_git_is_an_error_not_a_warning() {
        let err =
            deps("[dependencies]\nmat = { git = \"https://example.com/mat\" }\n").unwrap_err();
        assert!(err.contains("rev") && err.contains("tag"), "{err}");
    }

    #[test]
    fn bare_version_string_points_at_the_deferred_registry() {
        let err = deps("[dependencies]\nmat = \"1.0\"\n").unwrap_err();
        assert!(err.contains("registry"), "{err}");
    }

    #[test]
    fn unknown_dependency_key_is_an_error() {
        let err = deps("[dependencies]\nmat = { path = \"x\", branch = \"main\" }\n").unwrap_err();
        assert!(err.contains("unknown key `branch`"), "{err}");
    }

    #[test]
    fn path_plus_git_is_rejected() {
        let err =
            deps("[dependencies]\nmat = { path = \"x\", git = \"https://e.com\" }\n").unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    // ── RFC-0022: [theme] / [assets.fonts] parsing ────────────────────────

    fn theme_of(src: &str) -> Result<Theme, String> {
        let table: toml::Table = src.parse().unwrap();
        parse_theme(&table, &examples_root(), &[])
    }

    /// The examples directory, which is where the shipped font assets live.
    /// Font paths in these tests resolve against it, so they exercise the real
    /// files rather than a fixture nothing ships.
    fn examples_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")
    }

    /// A package's theme, extended by a consumer (RFC-0008 pillar D): its
    /// font is registered under the package's name with the bytes read from
    /// the package, its typography points at that font rather than at a
    /// family of the same name the consumer might declare, and the
    /// consumer's own tokens still win.
    #[test]
    fn an_extended_package_theme_brings_its_fonts_scoped_to_it() {
        let dir = std::env::temp_dir().join(format!("byard-pkgtheme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("kit/fonts")).unwrap();
        std::fs::create_dir_all(dir.join("kit/src")).unwrap();
        std::fs::create_dir_all(dir.join("app")).unwrap();
        std::fs::copy(
            examples_root().join("assets/fonts/SpaceGrotesk-Variable.ttf"),
            dir.join("kit/fonts/Display.ttf"),
        )
        .unwrap();
        std::fs::write(
            dir.join("kit/byard.toml"),
            "[package]\nname = \"brand\"\nversion = \"0.1.0\"\n\
             [assets.fonts]\nDisplay = \"fonts/Display.ttf\"\n\
             [theme.color.light]\nprimary = \"#123456\"\nsecondary = \"#654321\"\n\
             [theme.typography]\nheadline = { size = 30, family = \"Display\" }\n",
        )
        .unwrap();
        std::fs::write(dir.join("kit/src/a.byd"), "View A() { Box {} }\n").unwrap();
        std::fs::write(dir.join("app/main.byd"), "View Main() { Box {} }\n").unwrap();
        std::fs::write(
            dir.join("app/byard.toml"),
            "[project]\nname = \"app\"\nentry = \"main.byd\"\n\
             [dependencies]\nkit = { path = \"../kit\" }\n\
             [theme]\nextends = \"kit\"\n\
             [theme.color.light]\nprimary = \"#ABCDEF\"\n",
        )
        .unwrap();

        let theme = Manifest::discover(Some(&dir.join("app"))).unwrap().theme;
        let font = theme
            .font("brand/Display")
            .expect("registered under the package name");
        assert!(!font.bytes.is_empty());
        assert_eq!(
            theme
                .typo("headline")
                .and_then(|t| t.family.clone())
                .as_deref(),
            Some("brand/Display"),
            "the package's typography names the package's font"
        );
        assert_eq!(
            theme.color("secondary", false),
            Some(0x0065_4321),
            "from the package"
        );
        assert_eq!(
            theme.color("primary", false),
            Some(0x00AB_CDEF),
            "the project wins"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hex_colors_parse_full_and_shorthand() {
        assert_eq!(parse_hex_color("#6750A4"), Some(0x0067_50A4));
        assert_eq!(parse_hex_color("6750A4"), Some(0x0067_50A4));
        assert_eq!(parse_hex_color("#FFF"), Some(0x00FF_FFFF));
        assert_eq!(parse_hex_color("#12"), None);
        assert_eq!(parse_hex_color("#GGGGGG"), None);
    }

    #[test]
    fn theme_colors_map_snake_to_camel_and_layer_on_base() {
        let theme = theme_of(
            "[theme]\nname = \"demo\"\n\
             [theme.color.light]\nprimary = \"#6750A4\"\nprimary_container = \"#EADDFF\"\n\
             [theme.color.dark]\nprimary = \"#D0BCFF\"\n",
        )
        .unwrap();
        assert_eq!(theme.name, "demo");
        assert_eq!(theme.color("primary", false), Some(0x0067_50A4));
        assert_eq!(theme.color("primaryContainer", false), Some(0x00EA_DDFF));
        assert_eq!(theme.color("primary", true), Some(0x00D0_BCFF));
        // An untouched base token still resolves (layering, not replacement).
        assert!(theme.color("onSurface", false).is_some());
    }

    #[test]
    fn malformed_theme_color_is_an_error_not_a_drop() {
        let err = theme_of("[theme.color.light]\nprimary = \"not-a-hex\"\n").unwrap_err();
        assert!(err.contains("primary") && err.contains("hex"), "{err}");
    }

    #[test]
    fn typography_token_parses_size_weight_and_defaults() {
        let theme = theme_of(
            "[theme.typography]\n\
             title_large = { size = 22, weight = \"medium\", tracking = 0.1 }\n",
        )
        .unwrap();
        let tok = theme.typo("titleLarge").unwrap();
        assert!((tok.size - 22.0).abs() < f32::EPSILON);
        assert_eq!(tok.weight, FontWeight::Medium);
        assert!((tok.tracking - 0.1).abs() < 1e-6);
        // Derived line height ≈ 1.25× size when unset.
        assert!((tok.line_height - 28.0).abs() < f32::EPSILON);
    }

    #[test]
    fn typography_without_size_is_an_error() {
        let err =
            theme_of("[theme.typography]\ntitle_large = { weight = \"regular\" }\n").unwrap_err();
        assert!(err.contains("size"), "{err}");
    }

    #[test]
    fn typography_unknown_weight_is_an_error() {
        let err = theme_of("[theme.typography]\ntitle_large = { size = 22, weight = \"ultra\" }\n")
            .unwrap_err();
        assert!(err.contains("weight") && err.contains("ultra"), "{err}");
    }

    /// `[theme] transition = <ms>` sets how long a scheme flip cross-fades
    /// (RFC-0016); absent is the cut.
    #[test]
    fn a_theme_transition_parses_and_defaults_to_the_cut() {
        assert_eq!(
            theme_of("[theme]\nname = \"x\"\n").unwrap().transition_ms,
            0
        );
        assert_eq!(
            theme_of("[theme]\ntransition = 250\n")
                .unwrap()
                .transition_ms,
            250
        );
        let err = theme_of("[theme]\ntransition = \"slow\"\n").unwrap_err();
        assert!(
            err.contains("transition") && err.contains("milliseconds"),
            "{err}"
        );
    }

    /// `[theme] seed` derives the palette, both schemes, and an explicit
    /// `[theme.color.*]` token still wins over the derived one.
    #[test]
    fn a_seed_derives_both_schemes_and_explicit_tokens_win() {
        let base = theme_of("[theme]\nname = \"x\"\n").unwrap();
        let seeded = theme_of("[theme]\nseed = \"#1E8E3E\"\n").unwrap();
        for dark in [false, true] {
            assert_ne!(
                seeded.color("primary", dark),
                base.color("primary", dark),
                "dark={dark}"
            );
        }
        assert_ne!(
            seeded.color("primary", false),
            seeded.color("primary", true)
        );
        let overridden =
            theme_of("[theme]\nseed = \"#1E8E3E\"\n[theme.color.light]\nprimary = \"#123456\"\n")
                .unwrap();
        assert_eq!(overridden.color("primary", false), Some(0x12_3456));
        assert_eq!(
            overridden.color("onPrimary", false),
            seeded.color("onPrimary", false)
        );
        let err = theme_of("[theme]\nseed = 12\n").unwrap_err();
        assert!(err.contains("seed"), "{err}");
    }

    /// `[theme.breakpoints]` declares named widths for responsive variants
    /// (RFC-0016), canonicalised like every other token table.
    #[test]
    fn breakpoints_parse_and_canonicalize() {
        let theme = theme_of("[theme.breakpoints]\nmd = 600\nextra_wide = 1440.5\n").unwrap();
        assert_eq!(theme.breakpoint("md"), Some(600.0));
        assert_eq!(theme.breakpoint("extraWide"), Some(1440.5));
        assert_eq!(theme.breakpoint("lg"), None);
    }

    /// A breakpoint that is not a number is an error: read as anything else, it
    /// would make every block that names it silently never apply.
    #[test]
    fn a_breakpoint_that_is_not_a_number_is_an_error() {
        let err = theme_of("[theme.breakpoints]\nmd = \"wide\"\n").unwrap_err();
        assert!(
            err.contains("md") && err.contains("logical pixels"),
            "{err}"
        );
    }

    #[test]
    fn shape_tokens_parse_and_canonicalize() {
        let theme = theme_of("[theme.shape]\ncorner_lg = 16\ncorner_xl = 28\n").unwrap();
        assert_eq!(theme.shape("cornerLg"), Some(16.0));
        assert_eq!(theme.shape("cornerXl"), Some(28.0));
    }

    #[test]
    fn font_assets_register_families() {
        let theme =
            theme_of("[assets.fonts]\ndisplay = \"assets/fonts/SpaceGrotesk-Variable.ttf\"\n")
                .unwrap();
        assert!(theme.has_font("display"));
        assert!(!theme.has_font("sfpro"));
    }

    /// The bytes are loaded, not merely named, and the family name shaping
    /// will match on is resolved from them (RFC-0034).
    ///
    /// The declared name and the resolved one differ here on purpose: that
    /// difference is the reason the record carries both, and a loader that
    /// kept only the manifest's spelling would shape against a family no font
    /// answers to.
    #[test]
    fn a_declared_family_is_loaded_and_resolved() {
        let theme =
            theme_of("[assets.fonts]\ndisplay = \"assets/fonts/SpaceGrotesk-Variable.ttf\"\n")
                .unwrap();
        let font = theme.font("display").expect("declared");
        assert_eq!(font.resolved.as_ref(), "Space Grotesk");
        assert!(!font.bytes.is_empty(), "the bytes were actually read");
    }

    /// A font file that is not there is a diagnostic naming the family and the
    /// path (INV-4). By paint time nobody can tell a missing file from a
    /// typeface that simply looks like that.
    #[test]
    fn a_missing_font_file_names_the_family_and_the_path() {
        let err = theme_of("[assets.fonts]\ndisplay = \"assets/fonts/NotHere.ttf\"\n").unwrap_err();
        assert!(err.contains("display"), "{err}");
        assert!(err.contains("NotHere.ttf"), "{err}");
    }

    /// And a file that is there but is not a font is the same kind of
    /// diagnostic, for the same reason.
    #[test]
    fn a_file_that_is_not_a_font_is_a_diagnostic() {
        let err = theme_of("[assets.fonts]\ndisplay = \"assets/fonts/README.md\"\n").unwrap_err();
        assert!(err.contains("display"), "{err}");
        assert!(err.contains("not a font file"), "{err}");
    }

    // ── RFC-0030 §V2: the [dev] table ─────────────────────────────────────

    fn dev_of(src: &str) -> Result<DevConfig, String> {
        let table: toml::Table = src.parse().unwrap();
        parse_dev(&table)
    }

    #[test]
    fn an_absent_dev_table_changes_nothing() {
        // A manifest written before `[dev]` existed must keep working, and the
        // budget must stay "ask the display" rather than becoming a number.
        let dev = dev_of("[project]\nname = \"x\"\n").unwrap();
        assert_eq!(dev, DevConfig::default());
        assert_eq!(dev.frame_budget_ns, None);
        assert!(dev.statusline);
        assert!(!dev.hud);
    }

    #[test]
    fn frame_budget_parses_every_unit_it_documents() {
        let ns = |src: &str| dev_of(src).unwrap().frame_budget_ns.unwrap();
        assert_eq!(ns("[dev]\nframe_budget = \"8ms\"\n"), 8_000_000);
        assert_eq!(ns("[dev]\nframe_budget = \"16.7ms\"\n"), 16_700_000);
        assert_eq!(ns("[dev]\nframe_budget = \"8300us\"\n"), 8_300_000);
        assert_eq!(ns("[dev]\nframe_budget = \"0.008s\"\n"), 8_000_000);
        assert_eq!(ns("[dev]\nframe_budget = \"500000ns\"\n"), 500_000);
    }

    #[test]
    fn a_misspelt_dev_key_is_an_error_not_a_shrug() {
        // The one place the forward-compatible warn-and-ignore policy would do
        // real harm: a developer who wrote `frame_bugdet` and had it silently
        // dropped is staring at bars drawn against a budget they believe they
        // changed.
        let err = dev_of("[dev]\nframe_bugdet = \"8ms\"\n").unwrap_err();
        assert!(err.contains("frame_bugdet"), "{err}");

        let err = dev_of("[dev]\nframe_budget = \"soon\"\n").unwrap_err();
        assert!(err.contains("duration"), "{err}");

        let err = dev_of("[dev]\nframe_budget = 8\n").unwrap_err();
        assert!(err.contains("string"), "{err}");

        let err = dev_of("[dev]\nstatusline = \"yes\"\n").unwrap_err();
        assert!(err.contains("boolean"), "{err}");
    }

    #[test]
    fn a_zero_or_negative_budget_is_rejected() {
        // Zero would make every bar full and every frame 100 % over budget,
        // which is a readout that is wrong rather than merely unhelpful.
        assert!(parse_duration_ns("0ms").is_none());
        assert!(parse_duration_ns("-8ms").is_none());
        assert!(parse_duration_ns("8").is_none());
        assert!(parse_duration_ns("").is_none());
    }

    #[test]
    fn dev_flags_round_trip() {
        let dev = dev_of("[dev]\nhud = true\nstatusline = false\n").unwrap();
        assert!(dev.hud);
        assert!(!dev.statusline);
    }

    #[test]
    fn no_theme_section_yields_byard_base() {
        let theme = theme_of("[project]\nname = \"x\"\n").unwrap();
        assert_eq!(theme.name, "byard-base");
        assert!(theme.color("primary", false).is_some());
    }
}
