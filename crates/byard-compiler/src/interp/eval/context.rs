//! Context (RFC-0065): `context` declarations, `provide`, and `inject`.
//!
//! A view that `provide`s a context keeps one node in a chain
//! ([`CtxNode`]) for as long as it is being lowered; every view lowered under
//! it, the ones it draws and the blocks its caller wrote for its slots (which
//! are lowered where the callee places them), sees the chain as it was when
//! they were reached. `inject` finds the nearest node of the name, or builds
//! the defaults when there is none. The handle it binds is a record whose
//! fields are the provider's own memos, so a read is a signal read and a
//! change in the provider reaches every reader.
//!
//! A view that neither provides nor injects a context does nothing here: no
//! node, and the chain is a pointer it carries along.

use std::rc::Rc;

use super::{CtxNode, Interpreter};
use crate::interp::env::Value;
use crate::parser::ast::{Expr, Member, RecordField, Type, ViewDecl};
use crate::symbol::Symbol;

/// The field a context handle starts with, which tells it from a record of
/// data: no program can write the name.
const HANDLE_MARK: &str = "$context";

/// Whether `expr` is the word `none`.
fn is_none(expr: &Expr) -> bool {
    matches!(expr, Expr::Ident(n, _) if n.as_str() == "none")
}

impl Interpreter {
    /// Gives, for every `provide` of `view`, a node to the chain: the values
    /// the view wrote, and the context's defaults for the fields it left out.
    /// Done after the view's other declarations, so a `provide` can name any
    /// `var` of the view wherever it is written.
    pub(super) fn provide_contexts(&mut self, view: &ViewDecl) {
        for member in &view.body {
            let Member::Provide {
                ty: Type::Named { name, .. },
                fields,
                ..
            } = member
            else {
                continue;
            };
            // A name that is no context was told by the checker.
            let Some(decl) = self.contexts.get(name).cloned() else {
                continue;
            };
            let mut values = Vec::with_capacity(decl.len());
            for field in decl.iter() {
                let written = fields
                    .iter()
                    .find(|arg| arg.name.as_ref() == Some(&field.name));
                let value = match written {
                    Some(arg) => self.provided_value(field, &arg.value),
                    None => self.field_default(field),
                };
                values.push((field.name.clone(), value));
            }
            let parent = self.ctx_chain.take();
            self.ctx_chain = Some(Rc::new(CtxNode {
                parent,
                name: name.clone(),
                fields: values,
            }));
        }
    }

    /// What a `provide` wrote for `field`: a callback for an `Fn`, nothing for
    /// `none`, else a memo of the expression in the providing view's scope.
    fn provided_value(&mut self, field: &RecordField, expr: &Expr) -> Value {
        if is_none(expr) {
            return Value::Unit;
        }
        if let Type::Function { params, .. } = &field.ty {
            return self.callback_value(expr, params.len());
        }
        Value::Memo(self.project_arg(expr))
    }

    /// A callback written in a `provide` or a default: the block, captured
    /// among the names where it was written; a callback of the view passed on
    /// by name; or nothing.
    fn callback_value(&mut self, expr: &Expr, arity: usize) -> Value {
        match expr {
            Expr::Lambda { params, body, .. } => {
                let captured = self.env.capture(self.env.len());
                Value::Fn(self.register_callback(params, body, captured))
            }
            Expr::Ident(name, span) => match self.env.lookup(name).cloned() {
                Some(value @ (Value::Fn(_) | Value::Unit)) => value,
                _ => Value::Fn(self.noop_callback(arity, *span)),
            },
            _ => Value::Unit,
        }
    }

    /// The value of `field` when nothing provides it: its default, read among
    /// the program's constants and nothing of the view.
    fn field_default(&mut self, field: &RecordField) -> Value {
        let Some(default) = &field.default else {
            return Value::Unit;
        };
        if is_none(default) {
            return Value::Unit;
        }
        let frame = self
            .env
            .enter_closure(crate::interp::env::Captured::from([]));
        let value = match (&field.ty, default) {
            (Type::Function { .. }, Expr::Lambda { params, body, .. }) => Value::Fn(
                self.register_callback(params, body, crate::interp::env::Captured::from([])),
            ),
            (Type::Function { .. }, _) => Value::Unit,
            _ => Value::Memo(self.project_arg(default)),
        };
        self.env.leave_closure(frame);
        value
    }

    /// `inject Name as handle` for a context: the nearest provider's fields,
    /// or the defaults.
    pub(super) fn inject_context(&mut self, context: &Symbol, handle: &Symbol) {
        let mut node = self.ctx_chain.as_ref();
        let found = loop {
            match node {
                Some(n) if n.name == *context => break Some(n.fields.clone()),
                Some(n) => node = n.parent.as_ref(),
                None => break None,
            }
        };
        let fields = if let Some(fields) = found {
            fields
        } else {
            let decl = self.contexts.get(context).cloned().unwrap_or_default();
            decl.iter()
                .map(|f| (f.name.clone(), self.field_default(f)))
                .collect()
        };
        let mut record = Vec::with_capacity(fields.len() + 1);
        record.push((Symbol::intern(HANDLE_MARK), Value::Unit));
        record.extend(fields);
        self.env.push(handle.clone(), Value::Record(record));
    }

    /// The value of `field` on `base` when `base` is the handle of an injected
    /// context.
    pub(super) fn context_field(&self, base: &Expr, field: &Symbol) -> Option<Value> {
        let Expr::Ident(name, _) = base else {
            return None;
        };
        if self.lambda_params.contains(name) {
            return None;
        }
        let Value::Record(fields) = self.env.lookup(name)? else {
            return None;
        };
        if fields.first().map(|(k, _)| k.as_str()) != Some(HANDLE_MARK) {
            return None;
        }
        fields
            .iter()
            .find(|(k, _)| k == field)
            .map(|(_, v)| v.clone())
    }
}
