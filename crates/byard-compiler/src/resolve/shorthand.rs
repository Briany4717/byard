//! The short form of an enum case (RFC-0055 §4): `variant: .tonal` for
//! `variant: Variant.tonal`, where the parameter it is passed to is declared
//! `: Variant`.
//!
//! The resolver is the one pass that holds both sides, a call and the
//! parameters of the view it calls, so it is where the rewrite is done. After
//! it the checker and the interpreter see the long form and nothing about the
//! short one exists at run time.

use std::collections::{HashMap, HashSet};

use crate::parser::ast::{ElementNode, EnumDecl, Expr, Member, Param, Type, ViewDecl};
use crate::symbol::Symbol;

/// What a call needs to know about one parameter of the view it calls.
struct Slot {
    name: Symbol,
    /// The enum the parameter is declared as, canonically named.
    enum_name: Option<Symbol>,
    /// Whether a positional argument can fill it: not the child slot, not a
    /// callback.
    positional: bool,
}

/// Rewrites, in `views`, every `.case` that is the argument of a parameter
/// typed with one of `enums`, and every parameter default of that kind.
pub fn rewrite(views: &mut [ViewDecl], enums: &[EnumDecl]) {
    if enums.is_empty() {
        return;
    }
    let known: HashSet<&Symbol> = enums.iter().map(|e| &e.name).collect();
    let slot = |p: &Param| Slot {
        name: p.name.clone(),
        enum_name: match &p.ty {
            Some(Type::Named { name, args, .. }) if args.is_empty() && known.contains(name) => {
                Some(name.clone())
            }
            _ => None,
        },
        positional: p.name.as_str() != crate::interp::eval::RESERVED_CONTENT
            && !matches!(p.ty, Some(Type::Function { .. })),
    };
    // A name two views share cannot be told apart here; it is left alone, as
    // the checker leaves it.
    let mut table: HashMap<Symbol, Option<Vec<Slot>>> = HashMap::new();
    for view in views.iter() {
        let slots = view.params.iter().map(slot).collect();
        table
            .entry(view.name.clone())
            .and_modify(|e| *e = None)
            .or_insert(Some(slots));
    }
    table.retain(|_, slots| {
        slots
            .as_ref()
            .is_some_and(|s| s.iter().any(|p| p.enum_name.is_some()))
    });
    for view in views.iter_mut() {
        let declared: Vec<Option<Symbol>> = view.params.iter().map(|p| slot(p).enum_name).collect();
        for (param, enum_name) in view.params.iter_mut().zip(declared) {
            if let (Some(default), Some(enum_name)) = (&mut param.default, enum_name) {
                case(default, &enum_name);
            }
        }
        if !table.is_empty() {
            for member in &mut view.body {
                walk(member, &table);
            }
        }
    }
}

fn walk(member: &mut Member, table: &HashMap<Symbol, Option<Vec<Slot>>>) {
    match member {
        Member::Element(el) => {
            call(el, table);
            for child in &mut el.children {
                walk(child, table);
            }
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
                    crate::parser::ast::Expr::SlotBlock { members, .. }
                    | crate::parser::ast::Expr::Lambda {
                        slot: Some(members),
                        ..
                    } => Some(members),
                    _ => None,
                };
                for m in members.into_iter().flatten() {
                    walk(m, table);
                }
            }
        }
        Member::For { body, .. } | Member::Route { body, .. } => {
            for m in body {
                walk(m, table);
            }
        }
        Member::When { then, els, .. } => {
            for m in then {
                walk(m, table);
            }
            if let Some(els) = els {
                for m in els {
                    walk(m, table);
                }
            }
        }
        _ => {}
    }
}

/// The argument matching of a call, as the checker does it: positional
/// arguments fill the value parameters in order, named ones and attributes
/// match by name.
fn call(el: &mut ElementNode, table: &HashMap<Symbol, Option<Vec<Slot>>>) {
    let Some(Some(slots)) = table.get(&el.name) else {
        return;
    };
    let mut positional = slots.iter().filter(|s| s.positional);
    for arg in &mut el.content {
        let slot = match &arg.name {
            Some(name) => slots.iter().find(|s| &s.name == name),
            None => positional.next(),
        };
        if let Some(enum_name) = slot.and_then(|s| s.enum_name.as_ref()) {
            case(&mut arg.value, enum_name);
        }
    }
    for attr in &mut el.attrs {
        if let crate::parser::ast::AttrKind::Prop { value } = &mut attr.kind {
            if let Some(enum_name) = slots
                .iter()
                .find(|s| s.name == attr.name)
                .and_then(|s| s.enum_name.as_ref())
            {
                case(value, enum_name);
            }
        }
    }
}

/// `.case` becomes `Enum.case`, through the arms of a `?:`.
fn case(expr: &mut Expr, enum_name: &Symbol) {
    match expr {
        Expr::ClassRef(name, span) => {
            *expr = Expr::Member {
                base: Box::new(Expr::Ident(enum_name.clone(), *span)),
                field: name.clone(),
                span: *span,
            };
        }
        Expr::Ternary { then, els, .. } => {
            case(then, enum_name);
            case(els, enum_name);
        }
        _ => {}
    }
}
