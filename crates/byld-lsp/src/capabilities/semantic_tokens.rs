//! Semantic tokens capability for rich AST-based syntax highlighting.

use byard_compiler::Symbol;
use byard_compiler::diagnostics::Span;
use byard_compiler::parser::ast::{
    Attr, AttrKind, ElementNode, Expr, Member, Param, StrPart, ViewDecl,
};
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

    // 2. Top-level functions
    for decl in &doc.parsed.fns {
        collect_fn_tokens(
            &decl.name,
            &decl.params,
            &decl.body,
            decl.span,
            &mut raw_tokens,
        );
    }

    // 3. Views and their members
    for view in &doc.parsed.views {
        collect_view_tokens(view, &doc.content, &mut raw_tokens);
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

fn collect_view_tokens(view: &ViewDecl, content: &str, out: &mut Vec<RawToken>) {
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
        let mut name_start = param.span.start;
        if param.bind {
            // `bind name: Type` (RFC-0051): the word, then the name after the
            // blanks that follow it.
            out.push(RawToken {
                span: Span::new(param.span.start, param.span.start + 4),
                token_type: 0, // KEYWORD
                modifiers: 0,
            });
            let after = (param.span.start + 4) as usize;
            let blanks = content
                .get(after..)
                .map_or(1, |rest| rest.len() - rest.trim_start().len());
            name_start = param.span.start + 4 + blanks as u32;
        }
        out.push(RawToken {
            span: Span::new(name_start, name_start + param.name.as_str().len() as u32),
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
            } => collect_fn_tokens(name, params, body, *span, out),
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
            Member::Change {
                watched,
                action,
                span,
            } => {
                out.push(RawToken {
                    span: Span::new(span.start, span.start + 2), // "on"
                    token_type: 0,                               // KEYWORD
                    modifiers: 0,
                });
                for value in watched {
                    collect_expr_tokens(value, out);
                }
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

/// `fn name(params) => body`, in a view or at the top of a file.
fn collect_fn_tokens(
    name: &Symbol,
    params: &[Param],
    body: &Expr,
    span: Span,
    out: &mut Vec<RawToken>,
) {
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

fn collect_element_tokens(el: &ElementNode, out: &mut Vec<RawToken>) {
    let name_len = el.name.as_str().len() as u32;

    out.push(RawToken {
        span: Span::new(el.span.start, el.span.start + name_len),
        token_type: 9, // CLASS
        modifiers: 0,
    });

    collect_attr_tokens(&el.attrs, out);

    if let Some(action) = &el.action {
        collect_expr_tokens(action, out);
    }

    collect_member_tokens(&el.children, out);
}

/// The tokens of an attribute list, `#[name: value, on_tap => action]`, on an
/// element or inside a `style { … }` value.
fn collect_attr_tokens(attrs: &[Attr], out: &mut Vec<RawToken>) {
    for attr in attrs {
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
}

/// The tokens of an expression and of everything inside it.
///
/// Every kind of expression is named, with no catch-all: a new one must say
/// here what it contains, or the compiler says it was forgotten, rather than
/// the editor quietly leaving what is inside it uncoloured.
fn collect_expr_tokens(expr: &Expr, out: &mut Vec<RawToken>) {
    match expr {
        Expr::Ident(_, span) => {
            out.push(RawToken {
                span: *span,
                token_type: 3, // VARIABLE
                modifiers: 0,
            });
        }
        Expr::IntLit(_, span) | Expr::FloatLit(_, span) | Expr::AngleLit(_, span) => {
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
                if let StrPart::Interp(inner, _) = part {
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
        Expr::Tuple(args, _) => {
            for arg in args {
                collect_expr_tokens(&arg.value, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_tokens(lhs, out);
            collect_expr_tokens(rhs, out);
        }
        Expr::Merge { left, right, .. } => {
            collect_expr_tokens(left, out);
            collect_expr_tokens(right, out);
        }
        Expr::Unary { rhs, .. } => collect_expr_tokens(rhs, out),
        Expr::Ternary {
            cond, then, els, ..
        } => {
            collect_expr_tokens(cond, out);
            collect_expr_tokens(then, out);
            collect_expr_tokens(els, out);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_expr_tokens(cond, out);
            collect_expr_tokens(then, out);
            if let Some(els) = els {
                collect_expr_tokens(els, out);
            }
        }
        Expr::Index { base, index, .. } => {
            collect_expr_tokens(base, out);
            collect_expr_tokens(index, out);
        }
        Expr::Record { fields, spread, .. } => {
            if let Some(spread) = spread {
                collect_expr_tokens(spread, out);
            }
            for (_, value) in fields {
                collect_expr_tokens(value, out);
            }
        }
        Expr::Lambda { body, .. } => collect_expr_tokens(body, out),
        Expr::Block(statements, _) => {
            for statement in statements {
                collect_expr_tokens(statement, out);
            }
        }
        Expr::Let { value, .. } | Expr::KeyframeStep { value, .. } => {
            collect_expr_tokens(value, out);
        }
        Expr::Assign { target, value, .. } => {
            collect_expr_tokens(target, out);
            collect_expr_tokens(value, out);
        }
        Expr::Postfix { target, .. } => collect_expr_tokens(target, out),
        Expr::ControllerCall { call, ok, err, .. } => {
            collect_expr_tokens(call, out);
            for arm in [ok, err].into_iter().flatten() {
                collect_expr_tokens(&arm.action, out);
            }
        }
        Expr::Animated { value, anim, .. } => {
            collect_expr_tokens(value, out);
            collect_expr_tokens(anim, out);
        }
        Expr::StyleValue { attrs, states, .. } => {
            collect_attr_tokens(attrs, out);
            for state in states {
                collect_attr_tokens(&state.attrs, out);
            }
        }
        // Nothing was parsed here, so there is nothing to colour.
        Expr::Error(_) => {}
    }
}
