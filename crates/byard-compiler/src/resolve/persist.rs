//! `store Settings persist "settings" { … }`: a store kept between runs.
//!
//! The resolver writes, into the view the program runs, what a person would
//! write by hand with the app's `Store` capability, and nothing is added to
//! the interpreter:
//!
//! ```text
//! inject Store as __byard_store
//! inject Settings as __byard_Settings
//! on mount => {
//!     __byard_store.get("settings", {}) ok saved => { __byard_Settings.dark = saved.dark as Bool } err e => {}
//!     …one for each field…
//! }
//! on change(__byard_Settings.dark, …) => {
//!     __byard_store.set("settings", { dark: __byard_Settings.dark, … })
//! }
//! ```
//!
//! Each field is read on its own, through `as` with its type, so a field the
//! saved record lacks (it was added since) or holds as something else keeps
//! its default and the others are still read. Every change of a field writes
//! the whole store. Being ordinary code, it is checked like any other, and a
//! hot reload, which mounts the view again, reads the store back.

use crate::edition::Edition;
use crate::parser::ast::{RecordDecl, ViewDecl, root_view};

use super::generics::spell;
use super::rebase;

/// The view body that keeps every store with a `persist` key, added to the
/// view `views` runs. Nothing when no store has one, or there is no view.
pub fn keep(views: &mut [ViewDecl], records: &[RecordDecl], edition: Edition) {
    let kept: Vec<&RecordDecl> = records
        .iter()
        .filter(|r| r.store && r.persist.is_some())
        .collect();
    if kept.is_empty() {
        return;
    }
    let Some(root) = root_view(views).map(|v| v.name.clone()) else {
        return;
    };
    let Some(view) = views.iter_mut().find(|v| v.name == root) else {
        return;
    };
    for decl in kept {
        let Some(key) = &decl.persist else {
            continue;
        };
        let source = source_of(decl, key);
        let mut parsed = crate::parser::parse_in(&source, edition);
        let Some(mut made) = parsed.views.pop() else {
            continue;
        };
        if !parsed.errors.is_empty() {
            continue;
        }
        // What it says it is, at the store it keeps.
        rebase::shift_view(&mut made, decl.span.start);
        view.body.extend(made.body);
    }
}

/// The text of the view whose body keeps `decl` under `key`.
fn source_of(decl: &RecordDecl, key: &str) -> String {
    use std::fmt::Write as _;
    let store = decl.name.as_str();
    let handle = format!("__byard_{}", store.replace('.', "_"));
    let key = key
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('{', "\\{");
    let mut s = String::new();
    let _ = writeln!(s, "View __ByardPersist() {{");
    let _ = writeln!(s, "    inject Store as __byard_store");
    let _ = writeln!(s, "    inject {store} as {handle}");
    let _ = writeln!(s, "    on mount => {{");
    for field in &decl.fields {
        let name = field.name.as_str();
        let ty = spell(&field.ty);
        let _ = writeln!(
            s,
            "        __byard_store.get(\"{key}\", {{}}) ok saved => {{ {handle}.{name} = saved.{name} as {ty} }} err e => {{ }}"
        );
    }
    let _ = writeln!(s, "    }}");
    // An optional field is passed on as it is, none included (`?? none`
    // says the read knows it may be none).
    let read = |f: &crate::parser::ast::RecordField| {
        if f.optional {
            format!("{handle}.{} ?? none", f.name.as_str())
        } else {
            format!("{handle}.{}", f.name.as_str())
        }
    };
    let watched = decl.fields.iter().map(read).collect::<Vec<_>>().join(", ");
    let record = decl
        .fields
        .iter()
        .map(|f| format!("{}: {}", f.name.as_str(), read(f)))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        s,
        "    on change({watched}) => {{ __byard_store.set(\"{key}\", {{ {record} }}) }}"
    );
    let _ = writeln!(s, "}}");
    s
}
