//! Icon fonts (RFC-0054): which names a program uses, and which of its `Text`
//! the build cannot read.
//!
//! An icon font's glyphs are ligatures named by a string, so "the glyphs the
//! app needs" is "the names the app writes". Two questions follow, answered
//! here without running the program:
//!
//! - [`string_literals`]: every string literal in a source file. A name
//!   reaches a `Text` through a literal, a `var`, a `let` or a view argument
//!   (`Icon("favorite")`), and all of those start as a literal somewhere, so
//!   the set of literals contains the set of names. A literal that is not a
//!   name costs nothing: the caller keeps only the ones the font ligates.
//! - [`dynamic_icon_texts`]: the `Text` in an icon family whose content is
//!   built at run time (a concatenation, an interpolation, a call). Its name
//!   is in no literal, so the font cannot be cut for it.

use crate::diagnostics::Span;
use crate::lexer::{Token, lex};
use crate::parser::ast::{ElementNode, Expr, Member, StrPart};

/// The text of every string literal in `source` that has no interpolation and
/// no escape, in source order.
#[must_use]
pub fn string_literals(source: &str) -> Vec<String> {
    lex(source)
        .tokens
        .iter()
        .filter(|(t, _)| *t == Token::StrLit)
        .filter_map(|(_, span)| {
            let raw = source.get(span.start as usize..span.end as usize)?;
            let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
            (!inner.contains(['{', '\\'])).then(|| inner.to_string())
        })
        .collect()
}

/// Whether `el` is a `Text` in one of the `icons` families (named as the
/// manifest declares them) whose content is built at run time. A literal, or
/// a name (a parameter, a `var`, a `let`), is static: the literals it comes
/// from are in the source.
#[must_use]
pub fn dynamic_icon_text<'a>(el: &ElementNode, icons: &'a [String]) -> Option<&'a String> {
    if el.name.as_str() != "Text" {
        return None;
    }
    let family = el.attrs.iter().find_map(|a| match &a.kind {
        crate::parser::ast::AttrKind::Prop {
            value: Expr::Ident(sym, _),
        } if a.name.as_str() == "font" => icons.iter().find(|n| {
            n.as_str() == sym.as_str() || crate::interp::theme::to_camel(n) == sym.as_str()
        }),
        _ => None,
    })?;
    let dynamic = match el.content.first().map(|a| &a.value) {
        Some(Expr::StrLit(parts, _)) => parts.iter().any(|p| !matches!(p, StrPart::Text(_))),
        Some(Expr::Ident(..)) => false,
        _ => true,
    };
    dynamic.then_some(family)
}

/// The spans of every [`dynamic_icon_text`] in `members`.
pub fn dynamic_icon_texts(members: &[Member], icons: &[String], out: &mut Vec<Span>) {
    for member in members {
        match member {
            Member::Element(el) => {
                if dynamic_icon_text(el, icons).is_some() {
                    out.push(el.span);
                }
                dynamic_icon_texts(&el.children, icons, out);
            }
            Member::For { body, .. } | Member::Route { body, .. } => {
                dynamic_icon_texts(body, icons, out);
            }
            Member::When { then, els, .. } => {
                dynamic_icon_texts(then, icons, out);
                if let Some(e) = els {
                    dynamic_icon_texts(e, icons, out);
                }
            }
            _ => {}
        }
    }
}
