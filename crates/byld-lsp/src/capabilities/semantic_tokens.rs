//! Semantic tokens capability for rich AST-based syntax highlighting.

use byard_compiler::diagnostics::Span;
use byard_compiler::parser::ast::{AttrKind, ElementNode, Expr, Member, StrPart, ViewDecl};
use lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensLegend,
    SemanticTokensResult,
};

use crate::state::document::Document;

/// Returns the semantic tokens legend describing available token types and modifiers.
#[must_use]
pub fn get_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,  // 0
            SemanticTokenType::TYPE,     // 1
            SemanticTokenType::FUNCTION, // 2
            SemanticTokenType::VARIABLE, // 3
            SemanticTokenType::PROPERTY, // 4
            SemanticTokenType::STRING,   // 5
            SemanticTokenType::NUMBER,   // 6
            SemanticTokenType::OPERATOR, // 7
            SemanticTokenType::COMMENT,  // 8
            SemanticTokenType::CLASS,    // 9
            SemanticTokenType::EVENT,    // 10
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::READONLY,
        ],
    }
}

#[derive(Debug)]
struct RawToken {
    span: Span,
    token_type: u32,
    modifiers: u32,
}

/// Handles `textDocument/semanticTokens/full` request.
#[must_use]
pub fn handle_semantic_tokens(doc: &Document) -> Option<SemanticTokensResult> {
    let mut raw_tokens = Vec::new();

    // 1. Package imports
    for import in &doc.parsed.imports {
        raw_tokens.push(RawToken {
            span: Span::new(import.span.start, import.span.start + 3), // "use"
            token_type: 0,                                             // KEYWORD
            modifiers: 0,
        });
        raw_tokens.push(RawToken {
            span: Span::new(
                import.span.start + 4,
                import.span.start + 4 + import.package.as_str().len() as u32,
            ),
            token_type: 9, // CLASS / PACKAGE
            modifiers: 0,
        });
    }

    // 2. Views and their members
    for view in &doc.parsed.views {
        collect_view_tokens(view, &mut raw_tokens);
    }

    // Sort tokens by start position to ensure proper delta encoding
    raw_tokens.sort_by_key(|t| t.span.start);

    // Convert raw tokens into relative LSP SemanticTokens
    let mut lsp_tokens = Vec::with_capacity(raw_tokens.len());
    let mut prev_line = 0;
    let mut prev_char = 0;

    for token in raw_tokens {
        let pos = doc
            .line_index
            .offset_to_position(&doc.content, token.span.start as usize);
        let len = token.span.end.saturating_sub(token.span.start);

        let delta_line = pos.line - prev_line;
        let delta_start = if delta_line == 0 {
            pos.character.saturating_sub(prev_char)
        } else {
            pos.character
        };

        lsp_tokens.push(SemanticToken {
            delta_line,
            delta_start,
            length: len,
            token_type: token.token_type,
            token_modifiers_bitset: token.modifiers,
        });

        prev_line = pos.line;
        prev_char = pos.character;
    }

    Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data: lsp_tokens,
    }))
}

fn collect_view_tokens(view: &ViewDecl, out: &mut Vec<RawToken>) {
    let view_kw_len = 4;
    out.push(RawToken {
        span: Span::new(view.span.start, view.span.start + view_kw_len),
        token_type: 0, // KEYWORD
        modifiers: 0,
    });
    out.push(RawToken {
        span: Span::new(
            view.span.start + view_kw_len + 1,
            view.span.start + view_kw_len + 1 + view.name.as_str().len() as u32,
        ),
        token_type: 9, // CLASS
        modifiers: 1,  // DECLARATION
    });

    for param in &view.params {
        out.push(RawToken {
            span: Span::new(
                param.span.start,
                param.span.start + param.name.as_str().len() as u32,
            ),
            token_type: 3, // VARIABLE
            modifiers: 1,  // DECLARATION
        });
    }

    collect_member_tokens(&view.body, out);
}

fn collect_member_tokens(members: &[Member], out: &mut Vec<RawToken>) {
    for member in members {
        match member {
            Member::Var {
                name, init, span, ..
            } => {
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 3), // "var"
                    token_type: 0,                               // KEYWORD
                    modifiers: 0,
                });
                out.push(RawToken {
                    span: Span::new(span.start + 4, span.start + 4 + name.as_str().len() as u32),
                    token_type: 3, // VARIABLE
                    modifiers: 1,  // DECLARATION
                });
                collect_expr_tokens(init, out);
            }
            Member::Let {
                name, init, span, ..
            } => {
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 3), // "let"
                    token_type: 0,                               // KEYWORD
                    modifiers: 0,
                });
                out.push(RawToken {
                    span: Span::new(span.start + 4, span.start + 4 + name.as_str().len() as u32),
                    token_type: 3, // VARIABLE
                    modifiers: 3,  // DECLARATION | READONLY
                });
                collect_expr_tokens(init, out);
            }
            Member::Fn {
                name,
                params,
                body,
                span,
                ..
            } => {
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 2), // "fn"
                    token_type: 0,                               // KEYWORD
                    modifiers: 0,
                });
                out.push(RawToken {
                    span: Span::new(span.start + 3, span.start + 3 + name.as_str().len() as u32),
                    token_type: 2, // FUNCTION
                    modifiers: 1,  // DECLARATION
                });
                for param in params {
                    out.push(RawToken {
                        span: Span::new(
                            param.span.start,
                            param.span.start + param.name.as_str().len() as u32,
                        ),
                        token_type: 3, // VARIABLE
                        modifiers: 1,  // DECLARATION
                    });
                }
                collect_expr_tokens(body, out);
            }
            Member::Element(el) => {
                collect_element_tokens(el, out);
            }
            Member::For {
                var,
                iter,
                body,
                span,
                ..
            } => {
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 3), // "for"
                    token_type: 0,                               // KEYWORD
                    modifiers: 0,
                });
                out.push(RawToken {
                    span: Span::new(span.start + 4, span.start + 4 + var.as_str().len() as u32),
                    token_type: 3, // VARIABLE
                    modifiers: 1,  // DECLARATION
                });
                collect_expr_tokens(iter, out);
                collect_member_tokens(body, out);
            }
            Member::When {
                cond,
                then,
                els,
                span,
            } => {
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 4), // "when"
                    token_type: 0,                               // KEYWORD
                    modifiers: 0,
                });
                collect_expr_tokens(cond, out);
                collect_member_tokens(then, out);
                if let Some(els_members) = els {
                    collect_member_tokens(els_members, out);
                }
            }
            Member::Style { rules, span } => {
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 5), // "style"
                    token_type: 0,                               // KEYWORD
                    modifiers: 0,
                });
                for rule in rules {
                    out.push(RawToken {
                        span: Span::new(
                            rule.span.start,
                            rule.span.start + 1 + rule.class.as_str().len() as u32,
                        ),
                        token_type: 4, // PROPERTY / STYLE
                        modifiers: 0,
                    });
                }
            }
            Member::Route { body, span, .. } => {
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 5), // "route"
                    token_type: 0,                               // KEYWORD
                    modifiers: 0,
                });
                collect_member_tokens(body, out);
            }
            Member::Expr(expr) => {
                collect_expr_tokens(expr, out);
            }
            Member::Lifecycle { action, span, .. } => {
                // "on mount" / "on unmount": highlight the `on` keyword, then
                // the action body like any other expression.
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 2), // "on"
                    token_type: 0,                               // KEYWORD
                    modifiers: 0,
                });
                collect_expr_tokens(action, out);
            }
            Member::Timer { action, span, .. } => {
                // "every" and "after" are both five characters, so the keyword
                // run is the same length either way.
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 5),
                    token_type: 0, // KEYWORD
                    modifiers: 0,
                });
                collect_expr_tokens(action, out);
            }
            Member::Measure { action, span } => {
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 2), // "on"
                    token_type: 0,                               // KEYWORD
                    modifiers: 0,
                });
                collect_expr_tokens(action, out);
            }
            Member::Inject { name, span, .. } => {
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 6), // "inject"
                    token_type: 0,                               // KEYWORD
                    modifiers: 0,
                });
                out.push(RawToken {
                    span: Span::new(span.start + 7, span.start + 7 + name.as_str().len() as u32),
                    token_type: 3, // VARIABLE
                    modifiers: 1,  // DECLARATION
                });
            }
        }
    }
}

fn collect_element_tokens(el: &ElementNode, out: &mut Vec<RawToken>) {
    let name_len = el.name.as_str().len() as u32;

    out.push(RawToken {
        span: Span::new(el.span.start, el.span.start + name_len),
        token_type: 9, // CLASS
        modifiers: 0,
    });

    for attr in &el.attrs {
        let attr_name_len = attr.name.as_str().len() as u32;
        let attr_token_type = match attr.kind {
            AttrKind::Event { .. } => 10, // EVENT
            _ => 4,                       // PROPERTY
        };
        out.push(RawToken {
            span: Span::new(attr.span.start, attr.span.start + attr_name_len),
            token_type: attr_token_type,
            modifiers: 0,
        });

        match &attr.kind {
            AttrKind::Prop { value } | AttrKind::Spread { value } => {
                collect_expr_tokens(value, out);
            }
            AttrKind::Event { action, .. } => {
                collect_expr_tokens(action, out);
            }
        }
    }

    if let Some(action) = &el.action {
        collect_expr_tokens(action, out);
    }

    collect_member_tokens(&el.children, out);
}

fn collect_expr_tokens(expr: &Expr, out: &mut Vec<RawToken>) {
    match expr {
        Expr::Ident(_, span) => {
            out.push(RawToken {
                span: *span,
                token_type: 3, // VARIABLE
                modifiers: 0,
            });
        }
        Expr::IntLit(_, span) | Expr::FloatLit(_, span) => {
            out.push(RawToken {
                span: *span,
                token_type: 6, // NUMBER
                modifiers: 0,
            });
        }
        Expr::StrLit(parts, span) => {
            out.push(RawToken {
                span: *span,
                token_type: 5, // STRING
                modifiers: 0,
            });
            for part in parts {
                if let StrPart::Interp(inner) = part {
                    collect_expr_tokens(inner, out);
                }
            }
        }
        Expr::Array(items, _) => {
            for item in items {
                collect_expr_tokens(item, out);
            }
        }
        Expr::Call { callee, args, .. } => {
            collect_expr_tokens(callee, out);
            for arg in args {
                collect_expr_tokens(&arg.value, out);
            }
        }
        Expr::Member { base, field, span } => {
            collect_expr_tokens(base, out);
            let field_start = span.end as usize - field.as_str().len();
            out.push(RawToken {
                span: Span::new(field_start as u32, span.end),
                token_type: 4, // PROPERTY
                modifiers: 0,
            });
        }
        Expr::ClassRef(_, span) => {
            out.push(RawToken {
                span: *span,
                token_type: 4, // PROPERTY / CLASS_REF
                modifiers: 0,
            });
        }
        _ => {}
    }
}
