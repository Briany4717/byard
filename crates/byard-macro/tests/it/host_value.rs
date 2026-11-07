//! `#[derive(FromHostValue, IntoHostValue, HostValue)]`: compiled against the
//! real façade, and run. The whole file is `deny(warnings)` so a derive that
//! makes the person's crate warn (an unused `mut`, an unused variable) fails
//! here and not in their build.
#![deny(warnings)]

use byard::bridge::{FromHostValue, HostValue, IntoHostValue};

fn record(fields: &[(&str, HostValue)]) -> HostValue {
    HostValue::Record(
        fields
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect(),
    )
}

#[derive(Debug, Clone, PartialEq, byard::FromHostValue, byard::IntoHostValue)]
struct Address {
    street: String,
    zip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, byard::HostValue)]
struct Customer {
    name: String,
    age: i64,
    ratio: f64,
    active: bool,
    address: Address,
    tags: Vec<String>,
    previous: Vec<Address>,
    #[host(rename = "isVip")]
    vip: bool,
    r#type: String,
}

fn ada() -> Customer {
    Customer {
        name: "Ada".into(),
        age: 36,
        ratio: 0.5,
        active: true,
        address: Address {
            street: "Main St".into(),
            zip: Some("02134".into()),
        },
        tags: vec!["a".into(), "b".into()],
        previous: vec![Address {
            street: "Old Rd".into(),
            zip: None,
        }],
        vip: true,
        r#type: "person".into(),
    }
}

#[test]
fn a_struct_becomes_a_record_in_declaration_order_and_back() {
    let host = ada().into_host();
    let HostValue::Record(fields) = &host else {
        panic!("a struct is a record: {host:?}");
    };
    let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "name", "age", "ratio", "active", "address", "tags", "previous", "isVip", "type"
        ]
    );
    assert_eq!(host.field("isVip"), Some(&HostValue::Bool(true)));
    assert_eq!(
        host.field("address").and_then(|a| a.field("zip")),
        Some(&HostValue::Str("02134".into()))
    );
    // An absent optional is `Unit`, which is how a view reads "nothing".
    let HostValue::List(previous) = host.field("previous").unwrap() else {
        panic!("a list");
    };
    assert_eq!(previous[0].field("zip"), Some(&HostValue::Unit));

    assert_eq!(Customer::from_host(host), ada());
}

#[test]
fn field_order_in_the_record_does_not_matter() {
    let address = record(&[
        ("zip", HostValue::Str("1".into())),
        ("street", HostValue::Str("S".into())),
    ]);
    assert_eq!(
        Address::from_host(address),
        Address {
            street: "S".into(),
            zip: Some("1".into())
        }
    );
}

#[test]
fn a_missing_field_is_what_its_type_makes_of_nothing() {
    let customer = Customer::from_host(record(&[("name", HostValue::Str("Grace".into()))]));
    assert_eq!(customer.name, "Grace");
    assert_eq!(customer.age, 0);
    assert!(!customer.active);
    assert_eq!(customer.r#type, "", "not the text \"Unit\"");
    assert_eq!(customer.tags, Vec::<String>::new());
    assert_eq!(
        customer.address,
        Address {
            street: String::new(),
            zip: None
        }
    );
}

#[test]
fn a_value_that_is_not_a_record_never_panics() {
    for value in [
        HostValue::Unit,
        HostValue::Int(3),
        HostValue::Str("x".into()),
        HostValue::List(vec![HostValue::Int(1)]),
        HostValue::Bytes(vec![1, 2]),
    ] {
        assert_eq!(Address::from_host(value).zip, None);
    }
}

#[test]
fn a_field_of_the_wrong_shape_gets_the_lenient_fallback() {
    let customer = Customer::from_host(record(&[
        ("age", HostValue::Str("old".into())),
        ("tags", HostValue::Int(1)),
        ("address", HostValue::Int(5)),
    ]));
    assert_eq!(customer.age, 0);
    assert_eq!(customer.tags, Vec::<String>::new());
    assert_eq!(customer.address.street, "");
}

#[test]
fn an_unknown_field_is_ignored_and_the_first_of_a_duplicate_wins() {
    let address = record(&[
        ("street", HostValue::Str("first".into())),
        ("extra", HostValue::Int(1)),
        ("street", HostValue::Str("second".into())),
    ]);
    assert_eq!(Address::from_host(address).street, "first");
}

#[test]
fn an_int_reads_as_a_float_as_it_does_by_hand() {
    #[derive(byard::FromHostValue)]
    struct Point {
        x: f64,
    }
    assert!((Point::from_host(record(&[("x", HostValue::Int(2))])).x - 2.0).abs() < f64::EPSILON);
}

#[derive(Debug, PartialEq, byard::HostValue)]
struct Page<T> {
    items: Vec<T>,
    next: Option<T>,
}

#[test]
fn a_generic_struct_converts_when_its_parameter_does() {
    let page = Page {
        items: vec![Address {
            street: "S".into(),
            zip: None,
        }],
        next: None,
    };
    let host = page.into_host();
    let back = Page::<Address>::from_host(host);
    assert_eq!(back.items.len(), 1);
    assert_eq!(back.next, None);
    assert_eq!(
        Page::<i64>::from_host(record(&[(
            "items",
            HostValue::List(vec![HostValue::Int(1), HostValue::Int(2)])
        )])),
        Page {
            items: vec![1, 2],
            next: None
        }
    );
}

#[derive(Debug, PartialEq, byard::HostValue)]
struct Nothing {}

#[test]
fn a_struct_with_no_fields_is_the_empty_record() {
    assert_eq!(Nothing {}.into_host(), HostValue::Record(Vec::new()));
    assert_eq!(Nothing::from_host(HostValue::Int(1)), Nothing {});
}

#[derive(Debug, PartialEq, byard::IntoHostValue)]
struct Borrowing<'a> {
    #[host(rename = "label")]
    text: &'a str,
}

#[test]
fn a_struct_that_borrows_can_be_sent() {
    let host = Borrowing { text: "hi" }.into_host();
    assert_eq!(host.field("label"), Some(&HostValue::Str("hi".into())));
}

// ── Field names: data keeps the name it was given ───────────────────────────

#[derive(Debug, PartialEq, Eq, byard_macro::HostValue)]
struct Line {
    unit_price: i64,
    sku: String,
    #[host(rename = "onHand")]
    on_hand: i64,
}

#[test]
fn a_field_is_a_record_key_exactly_as_declared() {
    let line = Line {
        unit_price: 5,
        sku: "A".into(),
        on_hand: 2,
    };
    let HostValue::Record(fields) = line.into_host() else {
        panic!("a struct is a record")
    };
    let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
    // `unit_price` stays `unit_price`, so a struct that is also JSON reads the
    // same in Byld; an explicit rename is used as written.
    assert_eq!(keys, ["unit_price", "sku", "onHand"]);

    let back = Line::from_host(record(&[
        ("unit_price", HostValue::Int(9)),
        ("sku", HostValue::Str("B".into())),
        ("onHand", HostValue::Int(1)),
    ]));
    assert_eq!(back.unit_price, 9);
    assert_eq!(back.on_hand, 1);
}
