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
    let target = find_hover_target(&doc.parsed.views, offset);
    // A constant, an enum or one of its cases (RFC-0055), unless the cursor is
    // on an element or an attribute name, which can be spelled the same
    // (`gap: gap`).
    if !matches!(
        target,
        Some(HoverTarget::Intrinsic { .. } | HoverTarget::Attribute { .. })
    ) {
        if let Some(docs) = declaration_hover_docs(doc, offset) {
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: docs,
                }),
                range: None,
            });
        }
    }
    let target = target?;

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

/// What a hover on a constant, an enum or an enum case says: the constant's
/// type, the enum's cases, the case's enum. The word under the cursor is read
/// from the text, because a name can be written in a place no AST node marks.
fn declaration_hover_docs(doc: &Document, offset: usize) -> Option<String> {
    let text = &doc.content;
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let start = text
        .get(..offset)?
        .rfind(|c: char| !is_word(c))
        .map_or(0, |i| i + 1);
    let end = text[offset..]
        .find(|c: char| !is_word(c))
        .map_or(text.len(), |i| offset + i);
    let word = text.get(start..end).filter(|w| !w.is_empty())?;
    // The name before a dot, if the word follows one.
    let enum_before = text[..start].strip_suffix('.').and_then(|head| {
        let from = head.rfind(|c: char| !is_word(c)).map_or(0, |i| i + 1);
        doc.parsed
            .enums
            .iter()
            .find(|e| e.name.as_str() == &head[from..])
    });
    if let Some(decl) = enum_before {
        return decl.cases.iter().any(|(c, _)| c.as_str() == word).then(|| {
            format!(
                "```byld
{}.{word}
```

A case of the enum `{}`.",
                decl.name.as_str(),
                decl.name.as_str()
            )
        });
    }
    if let Some(decl) = doc.parsed.enums.iter().find(|e| e.name.as_str() == word) {
        let cases: Vec<&str> = decl.cases.iter().map(|(c, _)| c.as_str()).collect();
        return Some(format!(
            "```byld
{}enum {word} {{ {} }}
```",
            if decl.private { "private " } else { "" },
            cases.join(", ")
        ));
    }
    let symbol = Symbol::intern(word);
    let decl = doc.parsed.consts.iter().find(|c| c.name == symbol)?;
    let ty = doc
        .inference
        .consts
        .iter()
        .find(|(n, _)| *n == symbol)
        .map_or_else(String::new, |(_, ty)| format!(": {}", format_ty(ty)));
    Some(format!(
        "```byld
{}let {word}{ty}
```

A constant, evaluated once when the program loads.",
        if decl.private { "private " } else { "" }
    ))
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

pub(crate) fn format_ty(ty: &Ty) -> String {
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
        Ty::Optional(inner) => format!("{}?", format_ty(inner)),
        Ty::Named(sym) => sym.as_str().to_string(),
        Ty::Unknown => "Unknown".to_string(),
    }
}
