//! JSON as data, not as text (RFC-0029 O3).
//!
//! Two total functions between `serde_json::Value` and [`HostValue`], plus the
//! [`Json`] capability that exposes them to `byld`. The mapping is the reason
//! an API response needs no glue at all: a JSON object *is* an RFC-0027
//! record, a JSON array *is* a list, so `res.json.current.temperature_2m`
//! reads a parsed response the same way it would read a literal.
//!
//! ## Key order is part of the value
//!
//! `serde_json` is built here with `preserve_order`, so an object keeps the
//! order it was written in. RFC-0027 records are ordered, and a `for` over one
//! renders in that order, so a map-backed parse would make a list of fields
//! come back shuffled between runs, reproducible only in the sense that it is
//! always wrong somewhere.
//!
//! ## Numbers
//!
//! JSON has one number type and `byld` has two. An integral number becomes
//! `Int` and a fractional one becomes `Float`, which is what a reader of the
//! source expects (`"status": 200` is an integer) and what keeps arithmetic on
//! a parsed field from silently going floating point. A number too large for
//! `i64` degrades to `Float` rather than failing the parse: losing precision on
//! an absurd integer is a better answer than losing the whole response.

use crate::bridge::{BoxFuture, Controller, HostValue};

/// Converts a parsed JSON value into the boundary type (RFC-0029 O3).
///
/// Total: every `serde_json::Value` has a [`HostValue`] form, so a parse that
/// succeeded can never fail to cross.
#[must_use]
pub fn json_to_host(value: &serde_json::Value) -> HostValue {
    match value {
        serde_json::Value::Null => HostValue::Unit,
        serde_json::Value::Bool(b) => HostValue::Bool(*b),
        serde_json::Value::Number(n) => number_to_host(n),
        serde_json::Value::String(s) => HostValue::Str(s.clone()),
        serde_json::Value::Array(items) => {
            HostValue::List(items.iter().map(json_to_host).collect())
        }
        serde_json::Value::Object(fields) => HostValue::Record(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), json_to_host(v)))
                .collect(),
        ),
    }
}

/// One JSON number, as the `byld` scalar that reads correctly at the use site.
fn number_to_host(n: &serde_json::Number) -> HostValue {
    if let Some(i) = n.as_i64() {
        return HostValue::Int(i);
    }
    // A `u64` above `i64::MAX`, or a float. Both land as `Float`; the
    // alternative for the first is refusing the value, which would fail a
    // whole response over one field nobody was going to do arithmetic on.
    HostValue::Float(n.as_f64().unwrap_or(0.0))
}

/// Converts a boundary value into JSON (RFC-0029 O3), the inverse of
/// [`json_to_host`].
///
/// Total in the other direction too: `Unit` is `null`, and every other variant
/// has an exact JSON form, so a record written in `byld` and persisted (O5)
/// comes back as itself.
#[must_use]
pub fn host_to_json(value: &HostValue) -> serde_json::Value {
    match value {
        HostValue::Unit => serde_json::Value::Null,
        HostValue::Bool(b) => serde_json::Value::Bool(*b),
        HostValue::Int(n) => serde_json::Value::Number((*n).into()),
        HostValue::Float(f) => serde_json::Number::from_f64(*f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        HostValue::Str(s) => serde_json::Value::String(s.clone()),
        HostValue::List(items) => {
            serde_json::Value::Array(items.iter().map(host_to_json).collect())
        }
        HostValue::Record(fields) => serde_json::Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), host_to_json(v)))
                .collect(),
        ),
    }
}

/// Parses `text`, or `None` if it is not JSON.
///
/// Separate from the capability so [`Http`](super::http::Http) can populate a
/// response's `.json` field without going through a controller call
/// (RFC-0029 §3).
#[must_use]
pub fn parse(text: &str) -> Option<HostValue> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .map(|v| json_to_host(&v))
}

/// The `Json` capability (RFC-0029 O3): `json.parse(str)` and
/// `json.stringify(value)`.
///
/// Provided even though `Http` already parses its own responses, because JSON
/// arrives by other routes too, a file, a websocket frame, a query string, and
/// a language whose only parser is attached to its HTTP client has an HTTP
/// client, not a JSON parser.
#[derive(Debug, Default, Clone, Copy)]
pub struct Json;

impl Controller for Json {
    fn type_name(&self) -> &'static str {
        "Json"
    }

    fn invoke(
        &self,
        method: &str,
        args: Vec<HostValue>,
    ) -> BoxFuture<'static, Result<HostValue, HostValue>> {
        let out = match method {
            "parse" => match args.first() {
                Some(HostValue::Str(text)) => parse(text).ok_or_else(|| {
                    // Malformed input is an ordinary failure of a parse, not a
                    // panic and not a silent `Unit` (INV-4): the caller's `err`
                    // arm is exactly where it belongs.
                    error("invalid_json", "the text is not valid JSON")
                }),
                _ => Err(error("bad_argument", "`json.parse` takes one string")),
            },
            "stringify" => Ok(HostValue::Str(
                host_to_json(args.first().unwrap_or(&HostValue::Unit)).to_string(),
            )),
            other => Err(error(
                "unknown_method",
                &format!("`Json` has no method `{other}`"),
            )),
        };
        Box::pin(async move { out })
    }
}

/// The shape every capability's `err` arm receives (RFC-0029 §2): a record with
/// a machine-readable `kind` and a human-readable `message`, so a view can
/// branch on the first and show the second.
#[must_use]
pub fn error(kind: &str, message: &str) -> HostValue {
    HostValue::Record(vec![
        ("kind".to_string(), HostValue::Str(kind.to_string())),
        ("message".to_string(), HostValue::Str(message.to_string())),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, args: Vec<HostValue>) -> Result<HostValue, HostValue> {
        pollster::block_on(Json.invoke(method, args))
    }

    #[test]
    fn an_object_becomes_a_record_in_written_order() {
        // Order is the assertion. A `for` over a parsed record renders in this
        // order, so a map-backed parse would shuffle it between runs.
        let parsed = parse(r#"{"z": 1, "a": 2, "m": 3}"#).expect("valid JSON");
        let HostValue::Record(fields) = parsed else {
            panic!("expected a record, got {parsed:?}");
        };
        let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["z", "a", "m"]);
    }

    #[test]
    fn integral_numbers_stay_integers() {
        let parsed = parse(r#"{"status": 200, "temp": 21.5}"#).expect("valid JSON");
        assert_eq!(parsed.field("status"), Some(&HostValue::Int(200)));
        assert_eq!(parsed.field("temp"), Some(&HostValue::Float(21.5)));
    }

    #[test]
    fn null_arrays_and_nesting_all_have_a_form() {
        let parsed = parse(r#"{"a": null, "b": [1, "two", {"c": true}]}"#).expect("valid JSON");
        assert_eq!(parsed.field("a"), Some(&HostValue::Unit));
        let HostValue::List(items) = parsed.field("b").expect("b") else {
            panic!("expected a list");
        };
        assert_eq!(items[0], HostValue::Int(1));
        assert_eq!(items[1], HostValue::Str("two".into()));
        assert_eq!(items[2].field("c"), Some(&HostValue::Bool(true)));
    }

    #[test]
    fn a_value_round_trips_through_json_and_back() {
        let original = HostValue::Record(vec![
            ("id".into(), HostValue::Int(7)),
            ("ratio".into(), HostValue::Float(0.5)),
            (
                "tags".into(),
                HostValue::List(vec![HostValue::Str("a".into())]),
            ),
            ("missing".into(), HostValue::Unit),
        ]);
        let text = host_to_json(&original).to_string();
        assert_eq!(parse(&text).expect("round trip"), original);
    }

    #[test]
    fn a_number_too_large_for_i64_degrades_rather_than_failing_the_parse() {
        let parsed = parse(r#"{"big": 18446744073709551615}"#).expect("still valid JSON");
        assert!(matches!(parsed.field("big"), Some(HostValue::Float(_))));
    }

    #[test]
    fn malformed_json_reaches_the_err_arm_and_never_panics() {
        let result = call("parse", vec![HostValue::Str("{nope".into())]);
        let err = result.expect_err("malformed input must fail");
        assert_eq!(
            err.field("kind"),
            Some(&HostValue::Str("invalid_json".into()))
        );
    }

    #[test]
    fn stringify_is_the_inverse_of_parse() {
        let value = HostValue::Record(vec![("n".into(), HostValue::Int(1))]);
        let text = call("stringify", vec![value.clone()]).expect("stringify");
        let HostValue::Str(text) = text else {
            panic!("expected a string");
        };
        assert_eq!(call("parse", vec![HostValue::Str(text)]), Ok(value));
    }

    #[test]
    fn an_unknown_method_is_an_error_not_a_panic() {
        assert!(call("frobnicate", vec![]).is_err());
    }
}
