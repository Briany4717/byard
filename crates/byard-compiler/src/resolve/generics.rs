//! Types with parameters: `type Page<T> = { items: List<T>, total: Int }`.
//!
//! Each use with arguments, `Page<Item>`, is a record type of its own, named
//! as it is written (`Page<Item>`), whose fields are the declaration's with
//! `T` replaced by `Item`. The resolver makes one for every use the program
//! writes and rewrites the use to name it, so the checker and the interpreter
//! see ordinary record types: a `Page<Item>`'s `items` is a `List<Item>`, a
//! literal written for one is checked against those fields, and a reply cast
//! `as Page<Item>` is compared with them when it arrives.
//!
//! A use that gives a type the wrong number of arguments is told
//! (`TypeArgumentCount`) and left naming the declaration, whose fields then
//! read as their parameters, which nothing checks.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::diagnostics::CompileError;
use crate::parser::ast::{ConstDecl, EnumDecl, FnDecl, RecordDecl, Type, ViewDecl};
use crate::resolve::walk;
use crate::symbol::Symbol;

/// How many types one program may make from its declarations. A type whose
/// fields use it with a larger argument (`type A<T> = { next: A<List<T>>? }`)
/// would make one for ever; past this the uses are left as they are written.
const MOST_INSTANCES: usize = 1024;

/// Makes the record type of every use with arguments in the program, adds
/// them to `records`, and rewrites the uses to name them.
pub fn instantiate(
    views: &mut [ViewDecl],
    fns: &mut [FnDecl],
    consts: &mut [ConstDecl],
    enums: &mut [EnumDecl],
    records: &mut Vec<RecordDecl>,
    errors: &mut Vec<CompileError>,
) {
    let templates: HashMap<Symbol, RecordDecl> = records
        .iter()
        .filter(|r| !r.type_params.is_empty())
        .map(|r| (r.name.clone(), r.clone()))
        .collect();
    // The program's own types that take no arguments.
    let plain: HashSet<Symbol> = records
        .iter()
        .filter(|r| r.type_params.is_empty())
        .map(|r| r.name.clone())
        .chain(enums.iter().map(|e| e.name.clone()))
        .collect();
    let mut made = Made {
        templates: &templates,
        plain: &plain,
        instances: HashSet::new(),
        pending: VecDeque::new(),
        errors: Vec::new(),
    };

    for view in views.iter_mut() {
        walk::view_types(view, &mut |t| made.rewrite(t));
    }
    for decl in fns.iter_mut() {
        for p in &mut decl.params {
            walk::param_types(p, &mut |t| made.rewrite(t));
        }
        if let Some(t) = &mut decl.ret {
            made.rewrite(t);
        }
        walk::expr(&mut decl.body, &mut |e| {
            walk::expr_types(e, &mut |t| made.rewrite(t));
        });
    }
    for decl in consts.iter_mut() {
        if let Some(t) = &mut decl.ty {
            made.rewrite(t);
        }
        walk::expr(&mut decl.init, &mut |e| {
            walk::expr_types(e, &mut |t| made.rewrite(t));
        });
    }
    for decl in enums.iter_mut() {
        for (_, fields) in &mut decl.fields {
            for (_, t) in fields {
                made.rewrite(t);
            }
        }
    }
    // A declaration's own fields are rewritten only in the types made from it,
    // where its parameters have been replaced.
    for decl in records.iter_mut().filter(|r| r.type_params.is_empty()) {
        for field in &mut decl.fields {
            made.rewrite(&mut field.ty);
            if let Some(default) = &mut field.default {
                walk::expr(default, &mut |e| {
                    walk::expr_types(e, &mut |t| made.rewrite(t));
                });
            }
        }
    }
    // A type made for a use may use others in its fields: those are made in
    // turn, until none is new.
    let mut added = Vec::new();
    while let Some(mut decl) = made.pending.pop_front() {
        for field in &mut decl.fields {
            made.rewrite(&mut field.ty);
        }
        added.push(decl);
    }
    records.extend(added);
    for e in made.errors {
        if !errors.contains(&e) {
            errors.push(e);
        }
    }
}

struct Made<'a> {
    templates: &'a HashMap<Symbol, RecordDecl>,
    plain: &'a HashSet<Symbol>,
    /// The name of every type made so far.
    instances: HashSet<Symbol>,
    /// Types made whose fields are still to be rewritten.
    pending: VecDeque<RecordDecl>,
    errors: Vec<CompileError>,
}

impl Made<'_> {
    /// Rewrites the uses inside `t`, then `t` itself.
    fn rewrite(&mut self, t: &mut Type) {
        match t {
            Type::Named { args, .. } => {
                for a in args.iter_mut() {
                    self.rewrite(a);
                }
            }
            Type::Optional { inner, .. } => self.rewrite(inner),
            Type::Function { params, ret, .. } => {
                for p in params.iter_mut() {
                    self.rewrite(p);
                }
                if let Some(r) = ret {
                    self.rewrite(r);
                }
            }
        }
        let Type::Named { name, args, span } = t else {
            return;
        };
        if self.plain.contains(name) && !args.is_empty() {
            self.errors.push(CompileError::TypeArgumentCount {
                span: *span,
                name: name.as_str().to_string(),
                params: Vec::new(),
                found: args.len(),
            });
            args.clear();
            return;
        }
        let Some(template) = self.templates.get(name) else {
            return;
        };
        if args.len() != template.type_params.len() {
            self.errors.push(CompileError::TypeArgumentCount {
                span: *span,
                name: name.as_str().to_string(),
                params: template
                    .type_params
                    .iter()
                    .map(|p| p.as_str().to_string())
                    .collect(),
                found: args.len(),
            });
            args.clear();
            return;
        }
        let instance = Symbol::intern(&spell(t));
        let Type::Named { name, args, .. } = t else {
            return;
        };
        if !self.instances.contains(&instance) {
            if self.instances.len() >= MOST_INSTANCES {
                return;
            }
            self.instances.insert(instance.clone());
            let given: HashMap<&Symbol, &Type> =
                template.type_params.iter().zip(args.iter()).collect();
            let mut decl = template.clone();
            decl.name = instance.clone();
            decl.type_params = Vec::new();
            for field in &mut decl.fields {
                field.ty = substitute(&field.ty, &given);
            }
            self.pending.push_back(decl);
        }
        *name = instance;
        args.clear();
    }
}

/// `ty` with each parameter `given` names replaced by the type it is given.
fn substitute(ty: &Type, given: &HashMap<&Symbol, &Type>) -> Type {
    match ty {
        Type::Named { name, args, span } => {
            if args.is_empty() {
                if let Some(&to) = given.get(name) {
                    return to.clone();
                }
            }
            Type::Named {
                name: name.clone(),
                args: args.iter().map(|a| substitute(a, given)).collect(),
                span: *span,
            }
        }
        Type::Optional { inner, span } => Type::Optional {
            inner: Box::new(substitute(inner, given)),
            span: *span,
        },
        Type::Function { params, ret, span } => Type::Function {
            params: params.iter().map(|p| substitute(p, given)).collect(),
            ret: ret.as_ref().map(|r| Box::new(substitute(r, given))),
            span: *span,
        },
    }
}

/// A type as it is written: `Page<List<Item>>`, `Item?`, `Fn(Int) -> Str`.
fn spell(ty: &Type) -> String {
    match ty {
        Type::Named { name, args, .. } if args.is_empty() => name.as_str().to_string(),
        Type::Named { name, args, .. } => format!(
            "{}<{}>",
            name.as_str(),
            args.iter().map(spell).collect::<Vec<_>>().join(", ")
        ),
        Type::Optional { inner, .. } => format!("{}?", spell(inner)),
        Type::Function { params, ret, .. } => {
            let params = params.iter().map(spell).collect::<Vec<_>>().join(", ");
            match ret {
                Some(r) => format!("Fn({params}) -> {}", spell(r)),
                None => format!("Fn({params})"),
            }
        }
    }
}
