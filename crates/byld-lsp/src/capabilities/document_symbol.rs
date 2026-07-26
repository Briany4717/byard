//! Document Symbol capability for hierarchical outline navigation.

use byard_compiler::parser::ast::{ElementNode, Member};
use lsp_types::{DocumentSymbol, DocumentSymbolResponse, SymbolKind};

use crate::state::document::Document;

/// Handles a document symbol request, building a hierarchical outline of the document.
#[must_use]
pub fn handle_document_symbol(doc: &Document) -> Option<DocumentSymbolResponse> {
    let mut symbols = Vec::new();

    for view in &doc.parsed.views {
        let view_range = doc.line_index.span_to_range(&doc.content, view.span);
        let mut view_children = Vec::new();

        // Parameter symbols
        for param in &view.params {
            let param_range = doc.line_index.span_to_range(&doc.content, param.span);
            #[allow(deprecated)]
            view_children.push(DocumentSymbol {
                name: param.name.to_string(),
                detail: Some("Parameter".to_string()),
                kind: SymbolKind::VARIABLE,
                tags: None,
                deprecated: None,
                range: param_range,
                selection_range: param_range,
                children: None,
            });
        }

        // Body member symbols
        collect_member_symbols(doc, &view.body, &mut view_children);

        #[allow(deprecated)]
        symbols.push(DocumentSymbol {
            name: view.name.to_string(),
            detail: Some("View Component".to_string()),
            kind: SymbolKind::INTERFACE,
            tags: None,
            deprecated: None,
            range: view_range,
            selection_range: view_range,
            children: if view_children.is_empty() {
                None
            } else {
                Some(view_children)
            },
        });
    }

    Some(DocumentSymbolResponse::Nested(symbols))
}

fn collect_member_symbols(doc: &Document, members: &[Member], out: &mut Vec<DocumentSymbol>) {
    for member in members {
        match member {
            Member::Var { name, span, .. } => {
                let range = doc.line_index.span_to_range(&doc.content, *span);
                #[allow(deprecated)]
                out.push(DocumentSymbol {
                    name: name.to_string(),
                    detail: Some("State (var)".to_string()),
                    kind: SymbolKind::VARIABLE,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: None,
                });
            }
            Member::Let { name, span, .. } => {
                let range = doc.line_index.span_to_range(&doc.content, *span);
                #[allow(deprecated)]
                out.push(DocumentSymbol {
                    name: name.to_string(),
                    detail: Some("Memo (let)".to_string()),
                    kind: SymbolKind::CONSTANT,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: None,
                });
            }
            Member::Fn { name, span, .. } => {
                let range = doc.line_index.span_to_range(&doc.content, *span);
                #[allow(deprecated)]
                out.push(DocumentSymbol {
                    name: name.to_string(),
                    detail: Some("Helper Function".to_string()),
                    kind: SymbolKind::FUNCTION,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: None,
                });
            }
            Member::Inject { name, span, ty } => {
                let range = doc.line_index.span_to_range(&doc.content, *span);
                #[allow(deprecated)]
                out.push(DocumentSymbol {
                    name: name.to_string(),
                    detail: Some(format!("Inject: {ty:?}")),
                    kind: SymbolKind::PROPERTY,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: None,
                });
            }
            Member::Element(el) => {
                collect_element_symbol(doc, el, out);
            }
            Member::Style { rules, span } => {
                let range = doc.line_index.span_to_range(&doc.content, *span);
                let mut style_children = Vec::new();
                for rule in rules {
                    let rule_range = doc.line_index.span_to_range(&doc.content, rule.span);
                    #[allow(deprecated)]
                    style_children.push(DocumentSymbol {
                        name: format!(".{}", rule.class.as_str()),
                        detail: Some("Style Rule".to_string()),
                        kind: SymbolKind::PROPERTY,
                        tags: None,
                        deprecated: None,
                        range: rule_range,
                        selection_range: rule_range,
                        children: None,
                    });
                }
                #[allow(deprecated)]
                out.push(DocumentSymbol {
                    name: "style".to_string(),
                    detail: Some("Style Block".to_string()),
                    kind: SymbolKind::NAMESPACE,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: if style_children.is_empty() {
                        None
                    } else {
                        Some(style_children)
                    },
                });
            }
            Member::For { var, span, body, .. } => {
                let range = doc.line_index.span_to_range(&doc.content, *span);
                let mut children = Vec::new();
                collect_member_symbols(doc, body, &mut children);
                #[allow(deprecated)]
                out.push(DocumentSymbol {
                    name: format!("for {var}"),
                    detail: Some("Loop".to_string()),
                    kind: SymbolKind::ARRAY,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: if children.is_empty() {
                        None
                    } else {
                        Some(children)
                    },
                });
            }
            Member::Route {
                pattern, span, body, ..
            } => {
                let range = doc.line_index.span_to_range(&doc.content, *span);
                let mut children = Vec::new();
                collect_member_symbols(doc, body, &mut children);
                #[allow(deprecated)]
                out.push(DocumentSymbol {
                    name: format!("route \"{pattern}\""),
                    detail: Some("Route Case".to_string()),
                    kind: SymbolKind::MODULE,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: if children.is_empty() {
                        None
                    } else {
                        Some(children)
                    },
                });
            }
            Member::When {
                then, els, span, ..
            } => {
                let range = doc.line_index.span_to_range(&doc.content, *span);
                let mut children = Vec::new();
                collect_member_symbols(doc, then, &mut children);
                if let Some(els_members) = els {
                    collect_member_symbols(doc, els_members, &mut children);
                }
                #[allow(deprecated)]
                out.push(DocumentSymbol {
                    name: "when".to_string(),
                    detail: Some("Condition".to_string()),
                    kind: SymbolKind::BOOLEAN,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: if children.is_empty() {
                        None
                    } else {
                        Some(children)
                    },
                });
            }
            Member::Expr(_) => {}
        }
    }
}

fn collect_element_symbol(doc: &Document, el: &ElementNode, out: &mut Vec<DocumentSymbol>) {
    let range = doc.line_index.span_to_range(&doc.content, el.span);
    let mut children = Vec::new();
    collect_member_symbols(doc, &el.children, &mut children);

    #[allow(deprecated)]
    out.push(DocumentSymbol {
        name: el.name.to_string(),
        detail: Some("Element".to_string()),
        kind: SymbolKind::CLASS,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    });
}
