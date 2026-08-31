//! Completion capability for context-aware suggestions in Byld DSL.

use byard_compiler::Symbol;
use byard_compiler::interp::intrinsics::{INTRINSIC_NAMES, lookup};
use lsp_types::{CompletionItem, CompletionItemKind, CompletionResponse, Position};

use crate::semantic::symbols::{PackageIndex, resolve_package_view};
use crate::state::document::Document;
use crate::syntax::ast_utils::{collect_locals_in_members, span_contains};

const STYLE_PROPS: &[(&str, &str)] = &[
    ("width", "Int (logical pixels)"),
    ("height", "Int (logical pixels)"),
    ("gap", "Int (logical pixels spacing)"),
    ("p", "Len (padding on all sides)"),
    ("m", "Len (margin on all sides)"),
    ("px", "Len (horizontal padding)"),
    ("py", "Len (vertical padding)"),
    ("pt", "Len (top padding)"),
    ("pr", "Len (right padding)"),
    ("pb", "Len (bottom padding)"),
    ("pl", "Len (left padding)"),
    ("mx", "Len (horizontal margin)"),
    ("my", "Len (vertical margin)"),
    ("mt", "Len (top margin)"),
    ("mr", "Len (right margin)"),
    ("mb", "Len (bottom margin)"),
    ("ml", "Len (left margin)"),
    ("align", "Enum: start | center | end | stretch | justify"),
    (
        "justify",
        "Enum: start | center | end | between | around | evenly",
    ),
    ("grow", "Int (flex grow factor)"),
    ("basis", "Int (flex basis size)"),
    ("bg", "Color (background color hex)"),
    ("radius", "Len (border radius)"),
    ("opacity", "Float (0.0 to 1.0)"),
    ("border", "Color (border color)"),
    ("shadow", "Str (shadow specification)"),
    ("ripple", "Color (ripple ink color, RFC-0023)"),
    ("ripple_active", "Bool (triggers the ripple, RFC-0023)"),
    (
        "ripple_radius",
        "Float (max ripple radius override, RFC-0023)",
    ),
    (
        "ripple_duration",
        "Int (ripple fade-out ms, default 300, RFC-0023)",
    ),
    (
        "blur",
        "Float (backdrop blur radius in px, max 40, RFC-0023)",
    ),
    (
        "backdrop_tint",
        "Color (tint over the blurred backdrop, RFC-0023)",
    ),
    (
        "blur_saturation",
        "Float (vibrancy boost, default 1.8, RFC-0023)",
    ),
    ("blur_quality", "Enum: auto | high | low (RFC-0023)"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum AttributeContext {
    StyleRule,
    Element(String),
}

/// Handles autocompletion request at a position.
#[must_use]
pub fn handle_completion(doc: &Document, pos: Position) -> Option<CompletionResponse> {
    let offset = doc.line_index.position_to_offset(&doc.content, pos)?;
    let doc_path = doc.file_path();
    let index = PackageIndex::build(doc_path.as_deref());

    // `use <TAB>` -> manifest declared dependencies
    if in_use_decl(&doc.content, offset) {
        let items = index
            .declared
            .iter()
            .map(|name| CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some("Package (byard.toml [dependencies])".to_string()),
                ..Default::default()
            })
            .collect();
        return Some(CompletionResponse::Array(items));
    }

    if let Some(attr_context) = find_active_attribute_context(doc, offset) {
        let mut items = Vec::new();
        match attr_context {
            AttributeContext::StyleRule => {
                for &(prop, desc) in STYLE_PROPS {
                    items.push(CompletionItem {
                        label: prop.to_string(),
                        kind: Some(CompletionItemKind::PROPERTY),
                        detail: Some(desc.to_string()),
                        insert_text: Some(format!("{prop}: ")),
                        ..Default::default()
                    });
                }
            }
            AttributeContext::Element(el_name) => {
                if let Some(info) = lookup(&el_name) {
                    for (prop, ty) in info.properties() {
                        items.push(CompletionItem {
                            label: prop.to_string(),
                            kind: Some(CompletionItemKind::PROPERTY),
                            detail: Some(format!("Property ({ty:?})")),
                            insert_text: Some(format!("{prop}: ")),
                            ..Default::default()
                        });
                    }
                    for event in info.events() {
                        items.push(CompletionItem {
                            label: event.to_string(),
                            kind: Some(CompletionItemKind::EVENT),
                            detail: Some("Event Callback".to_string()),
                            insert_text: Some(format!("{event} => ")),
                            ..Default::default()
                        });
                    }
                } else {
                    let pkg_view = resolve_package_view(&el_name, &doc.parsed.imports, &index);
                    let local_view = doc.parsed.views.iter().find(|v| v.name.as_str() == el_name);
                    if let Some(view) = pkg_view.or(local_view) {
                        for param in &view.params {
                            items.push(CompletionItem {
                                label: param.name.to_string(),
                                kind: Some(CompletionItemKind::PROPERTY),
                                detail: Some("View Parameter".to_string()),
                                insert_text: Some(format!("{}: ", param.name)),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
        return Some(CompletionResponse::Array(items));
    }

    let mut items = Vec::new();

    let keywords = &[
        "use", "var", "let", "fn", "inject", "for", "in", "when", "else", "style", "route", "tab",
    ];
    for kw in keywords {
        items.push(CompletionItem {
            label: kw.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        });
    }

    for intrinsic in INTRINSIC_NAMES {
        items.push(CompletionItem {
            label: intrinsic.to_string(),
            kind: Some(CompletionItemKind::CLASS),
            detail: Some("Intrinsic Component".to_string()),
            ..Default::default()
        });
    }

    for view in &doc.parsed.views {
        items.push(CompletionItem {
            label: view.name.to_string(),
            kind: Some(CompletionItemKind::INTERFACE),
            detail: Some("User View".to_string()),
            ..Default::default()
        });
    }

    for import in &doc.parsed.imports {
        let pkg = import.package.as_str();
        let Some(exports) = index.exports.get(pkg) else {
            continue;
        };
        if let Some(symbols) = &import.symbols {
            for (sym, _) in symbols {
                if let Some(view) = index.view(pkg, sym.as_str()) {
                    items.push(CompletionItem {
                        label: view.name.to_string(),
                        kind: Some(CompletionItemKind::INTERFACE),
                        detail: Some(format!("View · package `{pkg}`")),
                        ..Default::default()
                    });
                }
            }
        } else {
            let alias = import.alias.as_ref().map_or(pkg, Symbol::as_str);
            for view in exports {
                items.push(CompletionItem {
                    label: format!("{alias}.{}", view.name.as_str()),
                    kind: Some(CompletionItemKind::INTERFACE),
                    detail: Some(format!("View · package `{pkg}`")),
                    ..Default::default()
                });
            }
        }
    }

    if let Some(active_view) = doc
        .parsed
        .views
        .iter()
        .find(|v| span_contains(v.span, offset))
    {
        let mut locals = Vec::new();
        for param in &active_view.params {
            locals.push((
                param.name.to_string(),
                CompletionItemKind::VARIABLE,
                "Parameter".to_string(),
            ));
        }
        collect_locals_in_members(&active_view.body, offset, &mut locals);

        for (name, kind, detail) in locals {
            items.push(CompletionItem {
                label: name,
                kind: Some(kind),
                detail: Some(detail),
                ..Default::default()
            });
        }
    }

    Some(CompletionResponse::Array(items))
}

fn in_use_decl(content: &str, offset: usize) -> bool {
    let line_start = content[..offset.min(content.len())]
        .rfind('\n')
        .map_or(0, |i| i + 1);
    let line = &content[line_start..offset.min(content.len())];
    let trimmed = line.trim_start();
    trimmed == "use" || (trimmed.starts_with("use ") && !trimmed.contains('.'))
}

fn find_active_attribute_context(doc: &Document, offset: usize) -> Option<AttributeContext> {
    let content = &doc.content;
    let mut bracket_depth = 0;
    let mut found_attr_start = None;
    let mut i = offset;
    let bytes = content.as_bytes();
    while i > 0 {
        i -= 1;
        if i >= bytes.len() {
            continue;
        }
        let c = bytes[i];
        if c == b']' {
            bracket_depth += 1;
        } else if c == b'[' && i > 0 && bytes[i - 1] == b'#' {
            if bracket_depth == 0 {
                found_attr_start = Some(i - 1);
                break;
            }
            bracket_depth -= 1;
        }
    }

    let start_offset = found_attr_start?;

    if is_style_rule_before_attr(content, start_offset) {
        return Some(AttributeContext::StyleRule);
    }

    if let Some(el_name) = find_element_name_before_attr(content, start_offset) {
        return Some(AttributeContext::Element(el_name));
    }

    for view in &doc.parsed.views {
        if !span_contains(view.span, start_offset) {
            continue;
        }
        if let Some(name) =
            crate::syntax::ast_utils::find_element_at_offset(&view.body, start_offset)
        {
            return Some(AttributeContext::Element(name));
        }
    }
    None
}

fn is_style_rule_before_attr(content: &str, attr_start: usize) -> bool {
    let bytes = content.as_bytes();
    let mut i = attr_start;
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    if i == 0 {
        return false;
    }
    while i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
        i -= 1;
    }
    if i > 0 && bytes[i - 1] == b'.' {
        let qualified = i >= 2 && (bytes[i - 2].is_ascii_alphanumeric() || bytes[i - 2] == b'_');
        return !qualified;
    }
    false
}

fn find_element_name_before_attr(content: &str, attr_start: usize) -> Option<String> {
    let bytes = content.as_bytes();
    let mut i = attr_start;
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    if i == 0 {
        return None;
    }
    if bytes[i - 1] == b')' {
        i -= 1;
        let mut paren_depth = 1;
        while i > 0 && paren_depth > 0 {
            i -= 1;
            if i >= bytes.len() {
                break;
            }
            if bytes[i] == b')' {
                paren_depth += 1;
            } else if bytes[i] == b'(' {
                paren_depth -= 1;
            }
        }
        while i > 0 && bytes[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
    }
    if i == 0 {
        return None;
    }
    let name_end = i;
    while i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
        i -= 1;
    }
    if i > 1 && bytes[i - 1] == b'.' {
        let mut j = i - 1;
        while j > 0 && (bytes[j - 1].is_ascii_alphanumeric() || bytes[j - 1] == b'_') {
            j -= 1;
        }
        if j < i - 1 {
            i = j;
        }
    }
    if i < name_end {
        let name = &content[i..name_end];
        if name.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return Some(name.to_string());
        }
    }
    None
}
