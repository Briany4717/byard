use super::*;
use crate::ByardError;

/// Acceptance criterion: Atlas computes a valid layout for a single
/// rectangle with a child.
#[test]
fn computes_layout_for_container_with_one_child() {
    let mut atlas = LayoutAtlas::new();

    let child = atlas
        .add_leaf(LeafSize::new(100.0, 50.0))
        .expect("add_leaf");
    let root = atlas
        .add_container(
            ContainerStyle {
                width: Some(200.0),
                height: Some(100.0),
                ..Default::default()
            },
            &[child],
        )
        .expect("add_container");
    atlas.set_root(root).unwrap();

    atlas.compute(Viewport::new(800.0, 600.0)).expect("compute");

    let root_rect = atlas.resolved_rect(root).unwrap().expect("root rect");
    assert_f32_eq(root_rect.width, 200.0);
    assert_f32_eq(root_rect.height, 100.0);

    let child_rect = atlas.resolved_rect(child).unwrap().expect("child rect");
    assert_f32_eq(child_rect.width, 100.0);
    assert_f32_eq(child_rect.height, 50.0);
}

#[test]
fn empty_atlas_has_no_nodes() {
    let atlas = LayoutAtlas::new();
    assert_eq!(atlas.node_count(), 0);
    assert!(atlas.root().is_none());
}

#[test]
fn clear_resets_to_building_and_allows_rebuild() {
    let mut atlas = LayoutAtlas::new();
    let child = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    let root = atlas
        .add_container(ContainerStyle::default(), &[child])
        .unwrap();
    atlas.set_root(root).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    atlas.clear();

    assert_eq!(atlas.node_count(), 0);
    assert!(atlas.root().is_none());
    // After clear, we should be able to build again without panic.
    let _ = atlas.add_leaf(LeafSize::new(5.0, 5.0)).unwrap();
}

#[test]
#[should_panic(expected = "called while in Computed state")]
fn add_leaf_after_compute_panics() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    // This must panic.
    let _ = atlas.add_leaf(LeafSize::new(20.0, 20.0));
}

#[test]
#[should_panic(expected = "called while in Computed state")]
fn add_container_after_compute_panics() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    let _ = atlas.add_container(ContainerStyle::default(), &[leaf]);
}

#[test]
#[should_panic(expected = "called before compute")]
fn resolved_rect_before_compute_panics() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();

    let _ = atlas.resolved_rect(leaf);
}

#[test]
#[should_panic(expected = "called without a root node")]
fn compute_without_root_panics() {
    let mut atlas = LayoutAtlas::new();
    let _ = atlas.compute(Viewport::new(100.0, 100.0));
}

#[test]
fn auto_sized_container_grows_to_fit_child() {
    let mut atlas = LayoutAtlas::new();
    let child = atlas.add_leaf(LeafSize::new(150.0, 75.0)).unwrap();
    let root = atlas
        .add_container(ContainerStyle::default(), &[child])
        .unwrap();
    atlas.set_root(root).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    let root_rect = atlas.resolved_rect(root).unwrap().unwrap();
    assert_f32_eq(root_rect.width, 150.0);
    assert_f32_eq(root_rect.height, 75.0);
}

#[test]
fn atlas_node_id_is_copy() {
    const fn assert_copy<T: Copy>() {}
    assert_copy::<AtlasNodeId>();
}

/// Asserts two `f32` values are equal within the layout precision tolerance.
///
/// Taffy produces deterministic exact values for simple layouts, but using
/// `assert_eq!` on `f32` triggers `clippy::float_cmp`. This helper makes
/// the tolerance explicit.
#[track_caller]
fn assert_f32_eq(actual: f32, expected: f32) {
    let diff = (actual - expected).abs();
    assert!(
        diff < 0.001,
        "expected {expected}, got {actual} (diff = {diff})",
    );
}

/// Acceptance criterion: resolved geometry is written into `RenderFrame`
/// without crossing subsystem boundaries directly.
///
/// The Atlas only touches `RenderFrame` via its public API. There is no
/// import of `encoder` or any other subsystem.
#[test]
fn populate_frame_writes_resolved_geometry() {
    use crate::frame::RenderFrame;

    let mut atlas = LayoutAtlas::new();
    let child = atlas.add_leaf(LeafSize::new(100.0, 50.0)).unwrap();
    let root = atlas
        .add_container(
            ContainerStyle {
                width: Some(200.0),
                height: Some(100.0),
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    atlas.set_root(root).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    let mut frame = RenderFrame::new();
    atlas.populate_frame(&mut frame, &[]);

    assert_eq!(frame.rects().len(), 2, "root + child");

    // Pre-order: root first, then child.
    assert_f32_eq(frame.rects()[0].width, 200.0);
    assert_f32_eq(frame.rects()[0].height, 100.0);
    assert_f32_eq(frame.rects()[1].width, 100.0);
    assert_f32_eq(frame.rects()[1].height, 50.0);

    // No dirty targets were passed in, so nothing is marked dirty.
    assert_eq!(frame.dirty(), &[false, false]);
}

#[test]
fn populate_frame_appends_without_clearing() {
    use crate::frame::RenderFrame;

    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    let mut frame = RenderFrame::new();
    atlas.populate_frame(&mut frame, &[]);
    atlas.populate_frame(&mut frame, &[]);

    assert_eq!(
        frame.rects().len(),
        2,
        "populate_frame appends; caller is responsible for clearing",
    );
}

#[test]
#[should_panic(expected = "called before compute")]
fn populate_frame_before_compute_panics() {
    use crate::frame::RenderFrame;

    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();

    let mut frame = RenderFrame::new();
    atlas.populate_frame(&mut frame, &[]);
}

#[test]
#[should_panic(expected = "called while in Computed state")]
fn set_root_after_compute_panics() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    atlas.set_root(leaf).unwrap();
}

#[test]
fn orphan_node_returns_zero_rect() {
    let mut atlas = LayoutAtlas::new();
    let root = atlas.add_leaf(LeafSize::new(50.0, 50.0)).unwrap();
    let orphan = atlas.add_leaf(LeafSize::new(999.0, 999.0)).unwrap();
    atlas.set_root(root).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    let orphan_rect = atlas.resolved_rect(orphan).unwrap().unwrap();
    assert_f32_eq(orphan_rect.width, 0.0);
    assert_f32_eq(orphan_rect.height, 0.0);
}

#[test]
fn flex_row_layout_positions_children_at_known_offsets() {
    // Pixel-perfect layout contract for Phase 1.
    //
    // Taffy 0.10 with `Style::default()` uses Display::Flex and
    // FlexDirection::Row. A container of 200x200 with two 50x50 children
    // lays them out left-to-right at y=0:
    //
    //   child A → (x=0,  y=0, w=50, h=50)
    //   child B → (x=50, y=0, w=50, h=50)
    //
    // This validates the location field is correctly threaded from
    // taffy::Layout into our frame::Rect.
    let mut atlas = LayoutAtlas::new();

    let a = atlas.add_leaf(LeafSize::new(50.0, 50.0)).unwrap();
    let b = atlas.add_leaf(LeafSize::new(50.0, 50.0)).unwrap();
    let root = atlas
        .add_container(
            ContainerStyle {
                width: Some(200.0),
                height: Some(200.0),
                ..Default::default()
            },
            &[a, b],
        )
        .unwrap();
    atlas.set_root(root).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    let a_rect = atlas.resolved_rect(a).unwrap().unwrap();
    let b_rect = atlas.resolved_rect(b).unwrap().unwrap();

    // Child A: top-left corner of the container.
    assert_f32_eq(a_rect.x, 0.0);
    assert_f32_eq(a_rect.y, 0.0);
    assert_f32_eq(a_rect.width, 50.0);
    assert_f32_eq(a_rect.height, 50.0);

    // Child B: stacked immediately to the right of A on the main axis.
    assert_f32_eq(b_rect.x, 50.0);
    assert_f32_eq(b_rect.y, 0.0);
    assert_f32_eq(b_rect.width, 50.0);
    assert_f32_eq(b_rect.height, 50.0);
}

#[test]
fn grid_two_columns_positions_children_side_by_side() {
    // RFC-0018: a 200×100 grid with two `1fr` columns (100px each) and two
    // auto-placed children lays them one per column, A at x=0, B at x=100.
    let mut atlas = LayoutAtlas::new();
    let a = atlas.add_leaf(LeafSize::new(0.0, 40.0)).unwrap();
    let b = atlas.add_leaf(LeafSize::new(0.0, 40.0)).unwrap();
    let grid = atlas
        .add_grid_container(
            ContainerStyle::new(Some(200.0), Some(100.0)),
            &[GridTrack::Fr(1.0), GridTrack::Fr(1.0)],
            &[],
            0.0,
            0.0,
            &[a, b],
        )
        .unwrap();
    atlas.set_root(grid).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    let a_rect = atlas.resolved_rect(a).unwrap().unwrap();
    let b_rect = atlas.resolved_rect(b).unwrap().unwrap();
    let mut xs = [a_rect.x, b_rect.x];
    xs.sort_by(f32::total_cmp);
    assert_f32_eq(xs[0], 0.0);
    assert_f32_eq(xs[1], 100.0);
}

#[test]
fn grid_explicit_placement_pins_child_to_second_column() {
    // RFC-0018: `set_grid_item` with `col_start = 2` pins the child to the
    // second 1fr column (x = 100) even though it is the only/first child.
    let mut atlas = LayoutAtlas::new();
    let a = atlas.add_leaf(LeafSize::new(0.0, 40.0)).unwrap();
    let grid = atlas
        .add_grid_container(
            ContainerStyle::new(Some(200.0), Some(100.0)),
            &[GridTrack::Fr(1.0), GridTrack::Fr(1.0)],
            &[],
            0.0,
            0.0,
            &[a],
        )
        .unwrap();
    atlas
        .set_grid_item(
            a,
            GridItemPlacement {
                col_start: Some(2),
                col_span: 1,
                row_start: None,
                row_span: 1,
            },
        )
        .unwrap();
    atlas.set_root(grid).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    let a_rect = atlas.resolved_rect(a).unwrap().unwrap();
    assert_f32_eq(a_rect.x, 100.0);
}

#[test]
fn zstack_overlaps_children_and_sizes_to_largest() {
    // RFC-0018: a ZStack sizes to its largest child (100×100) and centres a
    // smaller child (20×20) within it (offset 40,40); both share the same
    // stacking cell, so they overlap.
    let mut atlas = LayoutAtlas::new();
    let big = atlas.add_leaf(LeafSize::new(100.0, 100.0)).unwrap();
    let small = atlas.add_leaf(LeafSize::new(20.0, 20.0)).unwrap();
    let stack = atlas
        .add_stack_container(ContainerStyle::default(), StackAlign::Center, &[big, small])
        .unwrap();
    atlas.set_root(stack).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    let s_rect = atlas.resolved_rect(stack).unwrap().unwrap();
    assert_f32_eq(s_rect.width, 100.0);
    assert_f32_eq(s_rect.height, 100.0);

    let big_rect = atlas.resolved_rect(big).unwrap().unwrap();
    let small_rect = atlas.resolved_rect(small).unwrap().unwrap();
    assert_f32_eq(big_rect.x, 0.0);
    assert_f32_eq(big_rect.y, 0.0);
    assert_f32_eq(small_rect.x, 40.0);
    assert_f32_eq(small_rect.y, 40.0);
    // The small child sits fully inside the big one, they overlap.
    assert!(small_rect.x >= big_rect.x && small_rect.y >= big_rect.y);
}

#[test]
fn zstack_alignment_pins_a_small_child_to_a_corner() {
    // `TopEnd` puts the small child at the top-right of the stack.
    let mut atlas = LayoutAtlas::new();
    let big = atlas.add_leaf(LeafSize::new(100.0, 100.0)).unwrap();
    let small = atlas.add_leaf(LeafSize::new(20.0, 20.0)).unwrap();
    let stack = atlas
        .add_stack_container(ContainerStyle::default(), StackAlign::TopEnd, &[big, small])
        .unwrap();
    atlas.set_root(stack).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    let small_rect = atlas.resolved_rect(small).unwrap().unwrap();
    assert_f32_eq(small_rect.x, 80.0); // right edge: 100 − 20
    assert_f32_eq(small_rect.y, 0.0); // top
}

/// A deterministic stand-in for the real glyph shaper: width is
/// `chars × font_size/2`, wrapped to `max_width` by stacking whole "lines".
struct StubSizer;
impl crate::text::TextSizer for StubSizer {
    fn measure(&mut self, text: &str, font_size: f32, max_width: Option<f32>) -> (f32, f32) {
        let line_h = font_size * 1.2;
        #[allow(clippy::cast_precision_loss)]
        let natural = text.chars().count() as f32 * font_size * 0.5;
        match max_width {
            Some(w) if w > 0.0 && natural > w => (w, (natural / w).ceil() * line_h),
            _ => (natural, line_h),
        }
    }
}

#[test]
fn text_leaf_wraps_to_parent_width() {
    // RFC-0005 default wrap: a text leaf with no explicit width reflows to the
    // width its (100px) parent offers, growing taller instead of overflowing.
    let mut atlas = LayoutAtlas::new();
    let text = atlas
        .add_text_leaf(TextLeaf {
            content: "abcdefghijklmnopqrst".to_string(), // 20 chars → ~200px natural
            font_size: 20.0,
            width: None,
            fallback: (200.0, 24.0),
        })
        .unwrap();
    // A **column** (cross axis = width): align-items stretch constrains the
    // text to the column width, so it wraps. (A row would leave width as the
    // main axis and the text would take its natural width and overflow.)
    let col = atlas
        .add_container(
            ContainerStyle::new(Some(100.0), Some(200.0)).with_direction(FlexDir::Column),
            &[text],
        )
        .unwrap();
    atlas.set_root(col).unwrap();
    let mut sizer = StubSizer;
    atlas
        .compute_with_text(Viewport::new(800.0, 600.0), &mut sizer)
        .unwrap();

    let r = atlas.resolved_rect(text).unwrap().unwrap();
    assert!(
        r.width <= 100.5,
        "wrapped to the column width, got {}",
        r.width
    );
    assert!(
        r.height > 24.0,
        "wrapped onto multiple lines, got {}",
        r.height
    );
}

/// A sizer whose natural width is deliberately fractional, to exercise the
/// pixel-rounding path.
struct FractionalSizer(f32);
impl crate::text::TextSizer for FractionalSizer {
    fn measure(&mut self, _text: &str, font_size: f32, max_width: Option<f32>) -> (f32, f32) {
        let line_h = font_size * 1.2;
        match max_width {
            Some(w) if w > 0.0 && self.0 > w => (w, line_h * 2.0),
            _ => (self.0, line_h),
        }
    }
}

#[test]
fn fractional_text_width_reserves_a_whole_pixel_and_stays_one_line() {
    // Regression: a single-line text measured at a fractional width (100.4)
    // must resolve to an *integer* width ≥ that, so the render pass, which
    // re-shapes the run to the resolved width, never wraps a line that
    // actually fits. Taffy rounds resolved coordinates to whole pixels, so
    // without the ceil the node would resolve to 100 and the run would wrap.
    let mut atlas = LayoutAtlas::new();
    let text = atlas
        .add_text_leaf(TextLeaf {
            content: "one line".to_string(),
            font_size: 16.0,
            width: None,
            fallback: (100.4, 19.2),
        })
        .unwrap();
    // A **row** keeps width as the main axis, so the text takes its natural
    // (unconstrained) width rather than being stretched/wrapped.
    let row = atlas
        .add_container(
            ContainerStyle::new(Some(400.0), Some(80.0)).with_direction(FlexDir::Row),
            &[text],
        )
        .unwrap();
    atlas.set_root(row).unwrap();
    let mut sizer = FractionalSizer(100.4);
    atlas
        .compute_with_text(Viewport::new(800.0, 600.0), &mut sizer)
        .unwrap();

    let r = atlas.resolved_rect(text).unwrap().unwrap();
    assert!(
        r.width >= 100.4,
        "resolved width must cover the glyph extent (≥ 100.4), got {}",
        r.width
    );
    assert!(
        (r.width - r.width.round()).abs() < 1e-3,
        "resolved width must be a whole pixel, got {}",
        r.width
    );
}

#[test]
fn text_leaf_without_sizer_uses_fallback() {
    // `compute` (no sizer) falls back to the natural single-line size.
    let mut atlas = LayoutAtlas::new();
    let text = atlas
        .add_text_leaf(TextLeaf {
            content: "hello".to_string(),
            font_size: 16.0,
            width: None,
            fallback: (42.0, 19.0),
        })
        .unwrap();
    atlas.set_root(text).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();
    let r = atlas.resolved_rect(text).unwrap().unwrap();
    assert_f32_eq(r.width, 42.0);
    assert_f32_eq(r.height, 19.0);
}

#[test]
fn absolute_node_fills_containing_block_and_ignores_flow() {
    // RFC-0017: two absolute children pinned inset-0 both fill the viewport
    // and neither displaces the other (nor a flowing sibling).
    let mut atlas = LayoutAtlas::new();
    let flow = atlas.add_leaf(LeafSize::new(40.0, 40.0)).unwrap();
    let overlay_a = atlas
        .add_container(ContainerStyle::default().with_absolute(true), &[])
        .unwrap();
    let overlay_b = atlas
        .add_container(ContainerStyle::default().with_absolute(true), &[])
        .unwrap();
    let root = atlas
        .add_container(
            ContainerStyle::new(Some(300.0), Some(200.0)),
            &[flow, overlay_a, overlay_b],
        )
        .unwrap();
    atlas.set_root(root).unwrap();
    atlas.compute(Viewport::new(300.0, 200.0)).unwrap();

    // The flowing child keeps its natural size at the origin, absolute
    // siblings did not push it.
    let flow_rect = atlas.resolved_rect(flow).unwrap().unwrap();
    assert_f32_eq(flow_rect.x, 0.0);
    assert_f32_eq(flow_rect.width, 40.0);

    for ov in [overlay_a, overlay_b] {
        let r = atlas.resolved_rect(ov).unwrap().unwrap();
        assert_f32_eq(r.x, 0.0);
        assert_f32_eq(r.y, 0.0);
        assert_f32_eq(r.width, 300.0);
        assert_f32_eq(r.height, 200.0);
    }
}

#[test]
fn atlas_error_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AtlasError>();
    assert_send_sync::<ByardError>();
}

#[test]
fn mark_dirty_filters_foreign_kinds() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    let foreign_kind: u16 = 999;
    let target = TargetId::new(0, atlas.current_generation(), foreign_kind);

    // Should not panic, should not affect anything.
    atlas.mark_dirty_all(&[target]);

    // Recompute must still succeed (no spurious dirty propagation).
    atlas.recompute_dirty(Viewport::new(100.0, 100.0)).unwrap();
}

#[test]
fn mark_dirty_filters_stale_generation() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    let stale_generation = atlas.current_generation().wrapping_sub(1);
    let target = TargetId::new(0, stale_generation, TargetKind::AtlasNode as u16);

    atlas.mark_dirty_all(&[target]);
    atlas.recompute_dirty(Viewport::new(100.0, 100.0)).unwrap();
}

#[test]
fn mark_dirty_accepts_matching_kind_and_generation() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    let target = TargetId::new(0, atlas.current_generation(), TargetKind::AtlasNode as u16);
    atlas.mark_dirty_all(&[target]);

    atlas.recompute_dirty(Viewport::new(200.0, 200.0)).unwrap();

    // Re-fetched rect reflects the new viewport. Container with no
    // explicit width takes viewport-driven size only if it has flex_grow
    //, here it's a leaf with fixed size, so the size stays at 10x10.
    let rect = atlas.resolved_rect(leaf).unwrap().unwrap();
    assert_f32_eq(rect.width, 10.0);
    assert_f32_eq(rect.height, 10.0);
}

#[test]
fn clear_invalidates_previous_generation_targets() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    let old_target = TargetId::new(0, atlas.current_generation(), TargetKind::AtlasNode as u16);

    atlas.clear();
    let new_leaf = atlas.add_leaf(LeafSize::new(20.0, 20.0)).unwrap();
    atlas.set_root(new_leaf).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    // Old target points at index 0, but its generation no longer
    // matches, must be silently ignored.
    atlas.mark_dirty_all(&[old_target]);
    atlas.recompute_dirty(Viewport::new(100.0, 100.0)).unwrap();
}

#[test]
#[should_panic(expected = "called before compute")]
fn recompute_dirty_before_compute_panics() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();

    let _ = atlas.recompute_dirty(Viewport::new(100.0, 100.0));
}

#[test]
#[should_panic(expected = "called before compute")]
fn mark_dirty_before_compute_panics() {
    let mut atlas = LayoutAtlas::new();
    let _ = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();

    atlas.mark_dirty_all(&[]);
}

#[test]
fn next_target_index_returns_consecutive_values() {
    let mut atlas = LayoutAtlas::new();
    assert_eq!(atlas.next_target_index(), 0);
    let _ = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    assert_eq!(atlas.next_target_index(), 1);
    let _ = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    assert_eq!(atlas.next_target_index(), 2);
}

#[test]
fn current_generation_increments_on_clear() {
    let mut atlas = LayoutAtlas::new();
    assert_eq!(atlas.current_generation(), 0);
    atlas.clear();
    assert_eq!(atlas.current_generation(), 1);
    atlas.clear();
    assert_eq!(atlas.current_generation(), 2);
}

/// Acceptance criterion: a signal that mutates one leaf produces
/// exactly one `TargetId` in the tick, which the atlas processes as
/// exactly one `mark_dirty` call.
///
/// This is the end-to-end validation of the Evaluator → Atlas flow
/// described in RFC-0001 §2.2 and §4.1.
#[test]
fn signal_mutation_propagates_to_atlas_via_target_id() {
    use crate::evaluator::{EvaluatorTick, Signal, ViewArena};

    // ── Setup the Atlas ──────────────────────────────────────────────
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(50.0, 50.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(200.0, 200.0)).unwrap();

    // The leaf is registered as TargetId index 0 in the atlas.
    let leaf_target = TargetId::new(
        atlas.next_target_index().wrapping_sub(1),
        atlas.current_generation(),
        TargetKind::AtlasNode as u16,
    );

    // ── Setup an Evaluator signal subscribed to the leaf ─────────────
    let arena = ViewArena::new();
    let signal = Signal::new_in(&arena, 0_u32);
    signal.subscribe(leaf_target);

    let mut tick = EvaluatorTick::new();
    tick.register(signal);

    // First tick: no writes, no dirty targets.
    let dirty = tick.collect_dirty();
    assert!(
        dirty.is_empty(),
        "no writes should produce no dirty targets"
    );

    // ── Mutate the signal ────────────────────────────────────────────
    signal.write(|v| *v = 42);

    // The tick must collect exactly one TargetId pointing at our leaf.
    let dirty = tick.collect_dirty();
    assert_eq!(dirty.len(), 1, "one mutation → one dirty target");
    assert_eq!(dirty[0], leaf_target);

    // ── Atlas processes the dirty set ────────────────────────────────
    atlas.mark_dirty_all(&dirty);

    // Recompute completes successfully (Taffy has the leaf marked dirty
    // and re-runs layout for the affected subtree).
    atlas.recompute_dirty(Viewport::new(200.0, 200.0)).unwrap();

    // Geometry is still queryable post-recompute.
    let rect = atlas.resolved_rect(leaf).unwrap().unwrap();
    assert_f32_eq(rect.width, 50.0);
    assert_f32_eq(rect.height, 50.0);
}

/// Acceptance criterion: a `Signal` mutation results in
/// only the affected entries being marked dirty in `RenderFrame`.
///
/// Builds a two-leaf tree, subscribes a signal to only one leaf, and
/// verifies that after mutating it and ticking, `populate_frame` marks
/// exactly that leaf's `RenderFrame` entry dirty, the sibling and the
/// root stay clean.
#[test]
fn evaluator_tick_marks_only_affected_render_frame_entries_dirty() {
    use crate::evaluator::{EvaluatorTick, Signal, ViewArena};
    use crate::frame::RenderFrame;

    let mut atlas = LayoutAtlas::new();
    let a = atlas.add_leaf(LeafSize::new(50.0, 50.0)).unwrap();
    let b = atlas.add_leaf(LeafSize::new(50.0, 50.0)).unwrap();
    let root = atlas
        .add_container(
            ContainerStyle {
                width: Some(200.0),
                height: Some(200.0),
                ..Default::default()
            },
            &[a, b],
        )
        .unwrap();
    atlas.set_root(root).unwrap();
    atlas.compute(Viewport::new(200.0, 200.0)).unwrap();

    // `b` was the second leaf added, so its TargetId index is 1.
    let b_target = TargetId::new(1, atlas.current_generation(), TargetKind::AtlasNode as u16);

    let arena = ViewArena::new();
    let signal = Signal::new_in(&arena, 0_u32);
    signal.subscribe(b_target);

    let mut tick = EvaluatorTick::new();
    tick.register(signal);

    // Mutate only the signal subscribed to `b`.
    signal.write(|v| *v = 1);
    let dirty_targets = tick.collect_dirty();
    assert_eq!(dirty_targets, vec![b_target]);

    let mut frame = RenderFrame::new();
    atlas.populate_frame(&mut frame, &dirty_targets);

    // Pre-order: root, then a, then b.
    assert_eq!(frame.rects().len(), 3, "root + a + b");
    assert_eq!(
        frame.dirty(),
        &[false, false, true],
        "only b's entry is dirty, root and a are untouched"
    );
}

#[test]
fn container_style_constructor_round_trips() {
    let s = ContainerStyle::new(Some(100.0), Some(200.0));
    assert_eq!(s.width, Some(100.0));
    assert_eq!(s.height, Some(200.0));
}

#[test]
fn hit_test_pure_success_and_miss() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(100.0, 100.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    // Hit within leaf
    assert_eq!(atlas.hit_test(50.0, 50.0), Some(leaf));

    // Miss (empty area)
    assert_eq!(atlas.hit_test(150.0, 150.0), None);
}

#[test]
fn hit_test_z_order_implicit() {
    let mut atlas = LayoutAtlas::new();
    // A child that overlaps with its parent.
    // Let's create a container of size 200x200, and a child of size 100x100.
    // Taffy flexbox layout will position the child at (0, 0) relative to the container.
    let child = atlas.add_leaf(LeafSize::new(100.0, 100.0)).unwrap();
    let parent = atlas
        .add_container(
            ContainerStyle {
                width: Some(200.0),
                height: Some(200.0),
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    atlas.set_root(parent).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    // The intersection is at (0, 0) to (100, 100).
    // Since child is traversed later (pre-order: parent then child),
    // query should return the child node.
    assert_eq!(atlas.hit_test(50.0, 50.0), Some(child));

    // Outside child, but inside parent
    assert_eq!(atlas.hit_test(150.0, 150.0), Some(parent));
}

#[test]
fn hit_test_negative_coordinates() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(100.0, 100.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    // Should return None safely without panic
    assert_eq!(atlas.hit_test(-50.0, -50.0), None);
}

#[test]
fn hit_test_invalidation_cycle() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(100.0, 100.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    // Verify hit-test works initially
    assert_eq!(atlas.hit_test(50.0, 50.0), Some(leaf));

    // Clear and construct a new view
    atlas.clear();

    let new_leaf = atlas.add_leaf(LeafSize::new(50.0, 50.0)).unwrap();
    atlas.set_root(new_leaf).unwrap();
    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    // The old node is no longer valid, and coordinates outside the new leaf (e.g. 75, 75)
    // should return None, even though they were inside the old leaf.
    assert_eq!(atlas.hit_test(75.0, 75.0), None);
    // Inside the new leaf, it should return new_leaf
    assert_eq!(atlas.hit_test(25.0, 25.0), Some(new_leaf));
}

#[test]
#[should_panic(expected = "called while in Building state")]
fn hit_test_in_building_state_panics() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(100.0, 100.0)).unwrap();
    atlas.set_root(leaf).unwrap();

    // This must panic because compute has not been called.
    let _ = atlas.hit_test(50.0, 50.0);
}

// --- Cross-atlas AtlasNodeId scoping ---------------------
//
// These tests exercise the actual hazard this closes off: an
// `AtlasNodeId` produced by one `LayoutAtlas` must never be silently
// accepted by a different instance. Every entry point that takes an
// `AtlasNodeId` from a caller must return `Err(AtlasError::ForeignNode)`
//, never panic, never silently produce wrong geometry.

#[test]
fn two_atlases_have_distinct_instance_ids() {
    let atlas_a = LayoutAtlas::new();
    let atlas_b = LayoutAtlas::new();
    assert_ne!(atlas_a.instance_id(), atlas_b.instance_id());
}

#[test]
fn set_root_rejects_foreign_node() {
    let mut atlas_a = LayoutAtlas::new();
    let foreign_leaf = atlas_a.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();

    let mut atlas_b = LayoutAtlas::new();
    let err = atlas_b.set_root(foreign_leaf).unwrap_err();

    match err {
        AtlasError::ForeignNode { expected, actual } => {
            assert_eq!(expected, atlas_b.instance_id());
            assert_eq!(actual, atlas_a.instance_id());
        }
        other => panic!("expected AtlasError::ForeignNode, got {other:?}"),
    }
}

#[test]
fn add_container_rejects_foreign_child() {
    let mut atlas_a = LayoutAtlas::new();
    let foreign_leaf = atlas_a.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();

    let mut atlas_b = LayoutAtlas::new();
    let local_leaf = atlas_b.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();

    // Mixing one local and one foreign child must still be rejected,
    // validation can't be satisfied by having at least one valid id.
    let err = atlas_b
        .add_container(
            ContainerStyle::new(Some(100.0), Some(100.0)),
            &[local_leaf, foreign_leaf],
        )
        .unwrap_err();

    match err {
        AtlasError::ForeignNode { expected, actual } => {
            assert_eq!(expected, atlas_b.instance_id());
            assert_eq!(actual, atlas_a.instance_id());
        }
        other => panic!("expected AtlasError::ForeignNode, got {other:?}"),
    }
}

#[test]
fn resolved_rect_rejects_foreign_node() {
    let mut atlas_a = LayoutAtlas::new();
    let leaf_a = atlas_a.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas_a.set_root(leaf_a).unwrap();
    atlas_a.compute(Viewport::new(800.0, 600.0)).unwrap();

    let mut atlas_b = LayoutAtlas::new();
    let leaf_b = atlas_b.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas_b.set_root(leaf_b).unwrap();
    atlas_b.compute(Viewport::new(800.0, 600.0)).unwrap();

    // atlas_b is Computed, so the state assertion passes, the
    // cross-atlas check must still catch the foreign id from atlas_a.
    let err = atlas_b.resolved_rect(leaf_a).unwrap_err();

    match err {
        AtlasError::ForeignNode { expected, actual } => {
            assert_eq!(expected, atlas_b.instance_id());
            assert_eq!(actual, atlas_a.instance_id());
        }
        other => panic!("expected AtlasError::ForeignNode, got {other:?}"),
    }
}

#[test]
fn foreign_node_error_bridges_to_byard_error() {
    let mut atlas_a = LayoutAtlas::new();
    let foreign_leaf = atlas_a.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();

    let mut atlas_b = LayoutAtlas::new();
    let atlas_err = atlas_b.set_root(foreign_leaf).unwrap_err();
    let byard_err: ByardError = atlas_err.into();

    assert!(byard_err.to_string().contains("AtlasNodeId belongs to"));
}

// --- Builder API -----------------------------------------
//
// These tests exercise the acceptance criteria: a
// multi-level tree expressed as a single chained expression, identical
// `AtlasNodeId`s to the equivalent imperative sequence, and that the
// low-level API (PR #14) is untouched by the addition.

/// Acceptance criterion: a 3-level hierarchy mixing leaves and
/// containers can be expressed as a single chained expression.
#[test]
fn builder_expresses_three_level_mixed_hierarchy() {
    use LayoutAtlasBuilder as B;

    let mut atlas = LayoutAtlas::new();

    let root = atlas
        .build_root(B::container(
            ContainerStyle::new(Some(300.0), Some(200.0)),
            [
                B::leaf(LeafSize::new(50.0, 50.0)),
                B::container(
                    ContainerStyle::default(),
                    [
                        B::leaf(LeafSize::new(20.0, 20.0)),
                        B::container(
                            ContainerStyle::default(),
                            [B::leaf(LeafSize::new(10.0, 10.0))],
                        ),
                    ],
                ),
            ],
        ))
        .unwrap();

    atlas.compute(Viewport::new(800.0, 600.0)).unwrap();

    // 1 outer container + 1 leaf + 1 inner container + 1 leaf +
    // 1 innermost container + 1 leaf = 6 nodes.
    assert_eq!(atlas.node_count(), 6);
    let root_rect = atlas.resolved_rect(root).unwrap().expect("root rect");
    assert_f32_eq(root_rect.width, 300.0);
    assert_f32_eq(root_rect.height, 200.0);
}

/// Acceptance criterion: the builder produces the same `AtlasNodeId`s
/// as the equivalent imperative sequence.
///
/// Each sequence runs on its own *fresh* atlas. We compare only the
/// Taffy-level `node_id` (not the full `AtlasNodeId`): `atlas_id` is
/// instance-specific by design, so two different atlases can never
/// produce an equal `AtlasNodeId`, regardless of how their trees were
/// built. A fresh Taffy tree's internal slot allocation depends only
/// on insertion order, there are no removals involved here to make
/// slot reuse a confounding factor, so identical `node_id`s on two
/// fresh atlases is exactly the signal that `build` issues
/// `add_leaf`/`add_container` calls in the same order the imperative
/// version does.
#[test]
fn build_produces_same_ids_as_imperative_sequence() {
    use LayoutAtlasBuilder as B;

    // Imperative sequence: children before parents, leaf before the
    // sibling container, matching the builder's depth-first order.
    let mut imperative = LayoutAtlas::new();
    let leaf_a = imperative.add_leaf(LeafSize::new(50.0, 50.0)).unwrap();
    let leaf_b = imperative.add_leaf(LeafSize::new(20.0, 20.0)).unwrap();
    let inner = imperative
        .add_container(ContainerStyle::default(), &[leaf_b])
        .unwrap();
    let root_imperative = imperative
        .add_container(
            ContainerStyle::new(Some(300.0), Some(200.0)),
            &[leaf_a, inner],
        )
        .unwrap();

    // Equivalent builder sequence, on a separate fresh atlas.
    let mut built = LayoutAtlas::new();
    let root_builder = built
        .build(B::container(
            ContainerStyle::new(Some(300.0), Some(200.0)),
            [
                B::leaf(LeafSize::new(50.0, 50.0)),
                B::container(
                    ContainerStyle::default(),
                    [B::leaf(LeafSize::new(20.0, 20.0))],
                ),
            ],
        ))
        .unwrap();

    assert_eq!(root_builder.node_id, root_imperative.node_id);
}

/// Acceptance criterion (paraphrased): the low-level API from PR #14
/// is unchanged, `add_leaf`/`add_container`/`set_root` still work
/// exactly as before, with no signature or behavior change introduced
/// by adding the builder.
#[test]
fn low_level_api_unchanged_alongside_builder() {
    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    let root = atlas
        .add_container(ContainerStyle::default(), &[leaf])
        .unwrap();
    atlas.set_root(root).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    let rect = atlas.resolved_rect(leaf).unwrap().unwrap();
    assert_f32_eq(rect.width, 10.0);
    assert_f32_eq(rect.height, 10.0);
}

/// `build` rejects nodes the same way `add_leaf`/`add_container` do
/// once the atlas has moved to `Computed`, the panic contract is
/// inherited, not bypassed by going through the builder.
#[test]
#[should_panic(expected = "called while in Computed state")]
fn build_after_compute_panics() {
    use LayoutAtlasBuilder as B;

    let mut atlas = LayoutAtlas::new();
    let leaf = atlas.add_leaf(LeafSize::new(10.0, 10.0)).unwrap();
    atlas.set_root(leaf).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    let _ = atlas.build(B::leaf(LeafSize::new(5.0, 5.0)));
}

/// A single `build_root` call replaces the imperative
/// add-then-set_root pair and leaves the atlas ready to compute.
#[test]
fn build_root_sets_root_and_allows_compute() {
    use LayoutAtlasBuilder as B;

    let mut atlas = LayoutAtlas::new();
    let root = atlas
        .build_root(B::leaf(LeafSize::new(42.0, 24.0)))
        .unwrap();

    assert_eq!(atlas.root(), Some(root));
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    let rect = atlas.resolved_rect(root).unwrap().unwrap();
    assert_f32_eq(rect.width, 42.0);
    assert_f32_eq(rect.height, 24.0);
}

/// Deeply-nested trees (beyond the 3-level acceptance criterion) build
/// correctly, verifies the recursive `build` walk doesn't have a
/// depth assumption baked in.
#[test]
fn builder_handles_deeply_nested_tree() {
    use LayoutAtlasBuilder as B;

    let mut atlas = LayoutAtlas::new();

    // 5 levels of single-child containers, terminating in a leaf.
    let spec = B::container(
        ContainerStyle::default(),
        [B::container(
            ContainerStyle::default(),
            [B::container(
                ContainerStyle::default(),
                [B::container(
                    ContainerStyle::default(),
                    [B::leaf(LeafSize::new(15.0, 15.0))],
                )],
            )],
        )],
    );

    let root = atlas.build_root(spec).unwrap();
    atlas.compute(Viewport::new(100.0, 100.0)).unwrap();

    // 4 containers + 1 leaf.
    assert_eq!(atlas.node_count(), 5);
    let root_rect = atlas.resolved_rect(root).unwrap().unwrap();
    assert_f32_eq(root_rect.width, 15.0);
    assert_f32_eq(root_rect.height, 15.0);
}

#[test]
fn column_direction_gap_and_padding_lay_children_vertically() {
    let mut atlas = LayoutAtlas::new();
    let a = atlas.add_leaf(LeafSize::new(40.0, 20.0)).unwrap();
    let b = atlas.add_leaf(LeafSize::new(40.0, 20.0)).unwrap();
    let col = atlas
        .add_container(
            ContainerStyle::new(Some(200.0), Some(200.0))
                .with_direction(FlexDir::Column)
                .with_gap(10.0)
                .with_padding(Spacing::all(8.0)),
            &[a, b],
        )
        .unwrap();
    atlas.set_root(col).unwrap();
    atlas.compute(Viewport::new(200.0, 200.0)).unwrap();

    let ra = atlas.resolved_rect(a).unwrap().unwrap();
    let rb = atlas.resolved_rect(b).unwrap().unwrap();
    // Padding offsets the first child; the gap separates the two.
    assert_f32_eq(ra.x, 8.0);
    assert_f32_eq(ra.y, 8.0);
    assert_f32_eq(rb.y, 8.0 + 20.0 + 10.0); // padding + first height + gap
}

#[test]
fn grow_distributes_main_axis_space() {
    let mut atlas = LayoutAtlas::new();
    let spacer = atlas
        .add_container(ContainerStyle::default().with_grow(1.0), &[])
        .unwrap();
    let fixed = atlas.add_leaf(LeafSize::new(40.0, 20.0)).unwrap();
    let row = atlas
        .add_container(
            ContainerStyle::new(Some(200.0), Some(50.0)).with_direction(FlexDir::Row),
            &[spacer, fixed],
        )
        .unwrap();
    atlas.set_root(row).unwrap();
    atlas.compute(Viewport::new(200.0, 50.0)).unwrap();

    let rs = atlas.resolved_rect(spacer).unwrap().unwrap();
    let rf = atlas.resolved_rect(fixed).unwrap().unwrap();
    // The grow:1 spacer eats the slack, pushing the fixed leaf to the end.
    assert_f32_eq(rs.width, 160.0); // 200 - 40 fixed
    assert_f32_eq(rf.x, 160.0);
}
