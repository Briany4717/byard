//! Whether a value has the shape a record type declares.
//!
//! A record type (`type Place = { name: Str, latitude: Float }`) is checked
//! where code reads a value. A value that arrives from outside, a JSON reply,
//! is only a record of whatever the server sent, so the interpreter compares
//! it with the type the moment it is written into a `var` declared with one.
//! What it reports is the first difference, in words a person can act on:
//! "item 2 has no `country`".

use std::collections::HashMap;
use std::fmt::Write as _;

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
        Type::Optional { inner, .. } => names_a_record(inner, records),
        Type::Function { .. } => false,
    }
}

/// One step down into a value: a field of a record, or an item of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Field(Symbol),
    Item(usize),
}

/// What is wrong at the end of a path.
enum Leaf {
    /// The value there is not what the type says: `is text, not a number`.
    Is(String),
    /// The record there lacks a field the type has.
    Missing(Symbol),
}

/// A difference: where it is, and what it is.
struct Diff {
    steps: Vec<Step>,
    leaf: Leaf,
}

/// The first way `value` differs from `ty`, or `None` when it has the shape.
#[must_use]
pub fn difference(value: &Value, ty: &Type, records: &Records) -> Option<String> {
    let diff = differ(value, ty, records, 0)?;
    let mut said = String::new();
    for step in &diff.steps {
        match step {
            Step::Field(f) => {
                let _ = write!(said, "has a `{}` that ", f.as_str());
            }
            Step::Item(i) => {
                let _ = write!(said, "item {i} ");
            }
        }
    }
    match diff.leaf {
        Leaf::Is(why) => said.push_str(&why),
        Leaf::Missing(field) => {
            let _ = write!(said, "has no `{}`", field.as_str());
        }
    }
    Some(said)
}

/// The first difference as where it is and what it is: `daily[3].max` and
/// `is text, not a number`. The path is empty for the value itself.
#[must_use]
pub fn difference_at(value: &Value, ty: &Type, records: &Records) -> Option<(String, String)> {
    let diff = differ(value, ty, records, 0)?;
    let mut path = String::new();
    for step in &diff.steps {
        match step {
            Step::Field(f) if path.is_empty() => path.push_str(f.as_str()),
            Step::Field(f) => {
                path.push('.');
                path.push_str(f.as_str());
            }
            Step::Item(i) => {
                let _ = write!(path, "[{i}]");
            }
        }
    }
    let why = match diff.leaf {
        Leaf::Is(why) => why,
        Leaf::Missing(field) => {
            if path.is_empty() {
                path.push_str(field.as_str());
            } else {
                path.push('.');
                path.push_str(field.as_str());
            }
            format!("has no `{}`", field.as_str())
        }
    };
    Some((path, why))
}

/// Deep enough for any data a person writes a type for, and a guard against
/// a recursive type meeting a cyclic value.
const MAX_DEPTH: usize = 32;

fn differ(value: &Value, ty: &Type, records: &Records, depth: usize) -> Option<Diff> {
    if depth > MAX_DEPTH {
        return None;
    }
    let Type::Named { name, args, .. } = ty else {
        return None;
    };
    let kind = |v: &Value| match v {
        Value::Unit => "nothing".to_string(),
        Value::Int(_) | Value::Float(_) => "a number".to_string(),
        Value::Str(_) => "text".to_string(),
        Value::Bool(_) => "true or false".to_string(),
        Value::List(_) => "a list".to_string(),
        Value::Record(_) => "a record".to_string(),
        _ => "something else".to_string(),
    };
    let expected = |what: &str| {
        Some(Diff {
            steps: Vec::new(),
            leaf: Leaf::Is(format!("is {}, not {what}", kind(value))),
        })
    };
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
                differ(item, element, records, depth + 1).map(|mut d| {
                    d.steps.insert(0, Step::Item(i));
                    d
                })
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
                    None => Some(Diff {
                        steps: Vec::new(),
                        leaf: Leaf::Missing(field.name.clone()),
                    }),
                    Some(v) => differ(v, &field.ty, records, depth + 1).map(|mut d| {
                        d.steps.insert(0, Step::Field(field.name.clone()));
                        d
                    }),
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
        Type::Optional { inner, .. } => format!("{}?", written(inner)),
        Type::Function { .. } => "Fn".to_string(),
    }
}
