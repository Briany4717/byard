//! Hover capability for inspecting components, attributes, and typed variables.

use byard_compiler::Symbol;
use byard_compiler::infer::{Inference, Ty};
use byard_compiler::interp::intrinsics;
use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};

use crate::semantic::symbols::{PackageIndex, qualified_target};
use crate::state::document::Document;
use crate::syntax::ast_utils::{HoverTarget, find_hover_target};

/// Handles a hover request at a specific document position.
#[must_use]
pub fn handle_hover(doc: &Document, pos: Position) -> Option<Hover> {
    let offset = doc.line_index.position_to_offset(&doc.content, pos)?;
    let target = find_hover_target(&doc.parsed.views, offset)?;

    let docs = match target {
        HoverTarget::Intrinsic { name } if name.contains('.') => qualified_hover_docs(doc, &name)?,
        HoverTarget::Intrinsic { name } => intrinsic_hover_docs(&name)?,
        HoverTarget::Attribute {
            element_name,
            attr_name,
        } => attribute_hover_docs(&element_name, &attr_name)?,
        HoverTarget::VarIdent { name, .. } => binding_hover_docs(doc, offset, &name)
            .unwrap_or_else(|| var_hover_docs(&name, &doc.inference)),
    };

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: docs,
        }),
        range: None,
    })
}

/// A view reached by its package's name (`m.Button`, or `material.Button`
/// inside `material`): its signature and where it comes from.
fn qualified_hover_docs(doc: &Document, name: &str) -> Option<String> {
    let index = PackageIndex::build(doc.file_path().as_deref());
    let (pkg, rest) = qualified_target(name, &doc.parsed.imports, index.own_package.as_deref())?;
    let (view, origin) = match &pkg {
        Some(pkg) => (index.view(pkg, rest)?, format!("package `{pkg}`")),
        None => (
            doc.parsed.views.iter().find(|v| v.name.as_str() == rest)?,
            "this package".to_string(),
        ),
    };
    // A package's view lives in another file for an import: show its
    // parameter names, which is what a call site needs.
    let params: Vec<String> = view.params.iter().map(param_text).collect();
    Some(format!(
        "```byld\nView {rest}({})\n```\n\nA view of {origin}.",
        params.join(", ")
    ))
}

/// A parameter as a call site needs it: its name, and `bind` before it when it
/// is a binding (RFC-0051).
fn param_text(param: &byard_compiler::parser::ast::Param) -> String {
    if param.bind {
        format!("bind {}", param.name)
    } else {
        param.name.to_string()
    }
}

/// A binding of the view under the cursor, hovered by its declaration or any
/// use in the view: `bind on: Bool`, so that it is not mistaken for a copy.
fn binding_hover_docs(doc: &Document, offset: usize, name: &str) -> Option<String> {
    let view = doc
        .parsed
        .views
        .iter()
        .find(|v| crate::syntax::ast_utils::span_contains(v.span, offset))?;
    let param = view
        .params
        .iter()
        .find(|p| p.bind && p.name.as_str() == name)?;
    let ty = param.ty.as_ref().map(type_text).unwrap_or_default();
    Some(format!(
        "```byld\nbind {name}: {ty}\n```\n\nThe caller's `var`, read and written here as if \
         it were this view's own."
    ))
}

fn type_text(ty: &byard_compiler::parser::ast::Type) -> String {
    use byard_compiler::parser::ast::Type;
    match ty {
        Type::Named { name, args, .. } if args.is_empty() => name.to_string(),
        Type::Named { name, args, .. } => {
            let args: Vec<String> = args.iter().map(type_text).collect();
            format!("{name}<{}>", args.join(", "))
        }
        Type::Function { params, ret, .. } => {
            let params: Vec<String> = params.iter().map(type_text).collect();
            let ret = ret
                .as_ref()
                .map(|r| format!(" -> {}", type_text(r)))
                .unwrap_or_default();
            format!("Fn({}){ret}", params.join(", "))
        }
    }
}

fn intrinsic_hover_docs(name: &str) -> Option<String> {
    let info = intrinsics::lookup(name)?;
    let mut doc = format!("### Intrinsic `{name}`\n\n");
    if info.children {
        doc.push_str("- **Children**: Yes (expects `{ ... }`)\n");
    } else {
        doc.push_str("- **Children**: No\n");
    }
    if info.focusable {
        doc.push_str("- **Focusable**: Yes\n");
    }
    if info.interactive {
        doc.push_str("- **Interactive**: Yes\n");
    }
    doc.push_str("\n#### Accepted Properties:\n");
    let mut props: Vec<_> = info.properties().collect();
    props.sort_by_key(|(k, _)| *k);
    for (prop, ty) in props {
        doc.push_str(&format!("* `{prop}`: `{ty:?}`\n"));
    }
    doc.push_str("\n#### Accepted Events:\n");
    let mut events: Vec<_> = info.events().collect();
    events.sort_unstable();
    for event in events {
        doc.push_str(&format!("* `{event}`\n"));
    }
    Some(doc)
}

fn attribute_hover_docs(element_name: &str, attr_name: &str) -> Option<String> {
    let info = intrinsics::lookup(element_name)?;
    let mut doc = format!("### Attribute `{attr_name}` on `{element_name}`\n\n");
    if let Some(prop_ty) = info.property_type(attr_name) {
        doc.push_str(&format!("Type: Property (`{prop_ty:?}`)\n"));
    } else if info.has_event(attr_name) {
        doc.push_str("Type: Event Callback (`=>`)\n");
    } else {
        return None;
    }
    Some(doc)
}

fn var_hover_docs(name: &str, inference: &Inference) -> String {
    let var_symbol = Symbol::intern(name);
    if let Some((_, ty)) = inference
        .bindings
        .iter()
        .find(|(sym, _)| *sym == var_symbol)
    {
        format!("```byld\nvar {name}: {}\n```", format_ty(ty))
    } else {
        format!("```byld\nvar {name}\n```")
    }
}

fn format_ty(ty: &Ty) -> String {
    match ty {
        Ty::Int => "Int".to_string(),
        Ty::Float => "Float".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::Str => "Str".to_string(),
        Ty::List(inner) => format!("List<{}>", format_ty(inner)),
        Ty::Fn(params, ret) => {
            let params_str: Vec<String> = params.iter().map(format_ty).collect();
            let ret_str = match ret {
                Some(r) => format!(" -> {}", format_ty(r)),
                None => String::new(),
            };
            format!("Fn({}){}", params_str.join(", "), ret_str)
        }
        Ty::Named(sym) => sym.as_str().to_string(),
        Ty::Unknown => "Unknown".to_string(),
    }
}
