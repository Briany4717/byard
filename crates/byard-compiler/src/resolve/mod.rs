//! The module resolver (RFC-0008 Pillars A & B).
//!
//! Turns a set of `.byd` files, the root project plus every package reachable
//! through `use`, into **one validated program**: a flat, deterministic
//! `Vec<ViewDecl>` with package-qualified canonical names, ready for
//! [`ViewTable`](crate::interp::views::ViewTable) so instantiation (RFC-0007)
//! is identical for single-file and multi-file programs.
//!
//! Design invariants:
//!
//! - **No I/O decisions in `byld`** (RFC-0001 §1, D-F): a `use` names a
//!   package; *where* that package lives is answered by the
//!   [`PackageProvider`] the CLI/LSP injects (manifest + lockfile + cache).
//! - **A package is one namespace**: every `View` declared across a package's
//!   files shares one flat export table (like a Dart library / Rust crate
//!   root). Views of the *root* project keep their bare names; views of a
//!   dependency get the canonical name `<package>.<View>`. Duplicates are a
//!   [`CompileError::DuplicateViewName`].
//! - **Explicit namespacing (D-G)**: `use pkg as m` → `m.Card`;
//!   `use pkg` → `pkg.Card`; `use pkg.{Card}` → bare `Card`, legal only while
//!   unambiguous, any collision is a [`CompileError::NameCollision`]
//!   demanding an alias. Resolution is order-independent.
//! - **Determinism**: files are processed in the order the provider returns
//!   them (sorted by the CLI), packages in first-`use` order, and the merged
//!   view list is root-first, so the same inputs always produce the same
//!   program and the same diagnostics.
//!
//! Spans: each file is parsed in isolation, then every span is rebased by the
//! file's byte offset in the program-wide [`SourceMap`], so any diagnostic,
//! parse-time or lower-time, can be located back to `file:line:col`.

mod rebase;
mod shorthand;
pub(crate) mod walk;

pub use shorthand::rewrite as resolve_shorthand;

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::diagnostics::{CompileError, Span};
use crate::interp::intrinsics;
use crate::parser::ast::{
    ConstDecl, ElementNode, EnumDecl, Expr, FnDecl, Member, Type, UseDecl, ViewDecl,
};
use crate::parser::parse;
use crate::symbol::Symbol;
use crate::util::closest_match;

/// The reserved name of the root package (the project being built). Displayed
/// as "this project" in diagnostics.
pub const ROOT_PACKAGE: &str = "";

/// One `.byd` source file handed to the resolver: a display name (for
/// diagnostics; typically a path relative to the project root) plus its text.
#[derive(Clone, Debug)]
pub struct SourceFile {
    /// Display name, e.g. `src/sidebar.byd` or `material/src/buttons.byd`.
    pub name: String,
    /// The file's source text.
    pub source: String,
}

/// Supplies package sources to the resolver. Implemented by the CLI (manifest,
/// lockfile, and cache) and the LSP; tests use an in-memory map. The resolver
/// itself never touches the filesystem, acquisition policy lives one layer up
/// (RFC-0008 Pillar C).
pub trait PackageProvider {
    /// Returns the `.byd` files of package `package`, requested by
    /// `dependent` ([`ROOT_PACKAGE`] for the project). Order must be
    /// deterministic (the CLI sorts by path). An `Err` explains why the
    /// package cannot be resolved (not declared, not fetched, …).
    fn package_files(&mut self, dependent: &str, package: &str) -> Result<Vec<SourceFile>, String>;
}

/// One file's slice of the program-wide source map.
#[derive(Clone, Debug)]
pub struct MapEntry {
    /// Display name of the file.
    pub file: String,
    /// Byte offset of this file's first byte in program-wide span space.
    pub base: u32,
    /// The file's source text.
    pub source: String,
}

/// Maps program-wide spans back to `(file, local span)`, the multi-file
/// equivalent of handing `CompileError::render` the single source string.
#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    entries: Vec<MapEntry>,
}

impl SourceMap {
    /// Registers `source` under `file`, returning the base offset its spans
    /// were rebased by.
    fn add(&mut self, file: String, source: String) -> u32 {
        let base = self
            .entries
            .last()
            .map_or(0, |e| e.base + e.source.len() as u32 + 1);
        self.entries.push(MapEntry { file, base, source });
        base
    }

    /// The entry containing `span`, plus the span rebased back to file-local
    /// offsets. `None` for a span outside every file (e.g. the zero span).
    #[must_use]
    pub fn locate(&self, span: Span) -> Option<(&MapEntry, Span)> {
        let entry = self
            .entries
            .iter()
            .rev()
            .find(|e| span.start >= e.base && span.start <= e.base + e.source.len() as u32)?;
        Some((
            entry,
            Span::new(span.start - entry.base, span.end.saturating_sub(entry.base)),
        ))
    }

    /// Renders `err` as `file:line:col: error[kind]: headline` (the C7
    /// rustc-compatible shape `byard check` prints).
    #[must_use]
    pub fn render_line(&self, err: &CompileError) -> String {
        // Advisory diagnostics say so on their own first line, or a reader has
        // to know the code to know whether the build failed.
        let level = if err.is_warning() { "warning" } else { "error" };
        match self.locate(err.span()) {
            Some((entry, local)) => {
                let (line, col) = line_col(&entry.source, local.start as usize);
                format!(
                    "{}:{line}:{col}: {level}[{}]: {}",
                    entry.file,
                    err.kind(),
                    err.headline()
                )
            }
            None => format!("{level}[{}]: {}", err.kind(), err.headline()),
        }
    }

    /// The source context needed to draw a caret block beneath a diagnostic's
    /// machine-readable first line (RFC-0006 **C3**).
    ///
    /// Structured rather than pre-rendered, because the colouring belongs to
    /// whoever is doing the printing. `byard check` writes a palette-aware
    /// block; a future editor integration or a JSON formatter wants the same
    /// numbers and none of the escapes. Putting a `Palette` in the compiler to
    /// avoid one struct would be the wrong direction for a dev-only concern.
    ///
    /// Returns `None` for a diagnostic whose span belongs to no registered
    /// file, a project-level error, for instance, which has a headline and no
    /// source to point at.
    #[must_use]
    pub fn caret(&self, err: &CompileError) -> Option<CaretContext> {
        let (entry, local) = self.locate(err.span())?;
        let start = (local.start as usize).min(entry.source.len());
        let end = (local.end as usize).min(entry.source.len()).max(start);

        let line_start = entry.source[..start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = entry.source[start..]
            .find('\n')
            .map_or(entry.source.len(), |i| start + i);
        let (line, column) = line_col(&entry.source, start);

        Some(CaretContext {
            file: entry.file.clone(),
            line,
            column,
            // A file saved with CRLF endings (any file checked out on Windows
            // with `core.autocrlf`) keeps its `\r` before the `\n`; printed, it
            // sends the terminal's cursor back to the start of the line.
            text: entry.source[line_start..line_end]
                .trim_end_matches('\r')
                .to_string(),
            // Character offsets, not byte offsets: the caret is placed against
            // rendered columns, and a span after a multi-byte character would
            // otherwise point somewhere to its right.
            caret_start: entry.source[line_start..start].chars().count(),
            caret_len: entry.source[start..end.min(line_end)]
                .chars()
                .count()
                .max(1),
        })
    }

    /// Renders `err` with caret-anchored source context against its own file.
    #[must_use]
    pub fn render(&self, err: &CompileError) -> String {
        match self.locate(err.span()) {
            Some((entry, _)) => {
                let mut local = err.clone();
                local.shift_span(-i64::from(entry.base));
                format!("{}: {}", entry.file, local.render(&entry.source))
            }
            None => format!("error: {}", err.headline()),
        }
    }

    /// The registered files, in resolution order.
    pub fn files(&self) -> impl Iterator<Item = &MapEntry> + '_ {
        self.entries.iter()
    }
}

/// Where a diagnostic points, in a form a caller can draw a caret from.
///
/// See [`SourceMap::caret`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaretContext {
    /// The file the span belongs to.
    pub file: String,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column, in characters.
    pub column: usize,
    /// The full source line, without its newline.
    pub text: String,
    /// 0-based character offset of the first caret within [`Self::text`].
    pub caret_start: usize,
    /// How many characters the caret run covers; at least one.
    pub caret_len: usize,
}

/// Converts a byte offset to 1-based (line, col) within `src`.
#[must_use]
pub fn line_col(src: &str, byte: usize) -> (usize, usize) {
    let safe = byte.min(src.len());
    let line = src[..safe].bytes().filter(|&b| b == b'\n').count() + 1;
    let line_start = src[..safe].rfind('\n').map_or(0, |i| i + 1);
    (line, safe - line_start + 1)
}

/// The resolver's output: one flat program plus everything needed to report
/// on it.
#[derive(Debug, Default)]
pub struct ResolvedProgram {
    /// Every view in the program, canonically named (`material.Card`), spans
    /// rebased into [`Self::source_map`] space. Root-project views come first,
    /// in file order, so `views.first()` is still the dev-runner's root.
    pub views: Vec<ViewDecl>,
    /// All diagnostics, in deterministic order.
    pub errors: Vec<CompileError>,
    /// Maps program-wide spans back to files.
    pub source_map: SourceMap,
    /// Loaded packages in load order; [`ROOT_PACKAGE`] first.
    pub packages: Vec<String>,
}

/// The names a view binds itself (its parameters and its top-level `var`,
/// `let` and `fn` members), which shadow a package function of the same name.
fn view_bindings(view: &ViewDecl) -> std::collections::HashSet<Symbol> {
    let mut names: std::collections::HashSet<Symbol> =
        view.params.iter().map(|p| p.name.clone()).collect();
    for member in &view.body {
        if let Member::Var { name, .. } | Member::Let { name, .. } | Member::Fn { name, .. } =
            member
        {
            names.insert(name.clone());
        }
    }
    names
}

/// What a value rewrite needs to know about where an expression is written.
struct ValueScope<'a> {
    /// The package the expression is in.
    pkg_name: &'a str,
    /// The `use` names of its file.
    aliases: &'a HashMap<Symbol, String>,
    /// Names the enclosing view or function binds, which hide a constant of
    /// the same name.
    shadowed: &'a std::collections::HashSet<Symbol>,
}

/// Every name a view binds anywhere in it: its parameters, its `var`, `let`,
/// `fn` and `inject` names at any depth, loop variables, route parameters,
/// event payloads, lambda parameters, the `let`s of action blocks and the
/// bindings of result arms. A constant of one of these names is not reachable
/// by its bare name in this view (RFC-0055 §2).
fn view_binders(view: &mut ViewDecl) -> std::collections::HashSet<Symbol> {
    use crate::parser::ast::AttrKind;
    fn members(list: &[Member], names: &mut std::collections::HashSet<Symbol>) {
        for m in list {
            match m {
                Member::Var { name, .. }
                | Member::Let { name, .. }
                | Member::Inject { name, .. } => {
                    names.insert(name.clone());
                }
                Member::Fn { name, params, .. } => {
                    names.insert(name.clone());
                    names.extend(params.iter().map(|p| p.name.clone()));
                }
                Member::Element(el) => {
                    for attr in &el.attrs {
                        if let AttrKind::Event {
                            payload: Some(p), ..
                        } = &attr.kind
                        {
                            names.insert(p.clone());
                        }
                    }
                    members(&el.children, names);
                }
                Member::For {
                    var, index, body, ..
                } => {
                    names.insert(var.clone());
                    names.extend(index.iter().cloned());
                    members(body, names);
                }
                Member::When { then, els, .. } => {
                    members(then, names);
                    if let Some(els) = els {
                        members(els, names);
                    }
                }
                Member::Route { params, body, .. } => {
                    names.extend(params.iter().cloned());
                    members(body, names);
                }
                _ => {}
            }
        }
    }
    let mut names: std::collections::HashSet<Symbol> =
        view.params.iter().map(|p| p.name.clone()).collect();
    members(&view.body, &mut names);
    for member in &mut view.body {
        walk::member_exprs(member, &mut |e| match e {
            Expr::Lambda { params, .. } => names.extend(params.iter().cloned()),
            Expr::Let { name, .. } => {
                names.insert(name.clone());
            }
            Expr::ControllerCall { ok, err, .. } => {
                for arm in [ok, err].into_iter().flatten() {
                    names.insert(arm.binding.clone());
                }
            }
            _ => {}
        });
    }
    names
}

/// Human name of a package for diagnostics.
fn pkg_display(name: &str) -> String {
    if name == ROOT_PACKAGE {
        "this project".to_string()
    } else {
        format!("package `{name}`")
    }
}

/// One parsed file, spans already rebased.
struct ParsedModule {
    imports: Vec<UseDecl>,
    views: Vec<ViewDecl>,
    fns: Vec<FnDecl>,
    records: Vec<crate::parser::ast::RecordDecl>,
    consts: Vec<ConstDecl>,
    enums: Vec<EnumDecl>,
}

/// A loaded package: its parsed files and flat export table.
#[derive(Default)]
struct PackageData {
    modules: Vec<ParsedModule>,
    /// Bare view name → span of its declaration.
    exports: HashMap<Symbol, Span>,
    /// Bare top-level function name → span of its declaration.
    fn_exports: HashMap<Symbol, Span>,
    /// Bare constant name → span of its declaration.
    const_exports: HashMap<Symbol, Span>,
    /// Bare enum name → its cases.
    enum_exports: HashMap<Symbol, Vec<Symbol>>,
    /// Bare record type names the package declares.
    record_names: std::collections::HashSet<Symbol>,
    /// The names declared `private` (RFC-0055), by kind. A name can be a view
    /// and an enum at once, so each kind has its own set.
    private_views: std::collections::HashSet<Symbol>,
    private_fns: std::collections::HashSet<Symbol>,
    private_consts: std::collections::HashSet<Symbol>,
    private_enums: std::collections::HashSet<Symbol>,
    private_records: std::collections::HashSet<Symbol>,
}

struct Resolver<'p> {
    provider: &'p mut dyn PackageProvider,
    map: SourceMap,
    errors: Vec<CompileError>,
    packages: HashMap<String, PackageData>,
    order: Vec<String>,
    /// Set when the root files are a library package checked on its own: the
    /// package's name, which its views are canonically named under, as they
    /// are for the app that uses it.
    root_library: Option<String>,
}

impl Resolver<'_> {
    /// The prefix a view or function of package `pkg` carries in its
    /// canonical name, `None` for the root project's bare names.
    fn prefix<'a>(&'a self, pkg: &'a str) -> Option<&'a str> {
        if pkg == ROOT_PACKAGE {
            self.root_library.as_deref()
        } else {
            Some(pkg)
        }
    }
}

/// Resolves the whole program: `root_files` (entry first, siblings sorted by
/// the caller) plus every package reachable through `use`, recursively.
#[must_use]
pub fn resolve_program(
    root_files: Vec<SourceFile>,
    provider: &mut dyn PackageProvider,
) -> ResolvedProgram {
    resolve_root(root_files, provider, None)
}

/// Resolves the files of library package `name` on their own, as `byard check`
/// inside a package does. The views are named `name.View`, as they are for an
/// app that uses the package, which is what lets a package declare a view
/// named like a built-in (`Button`) and reach it as `name.Button`.
#[must_use]
pub fn resolve_library(
    name: &str,
    root_files: Vec<SourceFile>,
    provider: &mut dyn PackageProvider,
) -> ResolvedProgram {
    resolve_root(root_files, provider, Some(name.to_string()))
}

fn resolve_root(
    root_files: Vec<SourceFile>,
    provider: &mut dyn PackageProvider,
    root_library: Option<String>,
) -> ResolvedProgram {
    let mut r = Resolver {
        provider,
        map: SourceMap::default(),
        errors: Vec::new(),
        packages: HashMap::new(),
        order: Vec::new(),
        root_library,
    };

    let mut stack = Vec::new();
    r.load_package(ROOT_PACKAGE, root_files, &mut stack);
    r.rewrite_all();

    // Merge: root first (file order), then packages in load order.
    let mut views = Vec::new();
    let mut fns = Vec::new();
    let mut records: Vec<crate::parser::ast::RecordDecl> = Vec::new();
    let mut consts: Vec<ConstDecl> = Vec::new();
    let mut enums: Vec<EnumDecl> = Vec::new();
    for name in &r.order {
        if let Some(pkg) = r.packages.get(name) {
            for module in &pkg.modules {
                views.extend(module.views.iter().cloned());
                fns.extend(module.fns.iter().cloned());
                consts.extend(module.consts.iter().cloned());
                enums.extend(module.enums.iter().cloned());
                for record in &module.records {
                    // A type's name is the program's: two of one name are
                    // one name too many, wherever each was written.
                    if records.iter().any(|r| r.name == record.name) {
                        r.errors.push(CompileError::DuplicateRecordType {
                            span: record.span,
                            name: record.name.as_str().to_string(),
                        });
                    } else {
                        records.push(record.clone());
                    }
                }
            }
        }
    }
    // One table of every function, canonically named, and one of every
    // record type, shared by every view.
    let helpers = std::sync::Arc::new(fns);
    let records = std::sync::Arc::new(records);
    // A case written alone, where the callee's parameter says which enum.
    shorthand::rewrite(&mut views, &enums);
    let consts = std::sync::Arc::new(consts);
    let enums = std::sync::Arc::new(enums);
    for view in &mut views {
        view.helpers = std::sync::Arc::clone(&helpers);
        view.records = std::sync::Arc::clone(&records);
        view.consts = std::sync::Arc::clone(&consts);
        view.enums = std::sync::Arc::clone(&enums);
    }

    // The type checks run over the merged, rebased views, so their spans are
    // already program-wide. Held back while the program has parse or resolve
    // errors: a broken tree would only add follow-on noise to the real one.
    if r.errors.is_empty() {
        r.errors.extend(crate::infer::check_views(&views).errors);
    }

    ResolvedProgram {
        views,
        errors: r.errors,
        source_map: r.map,
        packages: r.order,
    }
}

impl Resolver<'_> {
    /// Parses `files` into package `name`, registers its exports, and loads
    /// its dependencies depth-first. `stack` is the active dependency chain
    /// for cycle detection.
    fn load_package(&mut self, name: &str, files: Vec<SourceFile>, stack: &mut Vec<String>) {
        self.order.push(name.to_string());
        self.packages
            .insert(name.to_string(), PackageData::default());
        stack.push(name.to_string());

        let mut modules = Vec::new();
        let mut exports: HashMap<Symbol, Span> = HashMap::new();
        let mut fn_exports: HashMap<Symbol, Span> = HashMap::new();
        let mut data = PackageData::default();
        let mut dep_requests: Vec<(String, Span)> = Vec::new();

        for file in files {
            let parsed = parse(&file.source);
            let base = self.map.add(file.name, file.source);

            for mut err in parsed.errors {
                err.shift_span(i64::from(base));
                self.errors.push(err);
            }
            let mut imports = parsed.imports;
            for import in &mut imports {
                rebase::shift_use(import, base);
                dep_requests.push((import.package.as_str().to_string(), import.span));
            }
            let mut views = parsed.views;
            for view in &mut views {
                rebase::shift_view(view, base);
                if view.private {
                    data.private_views.insert(view.name.clone());
                }
                match exports.entry(view.name.clone()) {
                    Entry::Vacant(v) => {
                        v.insert(view.span);
                    }
                    Entry::Occupied(_) => self.errors.push(CompileError::DuplicateViewName {
                        span: view.span,
                        name: view.name.as_str().to_string(),
                        package: pkg_display(name),
                    }),
                }
            }
            let mut fns = parsed.fns;
            for decl in &mut fns {
                rebase::shift_fn(decl, base);
                if decl.private {
                    data.private_fns.insert(decl.name.clone());
                }
                match fn_exports.entry(decl.name.clone()) {
                    Entry::Vacant(v) => {
                        v.insert(decl.span);
                    }
                    Entry::Occupied(_) => self.errors.push(CompileError::DuplicateFunction {
                        span: decl.span,
                        name: decl.name.as_str().to_string(),
                        package: pkg_display(name),
                    }),
                }
            }
            let mut records = parsed.records;
            for record in &mut records {
                rebase::shift(&mut record.span, base);
                for field in &mut record.fields {
                    rebase::shift_type(&mut field.ty, base);
                }
                data.record_names.insert(record.name.clone());
                if record.private {
                    data.private_records.insert(record.name.clone());
                }
            }
            let mut consts = parsed.consts;
            for decl in &mut consts {
                rebase::shift_const(decl, base);
                if decl.private {
                    data.private_consts.insert(decl.name.clone());
                }
                match data.const_exports.entry(decl.name.clone()) {
                    Entry::Vacant(v) => {
                        v.insert(decl.span);
                    }
                    Entry::Occupied(_) => self.errors.push(CompileError::DuplicateConstant {
                        span: decl.span,
                        name: decl.name.as_str().to_string(),
                        package: pkg_display(name),
                    }),
                }
            }
            let mut enums = parsed.enums;
            for decl in &mut enums {
                rebase::shift_enum(decl, base);
                if decl.private {
                    data.private_enums.insert(decl.name.clone());
                }
                let mut cases: Vec<Symbol> = Vec::with_capacity(decl.cases.len());
                for (case, span) in &decl.cases {
                    if cases.contains(case) {
                        self.errors.push(CompileError::DuplicateEnumCase {
                            span: *span,
                            enum_name: decl.name.as_str().to_string(),
                            name: case.as_str().to_string(),
                        });
                    } else {
                        cases.push(case.clone());
                    }
                }
                match data.enum_exports.entry(decl.name.clone()) {
                    Entry::Vacant(v) => {
                        v.insert(cases);
                    }
                    Entry::Occupied(_) => self.errors.push(CompileError::DuplicateEnum {
                        span: decl.span,
                        name: decl.name.as_str().to_string(),
                        package: pkg_display(name),
                    }),
                }
            }
            modules.push(ParsedModule {
                imports,
                views,
                fns,
                records,
                consts,
                enums,
            });
        }

        if let Some(pkg) = self.packages.get_mut(name) {
            *pkg = PackageData {
                modules,
                exports,
                fn_exports,
                ..data
            };
        }

        // Load dependencies depth-first, in first-`use` order (deterministic).
        for (dep, span) in dep_requests {
            if self.packages.contains_key(&dep) {
                // Already loaded, an edge back into the active chain is a cycle.
                if stack.contains(&dep) {
                    let mut path: Vec<&str> = stack
                        .iter()
                        .skip_while(|p| **p != dep)
                        .map(String::as_str)
                        .collect();
                    path.push(dep.as_str());
                    let path = path
                        .iter()
                        .map(|p| if p.is_empty() { "this project" } else { p })
                        .collect::<Vec<_>>()
                        .join(" → ");
                    self.errors.push(CompileError::PackageCycle { span, path });
                }
                continue;
            }
            match self.provider.package_files(name, &dep) {
                Ok(files) => self.load_package(&dep, files, stack),
                Err(detail) => self.errors.push(CompileError::UnknownPackage {
                    span,
                    name: dep,
                    detail,
                }),
            }
        }

        stack.pop();
    }

    /// Second pass: for every file, build its import maps, validate them, and
    /// rewrite element references + declared names to canonical form.
    fn rewrite_all(&mut self) {
        let pkg_names: Vec<String> = self.order.clone();
        for pkg_name in &pkg_names {
            let n_modules = self.packages.get(pkg_name).map_or(0, |p| p.modules.len());
            for module_idx in 0..n_modules {
                self.rewrite_module(pkg_name, module_idx);
            }
        }
    }

    fn rewrite_module(&mut self, pkg_name: &str, module_idx: usize) {
        // ---- build this file's import maps ----
        let imports: Vec<UseDecl> = self.packages[pkg_name].modules[module_idx].imports.clone();
        // alias → target package
        let mut aliases: HashMap<Symbol, String> = HashMap::new();
        // bare imported view name → target package
        let mut bare: HashMap<Symbol, String> = HashMap::new();
        let local_exports: Vec<(Symbol, Span)> = self.packages[pkg_name]
            .exports
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();

        for import in &imports {
            let target = import.package.as_str().to_string();
            if !self.packages.contains_key(&target) {
                continue; // already reported as UnknownPackage / cycle
            }
            if let Some(symbols) = &import.symbols {
                // Selective form: `use pkg.{A, B}`, binds bare names only.
                for (sym, span) in symbols {
                    self.check_selective_symbol(&target, sym, *span, &local_exports, &mut bare);
                }
            } else {
                let alias = import
                    .alias
                    .clone()
                    .unwrap_or_else(|| import.package.clone());
                match aliases.entry(alias.clone()) {
                    Entry::Vacant(v) => {
                        v.insert(target);
                    }
                    Entry::Occupied(o) => self.errors.push(CompileError::NameCollision {
                        span: import.span,
                        name: alias.as_str().to_string(),
                        first: pkg_display(o.get()),
                        second: pkg_display(&target),
                    }),
                }
            }
        }

        // A package reaches its own views by its own name, as its callers do
        // (`material.Button`), which is the way to name one that a built-in of
        // the same name hides when written bare. A `use` of that name wins.
        if let Some(own) = self.prefix(pkg_name) {
            aliases
                .entry(Symbol::intern(own))
                .or_insert_with(|| pkg_name.to_string());
        }

        // ---- canonicalize declared names + rewrite bodies ----
        let local: HashMap<Symbol, Span> = self.packages[pkg_name].exports.clone();
        let mut views =
            std::mem::take(&mut self.packages.get_mut(pkg_name).unwrap().modules[module_idx].views);
        let mut errors = Vec::new();
        for view in &mut views {
            if let Some(prefix) = self.prefix(pkg_name) {
                view.name = Symbol::intern(&format!("{prefix}.{}", view.name.as_str()));
            }
            for member in &mut view.body {
                self.rewrite_member(member, pkg_name, &local, &aliases, &bare, &mut errors);
            }
        }
        // Function calls: `alias.helper(...)` becomes the canonical
        // `pkg.helper(...)`, and inside a package a bare call to one of its own
        // functions becomes canonical too, unless the view shadows the name.
        let mut fns =
            std::mem::take(&mut self.packages.get_mut(pkg_name).unwrap().modules[module_idx].fns);
        let own_fns: std::collections::HashSet<Symbol> =
            self.packages[pkg_name].fn_exports.keys().cloned().collect();
        for view in &mut views {
            let shadowed = view_bindings(view);
            for member in &mut view.body {
                walk::member_exprs(member, &mut |e| {
                    self.rewrite_call(e, pkg_name, &own_fns, &shadowed, &aliases, &mut errors);
                });
            }
        }
        for decl in &mut fns {
            let shadowed: std::collections::HashSet<Symbol> =
                decl.params.iter().map(|p| p.name.clone()).collect();
            walk::expr(&mut decl.body, &mut |e| {
                self.rewrite_call(e, pkg_name, &own_fns, &shadowed, &aliases, &mut errors);
            });
            if let Some(prefix) = self.prefix(pkg_name) {
                decl.name = Symbol::intern(&format!("{prefix}.{}", decl.name.as_str()));
            }
        }

        // Constants, enums and the types that name them (RFC-0055): a value
        // or a type written as `gap`, `m.gap`, `Variant.tonal` or `m.Variant`
        // becomes the canonical name of the declaration it means.
        let mut consts = std::mem::take(
            &mut self.packages.get_mut(pkg_name).unwrap().modules[module_idx].consts,
        );
        let mut enums =
            std::mem::take(&mut self.packages.get_mut(pkg_name).unwrap().modules[module_idx].enums);
        let mut records = std::mem::take(
            &mut self.packages.get_mut(pkg_name).unwrap().modules[module_idx].records,
        );
        for view in &mut views {
            let shadowed = view_binders(view);
            let sc = ValueScope {
                pkg_name,
                aliases: &aliases,
                shadowed: &shadowed,
            };
            for p in &mut view.params {
                if let Some(default) = &mut p.default {
                    walk::expr(default, &mut |e| self.rewrite_value(e, &sc, &mut errors));
                }
            }
            for member in &mut view.body {
                walk::member_exprs(member, &mut |e| self.rewrite_value(e, &sc, &mut errors));
            }
            walk::view_types(view, &mut |t| {
                self.rewrite_type(t, pkg_name, &aliases, &mut errors);
            });
        }
        for decl in &mut fns {
            let shadowed: std::collections::HashSet<Symbol> =
                decl.params.iter().map(|p| p.name.clone()).collect();
            let sc = ValueScope {
                pkg_name,
                aliases: &aliases,
                shadowed: &shadowed,
            };
            walk::expr(&mut decl.body, &mut |e| {
                self.rewrite_value(e, &sc, &mut errors);
            });
            for p in &mut decl.params {
                walk::param_types(p, &mut |t| {
                    self.rewrite_type(t, pkg_name, &aliases, &mut errors);
                });
            }
            if let Some(ret) = &mut decl.ret {
                walk::ty(ret, &mut |t| {
                    self.rewrite_type(t, pkg_name, &aliases, &mut errors);
                });
            }
        }
        let no_shadow = std::collections::HashSet::new();
        let sc = ValueScope {
            pkg_name,
            aliases: &aliases,
            shadowed: &no_shadow,
        };
        for decl in &mut consts {
            walk::expr(&mut decl.init, &mut |e| {
                self.rewrite_value(e, &sc, &mut errors);
            });
            // Own calls to functions, like a function's own.
            let own_fns = &own_fns;
            walk::expr(&mut decl.init, &mut |e| {
                self.rewrite_call(e, pkg_name, own_fns, &no_shadow, &aliases, &mut errors);
            });
            if let Some(ty) = &mut decl.ty {
                walk::ty(ty, &mut |t| {
                    self.rewrite_type(t, pkg_name, &aliases, &mut errors);
                });
            }
            if let Some(prefix) = self.prefix(pkg_name) {
                decl.name = Symbol::intern(&format!("{prefix}.{}", decl.name.as_str()));
            }
        }
        for decl in &mut enums {
            if let Some(prefix) = self.prefix(pkg_name) {
                decl.name = Symbol::intern(&format!("{prefix}.{}", decl.name.as_str()));
            }
        }
        for decl in &mut records {
            for field in &mut decl.fields {
                walk::ty(&mut field.ty, &mut |t| {
                    self.rewrite_type(t, pkg_name, &aliases, &mut errors);
                });
            }
        }
        let module = &mut self.packages.get_mut(pkg_name).unwrap().modules[module_idx];
        module.views = views;
        module.fns = fns;
        module.consts = consts;
        module.enums = enums;
        module.records = records;
        self.errors.extend(errors);
    }

    /// `prefix.name` for a declaration of package `pkg`, the name the rest of
    /// the pipeline sees. The root project has no prefix.
    fn canonical(&self, pkg: &str, name: &str) -> Symbol {
        match self.prefix(pkg) {
            Some(prefix) => Symbol::intern(&format!("{prefix}.{name}")),
            None => Symbol::intern(name),
        }
    }

    /// Reports `name` of package `target` as private when `pkg_name`, the
    /// package using it, is another one.
    fn deny_private(
        pkg_name: &str,
        target: &str,
        private: bool,
        name: &str,
        span: Span,
        errors: &mut Vec<CompileError>,
    ) {
        if private && pkg_name != target {
            errors.push(CompileError::PrivateName {
                span,
                name: name.to_string(),
                package: pkg_display(target),
            });
        }
    }

    /// The names of `target` that `pkg_name` may suggest and use: every one
    /// when it is the package itself, the public ones otherwise.
    fn visible<'a>(
        pkg_name: &str,
        target: &str,
        names: impl Iterator<Item = &'a Symbol>,
        private: &std::collections::HashSet<Symbol>,
    ) -> Vec<&'a str> {
        names
            .filter(|n| pkg_name == target || !private.contains(*n))
            .map(Symbol::as_str)
            .collect()
    }

    /// Rewrites one expression node that names a constant or an enum to the
    /// canonical form (RFC-0055 §2).
    fn rewrite_value(&self, e: &mut Expr, sc: &ValueScope<'_>, errors: &mut Vec<CompileError>) {
        match e {
            // A bare constant of this package, inside the package.
            Expr::Ident(name, _) => {
                if self.prefix(sc.pkg_name).is_some()
                    && !sc.shadowed.contains(name)
                    && self.packages[sc.pkg_name].const_exports.contains_key(name)
                {
                    *name = self.canonical(sc.pkg_name, name.as_str());
                }
            }
            Expr::Member { base, field, span } => match base.as_mut() {
                // `m.gap`, `Variant.tonal` (the enum of this package).
                Expr::Ident(alias, _) if !sc.shadowed.contains(alias) => {
                    if let Some(target) = sc.aliases.get(alias) {
                        let pkg = &self.packages[target];
                        if pkg.const_exports.contains_key(field) {
                            Self::deny_private(
                                sc.pkg_name,
                                target,
                                pkg.private_consts.contains(field),
                                field.as_str(),
                                *span,
                                errors,
                            );
                            *e = Expr::Ident(self.canonical(target, field.as_str()), *span);
                        } else if !(pkg.enum_exports.contains_key(field)
                            || pkg.fn_exports.contains_key(field)
                            || pkg.exports.contains_key(field))
                        {
                            let names = Self::visible(
                                sc.pkg_name,
                                target,
                                pkg.const_exports
                                    .keys()
                                    .chain(pkg.enum_exports.keys())
                                    .chain(pkg.fn_exports.keys())
                                    .chain(pkg.exports.keys()),
                                &pkg.private_consts
                                    .iter()
                                    .chain(&pkg.private_enums)
                                    .chain(&pkg.private_fns)
                                    .chain(&pkg.private_views)
                                    .cloned()
                                    .collect(),
                            );
                            errors.push(CompileError::UnknownImportSymbol {
                                span: *span,
                                package: target.clone(),
                                name: field.as_str().to_string(),
                                hint: closest_match(field.as_str(), names).map(str::to_string),
                            });
                        }
                    } else if self.prefix(sc.pkg_name).is_some()
                        && self.packages[sc.pkg_name].enum_exports.contains_key(alias)
                    {
                        *alias = self.canonical(sc.pkg_name, alias.as_str());
                    }
                }
                // `m.Variant.tonal`.
                Expr::Member {
                    base: inner,
                    field: enum_name,
                    span: inner_span,
                } => {
                    let Expr::Ident(alias, _) = inner.as_ref() else {
                        return;
                    };
                    if sc.shadowed.contains(alias) {
                        return;
                    }
                    let Some(target) = sc.aliases.get(alias) else {
                        return;
                    };
                    let pkg = &self.packages[target];
                    if pkg.enum_exports.contains_key(enum_name) {
                        Self::deny_private(
                            sc.pkg_name,
                            target,
                            pkg.private_enums.contains(enum_name),
                            enum_name.as_str(),
                            *inner_span,
                            errors,
                        );
                        **base =
                            Expr::Ident(self.canonical(target, enum_name.as_str()), *inner_span);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Rewrites one type annotation that names an enum or a record of
    /// another package, or an enum of this one, to its canonical name.
    fn rewrite_type(
        &self,
        t: &mut Type,
        pkg_name: &str,
        aliases: &HashMap<Symbol, String>,
        errors: &mut Vec<CompileError>,
    ) {
        let Type::Named { name, span, .. } = t else {
            return;
        };
        if let Some((head, rest)) = name.as_str().split_once('.') {
            let rest = Symbol::intern(rest);
            let Some(target) = aliases.get(&Symbol::intern(head)) else {
                errors.push(CompileError::UnknownPackage {
                    span: *span,
                    name: head.to_string(),
                    detail: format!("no `use {head}` in this file"),
                });
                return;
            };
            let pkg = &self.packages[target];
            if pkg.enum_exports.contains_key(&rest) {
                Self::deny_private(
                    pkg_name,
                    target,
                    pkg.private_enums.contains(&rest),
                    rest.as_str(),
                    *span,
                    errors,
                );
                *name = self.canonical(target, rest.as_str());
            } else if pkg.record_names.contains(&rest) {
                Self::deny_private(
                    pkg_name,
                    target,
                    pkg.private_records.contains(&rest),
                    rest.as_str(),
                    *span,
                    errors,
                );
                // Record types are program-wide and keep their bare names.
                *name = rest;
            } else {
                let names = Self::visible(
                    pkg_name,
                    target,
                    pkg.enum_exports.keys().chain(&pkg.record_names),
                    &pkg.private_enums
                        .iter()
                        .chain(&pkg.private_records)
                        .cloned()
                        .collect(),
                );
                errors.push(CompileError::UnknownImportSymbol {
                    span: *span,
                    package: target.clone(),
                    name: rest.as_str().to_string(),
                    hint: closest_match(rest.as_str(), names).map(str::to_string),
                });
            }
            return;
        }
        // A bare name: an enum of this package, or a record another package
        // keeps private.
        if self.prefix(pkg_name).is_some()
            && self.packages[pkg_name].enum_exports.contains_key(name)
        {
            *name = self.canonical(pkg_name, name.as_str());
        } else if !self.packages[pkg_name].record_names.contains(name) {
            for (other, pkg) in &self.packages {
                if other != pkg_name && pkg.private_records.contains(name) {
                    Self::deny_private(pkg_name, other, true, name.as_str(), *span, errors);
                    break;
                }
            }
        }
    }

    /// Rewrites one call to a top-level function to its canonical name.
    fn rewrite_call(
        &self,
        e: &mut Expr,
        pkg_name: &str,
        own_fns: &std::collections::HashSet<Symbol>,
        shadowed: &std::collections::HashSet<Symbol>,
        aliases: &HashMap<Symbol, String>,
        errors: &mut Vec<CompileError>,
    ) {
        let Expr::Call { callee, .. } = e else {
            return;
        };
        match callee.as_mut() {
            Expr::Member { base, field, span } => {
                let Expr::Ident(alias, _) = base.as_ref() else {
                    return;
                };
                let Some(target) = aliases.get(alias) else {
                    return; // not a package: a controller handle, a record
                };
                if self.packages[target].fn_exports.contains_key(field) {
                    let span = *span;
                    Self::deny_private(
                        pkg_name,
                        target,
                        self.packages[target].private_fns.contains(field),
                        field.as_str(),
                        span,
                        errors,
                    );
                    let prefix = self.prefix(target).unwrap_or_default();
                    **callee = Expr::Ident(
                        Symbol::intern(&format!("{prefix}.{}", field.as_str())),
                        span,
                    );
                } else {
                    let hint = closest_match(
                        field.as_str(),
                        Self::visible(
                            pkg_name,
                            target,
                            self.packages[target].fn_exports.keys(),
                            &self.packages[target].private_fns,
                        ),
                    )
                    .map(str::to_string);
                    errors.push(CompileError::UnknownImportSymbol {
                        span: *span,
                        package: target.clone(),
                        name: field.as_str().to_string(),
                        hint,
                    });
                }
            }
            Expr::Ident(name, _)
                if self.prefix(pkg_name).is_some()
                    && own_fns.contains(name)
                    && !shadowed.contains(name) =>
            {
                let prefix = self.prefix(pkg_name).unwrap_or_default();
                *name = Symbol::intern(&format!("{prefix}.{}", name.as_str()));
            }
            _ => {}
        }
    }

    /// Validates one `use pkg.{sym}` entry and records the bare binding.
    fn check_selective_symbol(
        &mut self,
        target: &str,
        sym: &Symbol,
        span: Span,
        local_exports: &[(Symbol, Span)],
        bare: &mut HashMap<Symbol, String>,
    ) {
        let exports = &self.packages[target].exports;
        if exports.contains_key(sym) && self.packages[target].private_views.contains(sym) {
            self.errors.push(CompileError::PrivateName {
                span,
                name: sym.as_str().to_string(),
                package: pkg_display(target),
            });
        }
        let exports = &self.packages[target].exports;
        if !exports.contains_key(sym) {
            let names = Self::visible(
                "",
                target,
                exports.keys(),
                &self.packages[target].private_views,
            );
            let hint = closest_match(sym.as_str(), names).map(str::to_string);
            self.errors.push(CompileError::UnknownImportSymbol {
                span,
                package: target.to_string(),
                name: sym.as_str().to_string(),
                hint,
            });
            // Still record the binding so downstream references resolve to
            // *something* and don't cascade extra UnknownView noise.
        }
        if intrinsics::lookup(sym.as_str()).is_some() {
            self.errors.push(CompileError::ImportShadowsBuiltin {
                span,
                name: sym.as_str().to_string(),
                package: target.to_string(),
            });
            return; // the intrinsic always wins; don't bind
        }
        if local_exports.iter().any(|(name, _)| name == sym) {
            self.errors.push(CompileError::NameCollision {
                span,
                name: sym.as_str().to_string(),
                first: pkg_display(target),
                second: "a view declared here".to_string(),
            });
            return; // the local view wins deterministically
        }
        match bare.entry(sym.clone()) {
            Entry::Vacant(v) => {
                v.insert(target.to_string());
            }
            Entry::Occupied(o) => {
                if o.get() != target {
                    self.errors.push(CompileError::NameCollision {
                        span,
                        name: sym.as_str().to_string(),
                        first: pkg_display(o.get()),
                        second: pkg_display(target),
                    });
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rewrite_member(
        &self,
        member: &mut Member,
        pkg_name: &str,
        local: &HashMap<Symbol, Span>,
        aliases: &HashMap<Symbol, String>,
        bare: &HashMap<Symbol, String>,
        errors: &mut Vec<CompileError>,
    ) {
        match member {
            Member::Element(el) => {
                self.rewrite_element(el, pkg_name, local, aliases, bare, errors);
            }
            // A `for` body and an RFC-0026 `route`/`tab` body are both plain
            // member lists that may name package-qualified views.
            Member::For { body, .. } | Member::Route { body, .. } => {
                for m in body {
                    self.rewrite_member(m, pkg_name, local, aliases, bare, errors);
                }
            }
            Member::When { then, els, .. } => {
                for m in then {
                    self.rewrite_member(m, pkg_name, local, aliases, bare, errors);
                }
                if let Some(els) = els {
                    for m in els {
                        self.rewrite_member(m, pkg_name, local, aliases, bare, errors);
                    }
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rewrite_element(
        &self,
        el: &mut ElementNode,
        pkg_name: &str,
        local: &HashMap<Symbol, Span>,
        aliases: &HashMap<Symbol, String>,
        bare: &HashMap<Symbol, String>,
        errors: &mut Vec<CompileError>,
    ) {
        let name = el.name.as_str();
        if let Some((head, rest)) = name.split_once('.') {
            // Qualified reference `alias.View` (Pillar B).
            if let Some(target) = aliases.get(&Symbol::intern(head)) {
                if self.packages[target]
                    .exports
                    .contains_key(&Symbol::intern(rest))
                {
                    Self::deny_private(
                        pkg_name,
                        target,
                        self.packages[target]
                            .private_views
                            .contains(&Symbol::intern(rest)),
                        rest,
                        Span::new(el.span.start, el.span.start + name.len() as u32),
                        errors,
                    );
                } else {
                    let hint = closest_match(
                        rest,
                        Self::visible(
                            pkg_name,
                            target,
                            self.packages[target].exports.keys(),
                            &self.packages[target].private_views,
                        ),
                    )
                    .map(str::to_string);
                    errors.push(CompileError::UnknownImportSymbol {
                        span: el.span,
                        package: target.clone(),
                        name: rest.to_string(),
                        hint,
                    });
                }
                let prefix = self.prefix(target).unwrap_or_default();
                el.name = Symbol::intern(&format!("{prefix}.{rest}"));
            } else {
                errors.push(CompileError::UnknownPackage {
                    span: el.span,
                    name: head.to_string(),
                    detail: format!("no `use {head}` in this file"),
                });
            }
        } else if intrinsics::lookup(name).is_some() {
            // The closed intrinsic catalog always wins (RFC-0005).
        } else if local.contains_key(&el.name) {
            if let Some(prefix) = self.prefix(pkg_name) {
                el.name = Symbol::intern(&format!("{prefix}.{name}"));
            }
        } else if let Some(target) = bare.get(&el.name) {
            el.name = Symbol::intern(&format!("{target}.{name}"));
        }
        // Anything else stays as written; ViewTable validation reports
        // UnknownView with hints over the canonical name set.

        for child in &mut el.children {
            self.rewrite_member(child, pkg_name, local, aliases, bare, errors);
        }
        // RFC-0062: the blocks handed to a `Slot` name views too.
        let slot_values =
            el.content
                .iter_mut()
                .map(|a| &mut a.value)
                .chain(el.attrs.iter_mut().filter_map(|a| match &mut a.kind {
                    crate::parser::ast::AttrKind::Prop { value } => Some(value),
                    _ => None,
                }));
        for value in slot_values {
            let members = match value {
                Expr::SlotBlock { members, .. }
                | Expr::Lambda {
                    slot: Some(members),
                    ..
                } => Some(members),
                _ => None,
            };
            for child in members.into_iter().flatten() {
                self.rewrite_member(child, pkg_name, local, aliases, bare, errors);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::eval::Interpreter;
    use std::collections::BTreeMap;

    /// An in-memory provider: package name → files.
    struct MemProvider(BTreeMap<&'static str, Vec<(&'static str, &'static str)>>);

    impl PackageProvider for MemProvider {
        fn package_files(
            &mut self,
            _dependent: &str,
            package: &str,
        ) -> Result<Vec<SourceFile>, String> {
            self.0
                .get(package)
                .map(|files| {
                    files
                        .iter()
                        .map(|(name, src)| SourceFile {
                            name: (*name).to_string(),
                            source: (*src).to_string(),
                        })
                        .collect()
                })
                .ok_or_else(|| "not declared in `[dependencies]`".to_string())
        }
    }

    fn root(src: &str) -> Vec<SourceFile> {
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: src.to_string(),
        }]
    }

    fn material() -> (&'static str, Vec<(&'static str, &'static str)>) {
        (
            "material",
            vec![
                ("material/buttons.byd", "View Card() { Text(\"card\") }"),
                (
                    "material/chips.byd",
                    "View Chip(label: Str = \"chip\") { Text(label) }",
                ),
            ],
        )
    }

    fn names(program: &ResolvedProgram) -> Vec<&str> {
        program.views.iter().map(|v| v.name.as_str()).collect()
    }

    /// The element names of a view's body, flattened.
    fn body_names(view: &ViewDecl) -> Vec<&str> {
        view.body
            .iter()
            .filter_map(|m| match m {
                Member::Element(el) => Some(el.name.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn alias_import_rewrites_to_canonical_names() {
        let mut p = MemProvider(BTreeMap::from([material()]));
        let program = resolve_program(root("use material as m\nView App() { m.Card() }"), &mut p);
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        assert_eq!(names(&program), ["App", "material.Card", "material.Chip"]);
        assert_eq!(body_names(&program.views[0]), ["material.Card"]);
    }

    #[test]
    fn whole_package_import_uses_the_package_name_as_namespace() {
        let mut p = MemProvider(BTreeMap::from([material()]));
        let program = resolve_program(root("use material\nView App() { material.Chip() }"), &mut p);
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        assert_eq!(body_names(&program.views[0]), ["material.Chip"]);
    }

    #[test]
    fn selective_import_binds_bare_names() {
        let mut p = MemProvider(BTreeMap::from([material()]));
        let program = resolve_program(
            root("use material.{Card, Chip}\nView App() { Card()\nChip() }"),
            &mut p,
        );
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        assert_eq!(
            body_names(&program.views[0]),
            ["material.Card", "material.Chip"]
        );
    }

    #[test]
    fn unknown_package_is_reported_with_the_provider_detail() {
        let mut p = MemProvider(BTreeMap::new());
        let program = resolve_program(root("use nope\nView App() { Text(\"x\") }"), &mut p);
        assert!(matches!(
            &program.errors[0],
            CompileError::UnknownPackage { name, detail, .. }
                if name == "nope" && detail.contains("dependencies")
        ));
    }

    #[test]
    fn unknown_import_symbol_gets_a_hint() {
        let mut p = MemProvider(BTreeMap::from([material()]));
        let program = resolve_program(
            root("use material.{Cardd}\nView App() { Text(\"x\") }"),
            &mut p,
        );
        assert!(matches!(
            &program.errors[0],
            CompileError::UnknownImportSymbol { name, hint: Some(h), .. }
                if name == "Cardd" && h == "Card"
        ));
    }

    #[test]
    fn qualified_reference_to_a_missing_view_gets_a_hint() {
        let mut p = MemProvider(BTreeMap::from([material()]));
        let program = resolve_program(root("use material as m\nView App() { m.Cardz() }"), &mut p);
        assert!(matches!(
            &program.errors[0],
            CompileError::UnknownImportSymbol { package, hint: Some(h), .. }
                if package == "material" && h == "Card"
        ));
    }

    #[test]
    fn bare_name_collision_across_two_packages_demands_an_alias() {
        let mut p = MemProvider(BTreeMap::from([
            ("a", vec![("a/x.byd", "View Card() { Text(\"a\") }")]),
            ("b", vec![("b/x.byd", "View Card() { Text(\"b\") }")]),
        ]));
        let program = resolve_program(
            root("use a.{Card}\nuse b.{Card}\nView App() { Card() }"),
            &mut p,
        );
        assert!(
            program
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::NameCollision { name, .. } if name == "Card")),
            "{:?}",
            program.errors
        );
        // Deterministic winner: the first binding (package `a`).
        assert_eq!(body_names(&program.views[0]), ["a.Card"]);
    }

    #[test]
    fn duplicate_alias_is_a_collision() {
        let mut p = MemProvider(BTreeMap::from([
            ("a", vec![("a/x.byd", "View X() { Text(\"a\") }")]),
            ("b", vec![("b/x.byd", "View Y() { Text(\"b\") }")]),
        ]));
        let program = resolve_program(root("use a as m\nuse b as m\nView App() { m.X() }"), &mut p);
        assert!(
            program
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::NameCollision { name, .. } if name == "m"))
        );
    }

    #[test]
    fn selective_import_shadowing_an_intrinsic_is_rejected() {
        let mut p = MemProvider(BTreeMap::from([(
            "a",
            vec![("a/x.byd", "View Button() { Text(\"a\") }")],
        )]));
        let program = resolve_program(
            root("use a.{Button}\nView App() { Button(\"ok\") }"),
            &mut p,
        );
        assert!(program.errors.iter().any(
            |e| matches!(e, CompileError::ImportShadowsBuiltin { name, package, .. } if name == "Button" && package == "a")
        ));
        // The intrinsic wins: the reference stays bare.
        assert_eq!(body_names(&program.views[0]), ["Button"]);
    }

    #[test]
    fn qualified_access_reaches_an_intrinsic_named_package_view() {
        // `Button` as a *package* view is unreachable bare, but reachable
        // qualified, namespacing makes it unambiguous.
        let mut p = MemProvider(BTreeMap::from([(
            "a",
            vec![("a/x.byd", "View Button() { Text(\"a\") }")],
        )]));
        let program = resolve_program(root("use a\nView App() { a.Button() }"), &mut p);
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        assert_eq!(body_names(&program.views[0]), ["a.Button"]);
    }

    #[test]
    fn a_library_declares_a_builtin_name_and_reaches_it_by_its_own_name() {
        // Checked alone, the package is named `a`; a bare `Button` inside is
        // the built-in, `a.Button` is its own view.
        let mut p = MemProvider(BTreeMap::new());
        let program = resolve_library(
            "a",
            vec![SourceFile {
                name: "a/x.byd".into(),
                source: "View Button(l: Str) { Button(l) }\nView Pair() { a.Button(\"x\") }".into(),
            }],
            &mut p,
        );
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        assert_eq!(names(&program), ["a.Button", "a.Pair"]);
        assert_eq!(body_names(&program.views[0]), ["Button"]);
        assert_eq!(body_names(&program.views[1]), ["a.Button"]);
        let (_, errs) = crate::interp::views::ViewTable::build(&program.views);
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn a_dependency_reaches_its_own_views_by_its_name_too() {
        let mut p = MemProvider(BTreeMap::from([(
            "a",
            vec![(
                "a/x.byd",
                "View Button(l: Str) { Button(l) }\nView Pair() { a.Button(\"x\") }",
            )],
        )]));
        let program = resolve_program(
            root("use a as m\nView App() { m.Pair()\nButton(\"b\")\nm.Button(\"c\") }"),
            &mut p,
        );
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        assert_eq!(
            body_names(&program.views[0]),
            ["a.Pair", "Button", "a.Button"]
        );
    }

    #[test]
    fn a_builtin_named_view_calling_itself_is_recursion() {
        let mut p = MemProvider(BTreeMap::new());
        let program = resolve_library(
            "a",
            vec![SourceFile {
                name: "a/x.byd".into(),
                source: "View Button(l: Str) { a.Button(l) }".into(),
            }],
            &mut p,
        );
        let mut interp = Interpreter::new();
        let diags = interp.load_views(&program.views);
        assert!(
            diags
                .iter()
                .any(|e| matches!(e, CompileError::RecursiveView { .. })),
            "{diags:?}"
        );
    }

    #[test]
    fn package_cycle_is_detected() {
        let mut p = MemProvider(BTreeMap::from([
            ("a", vec![("a/x.byd", "use b\nView X() { Text(\"a\") }")]),
            ("b", vec![("b/x.byd", "use a\nView Y() { Text(\"b\") }")]),
        ]));
        let program = resolve_program(root("use a\nView App() { a.X() }"), &mut p);
        assert!(
            program
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::PackageCycle { path, .. } if path.contains("a → b → a"))),
            "{:?}",
            program.errors
        );
    }

    #[test]
    fn duplicate_view_across_package_files_is_reported() {
        let mut p = MemProvider(BTreeMap::from([(
            "a",
            vec![
                ("a/one.byd", "View X() { Text(\"1\") }"),
                ("a/two.byd", "View X() { Text(\"2\") }"),
            ],
        )]));
        let program = resolve_program(root("use a\nView App() { a.X() }"), &mut p);
        assert!(
            program
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::DuplicateViewName { name, .. } if name == "X"))
        );
    }

    #[test]
    fn root_project_is_one_namespace_across_files() {
        // Two root files; sibling views resolve with no `use` at all.
        let mut p = MemProvider(BTreeMap::new());
        let program = resolve_program(
            vec![
                SourceFile {
                    name: "main.byd".into(),
                    source: "View App() { Header() }".into(),
                },
                SourceFile {
                    name: "header.byd".into(),
                    source: "View Header() { Text(\"h\") }".into(),
                },
            ],
            &mut p,
        );
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        assert_eq!(names(&program), ["App", "Header"]);
    }

    #[test]
    fn transitive_dependencies_resolve() {
        let mut p = MemProvider(BTreeMap::from([
            (
                "material",
                vec![(
                    "material/card.byd",
                    "use icons\nView Card() { icons.Star() }",
                )],
            ),
            (
                "icons",
                vec![("icons/star.byd", "View Star() { Text(\"*\") }")],
            ),
        ]));
        let program = resolve_program(root("use material as m\nView App() { m.Card() }"), &mut p);
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        assert_eq!(names(&program), ["App", "material.Card", "icons.Star"]);
        // The package-internal reference was rewritten too.
        assert_eq!(body_names(&program.views[1]), ["icons.Star"]);
    }

    #[test]
    fn source_map_locates_errors_in_the_right_file() {
        let mut p = MemProvider(BTreeMap::from([(
            "a",
            vec![("a/x.byd", "View X() { Text(\"ok\") }\nView Y() { Bogus() }")],
        )]));
        let program = resolve_program(root("use a\nView App() { a.X() }"), &mut p);
        // Lower the program so UnknownView for `Bogus` fires with a rebased span.
        let mut interp = Interpreter::new();
        interp.load_views(&program.views);
        let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
        for view in &program.views {
            let _ = interp.lower_view(view, &known);
        }
        let unknown = interp
            .errors()
            .iter()
            .find(|e| matches!(e, CompileError::UnknownView { name, .. } if name == "Bogus"))
            .expect("Bogus must be reported");
        let (entry, local) = program.source_map.locate(unknown.span()).unwrap();
        assert_eq!(entry.file, "a/x.byd");
        let (line, _col) = line_col(&entry.source, local.start as usize);
        assert_eq!(line, 2, "Bogus is on line 2 of a/x.byd");
    }

    #[test]
    fn resolved_program_lowers_end_to_end() {
        let mut p = MemProvider(BTreeMap::from([material()]));
        let program = resolve_program(
            root("use material.{Chip}\nView App() { Column #[gap: 8] { Chip()\nChip(\"two\") } }"),
            &mut p,
        );
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        let mut interp = Interpreter::new();
        interp.load_views(&program.views);
        let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
        let tree = interp.lower_view(&program.views[0], &known);
        assert!(
            interp.errors().is_empty(),
            "lowering must be clean: {:?}",
            interp.errors()
        );
        assert!(!tree.is_empty(), "{:?}", tree);
    }

    #[test]
    fn import_after_view_is_reported() {
        let parsed = parse("View A() { Text(\"x\") }\nuse material");
        assert!(
            parsed
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::ImportAfterView { .. }))
        );
        // The decl is still recorded for resolver recovery.
        assert_eq!(parsed.imports.len(), 1);
    }
}
