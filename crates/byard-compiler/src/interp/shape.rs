//! Whether a value has the shape a record type declares.
//!
//! A record type (`type Place = { name: Str, latitude: Float }`) is checked
//! where code reads a value. A value that arrives from outside, a JSON reply,
//! is only a record of whatever the server sent, so the interpreter compares
//! it with the type the moment it is written into a `var` declared with one.
//! What it reports is the first difference, in words a person can act on:
//! "item 2 has no `country`".

use std::collections::HashMap;

use super::env::Value;
use crate::parser::ast::{RecordDecl, Type};
use crate::symbol::Symbol;

/// The program's record types, by name.
pub type Records = HashMap<Symbol, RecordDecl>;

/// Whether `ty` mentions a record type, which is when a `var` of it is worth
/// checking: a `List<Int>` is the checker's business, a `List<Place>` is the
/// server's.
#[must_use]
pub fn names_a_record(ty: &Type, records: &Records) -> bool {
    match ty {
        Type::Named { name, args, .. } => {
            records.contains_key(name) || args.iter().any(|a| names_a_record(a, records))
        }
        Type::Function { .. } => false,
    }
}

/// The first way `value` differs from `ty`, or `None` when it has the shape.
#[must_use]
pub fn difference(value: &Value, ty: &Type, records: &Records) -> Option<String> {
    differ(value, ty, records, 0)
}

/// Deep enough for any data a person writes a type for, and a guard against
/// a recursive type meeting a cyclic value.
const MAX_DEPTH: usize = 32;

fn differ(value: &Value, ty: &Type, records: &Records, depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    let Type::Named { name, args, .. } = ty else {
        return None;
    };
    let kind = |v: &Value| match v {
        Value::Unit => "nothing".to_string(),
        Value::Int(_) => "a number".to_string(),
        Value::Float(_) => "a number".to_string(),
        Value::Str(_) => "text".to_string(),
        Value::Bool(_) => "true or false".to_string(),
        Value::List(_) => "a list".to_string(),
        Value::Record(_) => "a record".to_string(),
        _ => "something else".to_string(),
    };
    let expected = |what: &str| Some(format!("is {}, not {what}", kind(value)));
    match (name.as_str(), value) {
        ("Str", Value::Str(_))
        | ("Int", Value::Int(_))
        | ("Float", Value::Float(_) | Value::Int(_))
        | ("Bool", Value::Bool(_)) => None,
        ("Str", _) => expected("text"),
        ("Int" | "Float", _) => expected("a number"),
        ("Bool", _) => expected("true or false"),
        ("List", Value::List(items)) => {
            let element = args.first()?;
            items.iter().enumerate().find_map(|(i, item)| {
                differ(item, element, records, depth + 1).map(|why| format!("item {i} {why}"))
            })
        }
        ("List", _) => expected("a list"),
        (record, _) => {
            let decl = records.get(name)?;
            let Value::Record(fields) = value else {
                return expected(&format!("a `{record}`"));
            };
            decl.fields.iter().find_map(|field| {
                let found = fields
                    .iter()
                    .find(|(k, _)| *k == field.name)
                    .map(|(_, v)| v)
                    .filter(|v| !matches!(v, Value::Unit));
                match found {
                    None if field.optional => None,
                    None => Some(format!("has no `{}`", field.name.as_str())),
                    Some(v) => differ(v, &field.ty, records, depth + 1)
                        .map(|why| format!("has a `{}` that {why}", field.name.as_str())),
                }
            })
        }
    }
}

/// `ty` as written, for a message: `List<Place>`.
#[must_use]
pub fn written(ty: &Type) -> String {
    match ty {
        Type::Named { name, args, .. } if args.is_empty() => name.as_str().to_string(),
        Type::Named { name, args, .. } => format!(
            "{}<{}>",
            name.as_str(),
            args.iter().map(written).collect::<Vec<_>>().join(", ")
        ),
        Type::Function { .. } => "Fn".to_string(),
    }
}
