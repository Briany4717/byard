use super::*;

/// A two-leaf column, built in a fixed order so a retained rebuild can be
/// asked to reproduce it exactly.
fn build(atlas: &mut LayoutAtlas, first: LeafSize, second: LeafSize) {
    let a = atlas.add_leaf(first).unwrap();
    let b = atlas.add_leaf(second).unwrap();
    let root = atlas
        .add_container(ContainerStyle::new(Some(200.0), Some(200.0)), &[a, b])
        .unwrap();
    atlas.set_root(root).unwrap();
}

fn fresh(first: LeafSize, second: LeafSize) -> LayoutAtlas {
    let mut atlas = LayoutAtlas::new();
    build(&mut atlas, first, second);
    atlas.compute(Viewport::new(200.0, 200.0)).unwrap();
    atlas
}

#[test]
fn an_identical_retained_build_marks_nothing_dirty() {
    let mut atlas = fresh(LeafSize::new(40.0, 20.0), LeafSize::new(40.0, 20.0));
    let generation = atlas.current_generation();

    atlas.begin_retained_build();
    build(
        &mut atlas,
        LeafSize::new(40.0, 20.0),
        LeafSize::new(40.0, 20.0),
    );
    assert!(atlas.end_retained_build(), "the build must be reusable");

    assert!(
        atlas.layout_dirty_targets().is_empty(),
        "nothing changed, so nothing may be marked"
    );
    assert_eq!(
        atlas.current_generation(),
        generation,
        "the retained path must not bump the view generation, that is what \
             invalidates every outstanding TargetId and is why the dirty \
             channel was unusable while every frame cleared"
    );
}

#[test]
fn a_changed_leaf_size_marks_exactly_that_slot() {
    let mut atlas = fresh(LeafSize::new(40.0, 20.0), LeafSize::new(40.0, 20.0));

    atlas.begin_retained_build();
    build(
        &mut atlas,
        LeafSize::new(40.0, 20.0),
        LeafSize::new(40.0, 99.0),
    );
    assert!(atlas.end_retained_build());

    let marked: Vec<u32> = atlas
        .layout_dirty_targets()
        .iter()
        .map(|t| t.index())
        .collect();
    assert_eq!(marked, vec![1], "only the second leaf's inputs moved");
}

#[test]
fn ids_are_reused_rather_than_reassigned() {
    // `next_target_index()` is `nodes_by_index.len()`, so a retained build
    // that created nodes instead of reusing them would silently hand every
    // element a different id, and every outstanding `TargetId`, hit-test
    // index and router key would point at the wrong element.
    let mut atlas = fresh(LeafSize::new(40.0, 20.0), LeafSize::new(40.0, 20.0));
    let before: Vec<AtlasNodeId> = atlas.nodes_by_index.clone();

    atlas.begin_retained_build();
    build(
        &mut atlas,
        LeafSize::new(40.0, 20.0),
        LeafSize::new(60.0, 20.0),
    );
    assert!(atlas.end_retained_build());

    assert_eq!(before, atlas.nodes_by_index);
    assert_eq!(atlas.node_count(), 3, "no node was added");
}

#[test]
fn a_kind_mismatch_aborts_the_retained_build() {
    // Default-deny (RFC-0032 §R4): a slot that does not hold what the walk
    // expects fails the whole pass rather than restyling a leaf as if it
    // were a container.
    let mut atlas = fresh(LeafSize::new(40.0, 20.0), LeafSize::new(40.0, 20.0));

    atlas.begin_retained_build();
    // A *text* leaf where an ordinary leaf lives.
    let err = atlas.add_text_leaf(TextLeaf {
        content: "x".to_string(),
        font_size: 12.0,
        weight: 400,
        family: None,
        width: None,
        fallback: (10.0, 12.0),
    });
    assert!(matches!(
        err,
        Err(AtlasError::RetainedSlotMismatch { index: 0 })
    ));
    assert!(
        !atlas.end_retained_build(),
        "the verdict must be `rebuild`, not `usable`"
    );
}

#[test]
fn a_short_walk_aborts_the_retained_build() {
    // The other direction: the walk produced fewer nodes than the retained
    // tree holds, so some slot is stale. Nothing about that is repairable
    // in place.
    let mut atlas = fresh(LeafSize::new(40.0, 20.0), LeafSize::new(40.0, 20.0));

    atlas.begin_retained_build();
    let _ = atlas.add_leaf(LeafSize::new(40.0, 20.0)).unwrap();
    assert!(!atlas.end_retained_build());
}

// ── Fingerprint arithmetic (RFC-0032 §R2) ─────────────────────────────

#[test]
fn negative_zero_is_not_the_same_fingerprint_as_zero() {
    // The dangerous one. Hashing the `f32` directly would make `-0.0` and
    // `0.0` compare equal, so a leaf that moved between them would be
    // reported permanently *clean*, silently, and with no way to see it.
    let mut a = LayoutFingerprint::new(NodeKind::Leaf);
    a.f32(0.0);
    let mut b = LayoutFingerprint::new(NodeKind::Leaf);
    b.f32(-0.0);
    assert_ne!(a.finish(), b.finish());
}

#[test]
fn nan_hashes_to_itself() {
    // The visible one. Hashing the `f32` directly would make `NaN != NaN`,
    // so a leaf holding one would be permanently dirty and would recompute
    // forever. Wasteful rather than wrong, but it would silently undo the
    // entire point of the retained path on any tree containing one.
    let mut a = LayoutFingerprint::new(NodeKind::Leaf);
    a.f32(f32::NAN);
    let mut b = LayoutFingerprint::new(NodeKind::Leaf);
    b.f32(f32::NAN);
    assert_eq!(a.finish(), b.finish());
}

#[test]
fn the_node_kind_is_part_of_the_fingerprint() {
    let mut a = LayoutFingerprint::new(NodeKind::Leaf);
    a.f32(1.0);
    let mut b = LayoutFingerprint::new(NodeKind::Container);
    b.f32(1.0);
    assert_ne!(
        a.finish(),
        b.finish(),
        "two different kinds carrying the same numbers must not collide \
             into `unchanged`"
    );
}

#[test]
fn text_content_is_part_of_the_layout_fingerprint() {
    // The row the whole RFC turns on: text content is layout-class, which
    // is what lets a clean text leaf skip glyph shaping and what makes a
    // missed classification produce un-wrapped text.
    let spec = |content: &str| TextLeaf {
        content: content.to_string(),
        font_size: 14.0,
        weight: 400,
        family: None,
        width: None,
        fallback: (10.0, 14.0),
    };
    let mut atlas = LayoutAtlas::new();
    let t = atlas.add_text_leaf(spec("hello")).unwrap();
    atlas.set_root(t).unwrap();
    atlas.compute(Viewport::new(200.0, 200.0)).unwrap();

    atlas.begin_retained_build();
    let t2 = atlas
        .add_text_leaf(spec("hello world, rather longer"))
        .unwrap();
    atlas.set_root(t2).unwrap();
    assert!(atlas.end_retained_build());

    assert_eq!(
        atlas.layout_dirty_targets().len(),
        1,
        "an edited string must mark its leaf for re-measurement"
    );
}

#[test]
fn recompute_dirty_with_text_reaches_the_sizer() {
    // RFC-0032 §R5: the sizer-less `recompute_dirty` would size this leaf
    // at its fallback. The whole reason `recompute_dirty_with_text` exists
    // is that the fallback is a *single line*.
    struct FixedSizer;
    impl crate::text::TextSizer for FixedSizer {
        fn measure(
            &mut self,
            _content: &str,
            _font_size: f32,
            _wrap: Option<f32>,
            _weight: u16,
            _family: Option<&str>,
        ) -> (f32, f32) {
            (77.0, 88.0)
        }
    }

    let mut atlas = LayoutAtlas::new();
    let t = atlas
        .add_text_leaf(TextLeaf {
            content: "hello".to_string(),
            font_size: 14.0,
            weight: 400,
            family: None,
            width: None,
            fallback: (10.0, 14.0),
        })
        .unwrap();
    let root = atlas
        .add_container(
            // `Start`, not the default `Stretch`: a stretched child is
            // sized by its parent and would report 200 whatever the sizer
            // said, which would make this test pass for the wrong reason.
            ContainerStyle::new(Some(200.0), Some(200.0)).with_align(Align::Start),
            &[t],
        )
        .unwrap();
    atlas.set_root(root).unwrap();
    atlas
        .compute_with_text(Viewport::new(200.0, 200.0), &mut FixedSizer)
        .unwrap();

    atlas.mark_dirty_all(&[TargetId::new(
        0,
        atlas.current_generation(),
        TargetKind::AtlasNode as u16,
    )]);
    atlas
        .recompute_dirty_with_text(Viewport::new(200.0, 200.0), &mut FixedSizer)
        .unwrap();

    let rect = atlas.resolved_rect(t).unwrap().unwrap();
    assert!(
        (rect.height - 88.0).abs() < 1.5,
        "the retained path resolved height {}, it fell back to the \
             single-line size instead of asking the sizer",
        rect.height
    );
}
