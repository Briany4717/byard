//! Hover capability for inspecting components, attributes, and typed variables.

use byard_compiler::Symbol;
use byard_compiler::infer::{Inference, Ty};
use byard_compiler::interp::intrinsics;
use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};

use crate::state::document::Document;
use crate::syntax::ast_utils::{HoverTarget, find_hover_target};

/// Handles a hover request at a specific document position.
#[must_use]
pub fn handle_hover(doc: &Document, pos: Position) -> Option<Hover> {
    let offset = doc.line_index.position_to_offset(&doc.content, pos)?;
    let target = find_hover_target(&doc.parsed.views, offset)?;

    let docs = match target {
        HoverTarget::Intrinsic { name } => intrinsic_hover_docs(&name)?,
        HoverTarget::Attribute {
            element_name,
            attr_name,
        } => attribute_hover_docs(&element_name, &attr_name)?,
        HoverTarget::VarIdent { name, .. } => var_hover_docs(&name, &doc.inference),
    };

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: docs,
        }),
        range: None,
    })
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
