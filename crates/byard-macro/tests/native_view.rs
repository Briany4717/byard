//! Integration tests for `#[byard::native_view]` (RFC-0039). A proc macro can
//! only be exercised from outside its own crate, so the expansion is checked
//! here, against the real façade the generated code names.
//!
//! The declared-but-unread fields are the point of several of these tests (an
//! `#[event]` slot, a field that is deliberately *not* a prop), so the
//! dead-code lint is expected in this target.
#![allow(dead_code)]

use byard::bridge::HostValue;
use byard::render::{Layout, NativePropType, NativeProps, NativeView, NativeViewMeta, RenderCtx};

/// A widget declaring one of every prop shape the catalog knows.
#[byard::native_view(name = "Chart")]
#[derive(Default)]
struct Chart {
    #[prop]
    data: Vec<f32>,
    #[prop]
    stroke: u32,
    #[prop]
    title: String,
    #[prop]
    origin: (f32, f32),
    #[prop]
    smooth: bool,
    /// A prop that can move geometry says so, and the compiler files it as a
    /// layout attribute.
    #[prop(layout)]
    bars: i32,
    #[event]
    point_hover: (),
    /// Not a prop: state the widget keeps for itself, which the macro must
    /// leave entirely alone.
    hovered: Option<usize>,
}

impl NativeView for Chart {
    fn render(&mut self, _layout: Layout, _cx: &mut RenderCtx<'_>) {}
}

/// A widget with no props at all, the case where the generated `set_prop` has
/// nothing to match on.
#[byard::native_view]
#[derive(Default)]
struct Divider {
    thickness: f32,
}

impl NativeView for Divider {
    fn render(&mut self, _layout: Layout, _cx: &mut RenderCtx<'_>) {}
}

#[test]
fn the_struct_comes_out_the_other_side_unchanged() {
    let chart = Chart {
        data: vec![1.0, 2.0],
        stroke: 0x00FF_0000,
        title: "temps".to_string(),
        origin: (1.0, 2.0),
        smooth: true,
        bars: 7,
        point_hover: (),
        hovered: Some(3),
    };
    assert_eq!(chart.data, vec![1.0, 2.0]);
    assert_eq!(chart.hovered, Some(3));
    assert_eq!(chart.bars, 7);
}

#[test]
fn the_catalog_entry_is_read_off_the_declaration() {
    let info = <Chart as NativeViewMeta>::INFO;
    assert_eq!(info.name, "Chart");

    let names: Vec<&str> = info.props.iter().map(|p| p.name).collect();
    assert_eq!(
        names,
        vec!["data", "stroke", "title", "origin", "smooth", "bars"],
        "every `#[prop]`, in declaration order, and nothing else"
    );

    let types: Vec<NativePropType> = info.props.iter().map(|p| p.ty).collect();
    assert_eq!(
        types,
        vec![
            NativePropType::Floats,
            NativePropType::Color,
            NativePropType::Str,
            NativePropType::Vec2,
            NativePropType::Bool,
            NativePropType::Int,
        ]
    );

    let layout: Vec<bool> = info.props.iter().map(|p| p.layout).collect();
    assert_eq!(
        layout,
        vec![false, false, false, false, false, true],
        "only `#[prop(layout)]` reaches layout"
    );

    assert_eq!(info.events, &["point_hover"]);
}

#[test]
fn a_view_with_no_props_declares_none() {
    let info = <Divider as NativeViewMeta>::INFO;
    assert_eq!(info.name, "Divider", "the name defaults to the type's");
    assert!(info.props.is_empty());
    assert!(info.events.is_empty());
}

#[test]
fn set_prop_assigns_the_field_the_name_belongs_to() {
    let mut chart = Chart::default();
    chart.set_prop("data", &HostValue::List(vec![HostValue::Float(3.5)]));
    chart.set_prop("stroke", &HostValue::Int(0x005B_8DEF));
    chart.set_prop("title", &HostValue::Str("temps".into()));
    chart.set_prop(
        "origin",
        &HostValue::List(vec![HostValue::Float(1.0), HostValue::Float(2.0)]),
    );
    chart.set_prop("smooth", &HostValue::Bool(true));
    chart.set_prop("bars", &HostValue::Int(12));

    assert_eq!(chart.data, vec![3.5]);
    assert_eq!(chart.stroke, 0x005B_8DEF);
    assert_eq!(chart.title, "temps");
    assert!((chart.origin.0 - 1.0).abs() < f32::EPSILON);
    assert!(chart.smooth);
    assert_eq!(chart.bars, 12);
}

#[test]
fn a_name_nobody_declared_changes_nothing() {
    // The compiler rejects an unknown prop with a span long before this, so
    // reaching here at all means something else went wrong; quietly leaving
    // every field alone is the only response that cannot make it worse.
    let mut chart = Chart::default();
    chart.set_prop("colour", &HostValue::Int(1));
    assert_eq!(chart.stroke, 0);
    assert!(chart.title.is_empty());
}

#[test]
fn create_makes_an_instance_the_engine_can_hold() {
    let view = <Chart as NativeViewMeta>::create();
    // Box<dyn NativeView>: the engine's own handle on a package's widget.
    let _: Box<dyn NativeView> = view;
}
