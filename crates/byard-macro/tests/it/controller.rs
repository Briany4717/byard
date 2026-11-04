//! Integration tests for `#[byard_controller]`. Proc-macro crates can only
//! exercise their macro from an external test target.
//!
//! The impl-form test controllers do sync work in `async fn`s (no real I/O), so
//! the `unused_async` lint is expected and allowed here.
#![allow(
    clippy::unused_async,
    clippy::unused_self,
    clippy::needless_pass_by_value
)]

use byard_macro::byard_controller;

#[byard_controller]
struct CounterController {
    count: i64,
    label: String,
    ratio: f64,
    enabled: bool,
    tries: u32,
}

#[test]
fn struct_is_emitted_unchanged() {
    // The original struct still exists and is constructible/usable.
    let c = CounterController {
        count: 3,
        label: "hi".to_string(),
        ratio: 0.5,
        enabled: true,
        tries: 2,
    };
    assert_eq!(c.count, 3);
    assert_eq!(c.label, "hi");
    assert!(c.enabled);
    assert_eq!(c.tries, 2);
    assert!((c.ratio - 0.5).abs() < f64::EPSILON);
}

#[test]
fn metadata_maps_fields_to_byld_types() {
    // The interpreter's `member → field type` inference (M5) consumes this.
    assert_eq!(CounterController::byard_field_type("count"), Some("Int"));
    assert_eq!(CounterController::byard_field_type("tries"), Some("Int"));
    assert_eq!(CounterController::byard_field_type("ratio"), Some("Float"));
    assert_eq!(CounterController::byard_field_type("label"), Some("Str"));
    assert_eq!(CounterController::byard_field_type("enabled"), Some("Bool"));
    assert_eq!(CounterController::byard_field_type("missing"), None);
}

#[test]
fn fields_are_in_declaration_order() {
    assert_eq!(
        CounterController::BYARD_FIELDS,
        &[
            ("count", "Int"),
            ("label", "Str"),
            ("ratio", "Float"),
            ("enabled", "Bool"),
            ("tries", "Int"),
        ]
    );
}

#[byard_controller]
struct StrRefHolder<'a> {
    name: &'a str,
}

#[test]
fn handles_references_and_generics() {
    assert_eq!(StrRefHolder::byard_field_type("name"), Some("Str"));
    let h = StrRefHolder { name: "x" };
    assert_eq!(h.name, "x");
}

// ── The impl-block form: `impl Controller` dispatch (RFC-0028 §2) ────────
//
// These compile the emitted `impl Controller` against the real `byard` façade,
// confirming the generated `::byard::bridge::*` references resolve (a
// dev-dependency cycle byard-macro ↔ byard, permitted for dev-deps).

use byard::bridge::{Controller, HostValue};

#[byard_controller]
#[derive(Clone)]
struct MathController {
    base: i64,
    label: String,
}

#[byard_controller]
impl MathController {
    async fn add(&self, n: i64) -> Result<i64, ()> {
        Ok(self.base + n)
    }

    async fn greet(&self, who: String) -> Result<String, String> {
        if who.is_empty() {
            Err("empty name".to_string())
        } else {
            Ok(format!("{}: {who}", self.label))
        }
    }
}

#[test]
fn impl_form_dispatches_async_methods_by_name() {
    let c = MathController {
        base: 40,
        label: "hi".to_string(),
    };
    assert_eq!(c.type_name(), "MathController");

    let ok = pollster::block_on(c.invoke("add", vec![HostValue::Int(2)]));
    assert_eq!(ok, Ok(HostValue::Int(42)));

    let greet = pollster::block_on(c.invoke("greet", vec![HostValue::Str("Ada".into())]));
    assert_eq!(greet, Ok(HostValue::Str("hi: Ada".into())));

    // A method's `Err` maps through the `err` arm to an error `HostValue`.
    let err = pollster::block_on(c.invoke("greet", vec![HostValue::Str(String::new())]));
    assert_eq!(err, Err(HostValue::Str("empty name".into())));
}

#[test]
fn impl_form_unknown_method_is_an_error_reply_not_a_panic() {
    let c = MathController {
        base: 0,
        label: String::new(),
    };
    let r = pollster::block_on(c.invoke("frobnicate", vec![]));
    assert!(matches!(r, Err(HostValue::Str(_))));
}

// ── Synchronous methods ──────────────────────────────────────────────────────
//
// A method that is not `async` used to be skipped by the macro, and a view
// that called it got "unknown controller method" at run time.

#[derive(Debug, Clone, PartialEq, byard::HostValue)]
struct Pair {
    left: i64,
    right: i64,
}

#[byard_controller]
#[derive(Clone)]
struct Calculator {
    base: i64,
}

impl Calculator {
    /// An ordinary constructor, in a plain block.
    fn new(base: i64) -> Self {
        Self { base }
    }
}

#[byard_controller]
impl Calculator {
    fn plus(&self, n: i64) -> i64 {
        self.base + n
    }

    fn halves(&self, n: i64) -> Result<Pair, String> {
        if n % 2 == 0 {
            Ok(Pair {
                left: n / 2,
                right: n / 2,
            })
        } else {
            Err(format!("{n} is odd"))
        }
    }

    fn swap(&self, pair: Pair) -> Pair {
        Pair {
            left: pair.right,
            right: pair.left,
        }
    }

    async fn later(&self, n: i64) -> i64 {
        self.base * n
    }

    /// A function that is not a method: left alone, still callable from Rust.
    fn doubled(self) -> Self {
        Self {
            base: self.base * 2,
        }
    }
}

#[test]
fn sync_methods_dispatch_beside_async_ones() {
    let c = Calculator::new(40);
    let reply = |method: &str, args: Vec<HostValue>| pollster::block_on(c.invoke(method, args));
    assert_eq!(
        reply("plus", vec![HostValue::Int(2)]),
        Ok(HostValue::Int(42))
    );
    assert_eq!(
        reply("later", vec![HostValue::Int(2)]),
        Ok(HostValue::Int(80))
    );
    assert_eq!(
        reply("halves", vec![HostValue::Int(8)]),
        Ok(HostValue::Record(vec![
            ("left".into(), HostValue::Int(4)),
            ("right".into(), HostValue::Int(4)),
        ]))
    );
    assert_eq!(
        reply("halves", vec![HostValue::Int(7)]),
        Err(HostValue::Str("7 is odd".into()))
    );
}

#[test]
fn a_sync_method_takes_and_returns_a_struct() {
    let c = Calculator::new(0);
    let sent = HostValue::Record(vec![
        ("left".into(), HostValue::Int(1)),
        ("right".into(), HostValue::Int(2)),
    ]);
    let back = pollster::block_on(c.invoke("swap", vec![sent])).unwrap();
    assert_eq!(back.field("left"), Some(&HostValue::Int(2)));
    assert_eq!(back.field("right"), Some(&HostValue::Int(1)));
}

#[test]
fn functions_that_are_not_methods_are_not_exposed() {
    let c = Calculator::new(1);
    for name in ["new", "doubled"] {
        let r = pollster::block_on(c.invoke(name, vec![]));
        assert!(matches!(r, Err(HostValue::Str(_))), "{name}: {r:?}");
    }
    assert_eq!(c.clone().doubled().base, 2);
}
