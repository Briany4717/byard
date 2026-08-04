use super::*;

// ── Z-layer pool partitioning (RFC-0017 layered draw batches) ──────────────

/// Shorthand: a [`crate::frame::LayerMark`] whose pool cursors are all `n`.
fn mark(n: u32) -> crate::frame::LayerMark {
    crate::frame::LayerMark {
        solid: n,
        decorated: n,
        texture: n,
        vector: n,
        text: n,
        canvas: n,
        ripple: n,
        backdrop: n,
    }
}

/// The per-segment text ranges, the pool the ported layer-partition
/// tests below assert on (every pool follows the same arithmetic).
fn text_ranges(segments: &[SegmentRanges]) -> Vec<std::ops::Range<usize>> {
    segments.iter().map(|s| s.text.clone()).collect()
}

#[test]
fn no_marks_is_one_full_range() {
    let none: &[crate::frame::LayerMark] = &[];
    assert_eq!(
        text_ranges(&compute_segments(none, &[], &mark(5))),
        vec![0..5]
    );
    assert_eq!(
        text_ranges(&compute_segments(none, &[], &mark(0))),
        vec![0..0]
    );
}

#[test]
fn marks_split_the_pool_into_contiguous_layers() {
    let marks = [mark(2), mark(2), mark(4)];
    // Layer 1 is empty (two marks at the same cursor), legal, draws nothing.
    assert_eq!(
        text_ranges(&compute_segments(&marks, &[], &mark(6))),
        vec![0..2, 2..2, 2..4, 4..6]
    );
}

#[test]
fn malformed_marks_clamp_instead_of_panicking() {
    // Overshooting cursor → clamped to the pool length; a decreasing
    // cursor → clamped to monotonic (empty layer). Render-thread safety:
    // a logic-thread bug degrades to a misdrawn layer, never a panic.
    let marks = [mark(9), mark(1)];
    assert_eq!(
        text_ranges(&compute_segments(&marks, &[], &mark(4))),
        vec![0..4, 4..4, 4..4]
    );
}

// ── Backdrop barriers split segments (RFC-0023 §2) ─────────────────────────

/// A [`crate::frame::LayerMark`] with every pool cursor at `n` except the
/// backdrop cursor, which sits at `b`.
fn mark_b(n: u32, b: u32) -> crate::frame::LayerMark {
    crate::frame::LayerMark {
        backdrop: b,
        ..mark(n)
    }
}

#[test]
fn a_backdrop_barrier_splits_the_single_layer_into_two_segments() {
    // 6 primitives per pool, one backdrop whose barrier snapshot sits at
    // cursor 4: segment 0 draws 0..4 then blurs for pane 0; segment 1
    // composites it and draws 4..6.
    let segs = compute_segments(&[], &[mark_b(4, 0)], &mark_b(6, 1));
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].text, 0..4);
    assert_eq!(segs[0].backdrop_after, Some(0));
    assert_eq!(segs[1].text, 4..6);
    assert_eq!(segs[1].backdrop_after, None);
}

#[test]
fn two_backdrops_in_one_layer_stack_in_painter_order() {
    // The upper pane's barrier (cursor 5) comes after the lower's
    // (cursor 2), natural painter's order, the RFC's "double blur".
    let segs = compute_segments(&[], &[mark_b(2, 0), mark_b(5, 1)], &mark_b(8, 2));
    assert_eq!(text_ranges(&segs), vec![0..2, 2..5, 5..8]);
    assert_eq!(segs[0].backdrop_after, Some(0));
    assert_eq!(segs[1].backdrop_after, Some(1));
    assert_eq!(segs[2].backdrop_after, None);
}

#[test]
fn a_backdrop_inside_an_overlay_layer_splits_only_that_layer() {
    // Layer boundary at cursor 3 (no backdrops in the main layer); the
    // overlay layer holds one backdrop at cursor 5.
    let layers = [mark_b(3, 0)];
    let segs = compute_segments(&layers, &[mark_b(5, 0)], &mark_b(8, 1));
    assert_eq!(text_ranges(&segs), vec![0..3, 3..5, 5..8]);
    assert_eq!(segs[0].backdrop_after, None, "main layer intact");
    assert_eq!(segs[1].backdrop_after, Some(0));
    assert_eq!(segs[2].backdrop_after, None);
}

#[test]
fn a_malformed_backdrop_cursor_clamps_into_its_layer() {
    // The barrier snapshot overshoots the pool: it clamps to the layer
    // end, an empty trailing segment, never a panic.
    let segs = compute_segments(&[], &[mark_b(9, 0)], &mark_b(4, 1));
    assert_eq!(text_ranges(&segs), vec![0..4, 4..4]);
    assert_eq!(segs[0].backdrop_after, Some(0));
}

#[test]
fn sub_slice_clamps_to_the_slice_bounds() {
    let s = [10, 20, 30];
    assert_eq!(sub_slice(&s, &(1..3)), &[20, 30]);
    // Parallel depth/clip slices may be shorter than the pool, clamp.
    assert_eq!(sub_slice(&s, &(2..7)), &[30]);
    assert_eq!(sub_slice(&s, &(5..7)), &[] as &[i32]);
}

// ── INV-8: paint-time transforms never trigger a relayout ─────────────────

#[test]
fn encoder_module_never_calls_layout_atlas_compute() {
    // RFC-0011 (INV-8): a paint-time `Transform` must never cause a Taffy
    // relayout. Structurally enforced by module boundaries (`encoder`
    // never imports `crate::atlas`), this test scans the encoder's own
    // sources for the literal call so a future edit can't reintroduce it
    // without at least this test noticing.
    //
    // Built at runtime (not a literal in this file) so this very
    // assertion doesn't trip on itself via `include_str!`. The file list
    // is every `.rs` file in this directory (`ls src/encoder/*.rs`),
    // keep it in sync when adding a new one, since `include_str!` can't
    // glob a directory.
    let forbidden_call = ["LayoutAtlas", "::", "compute"].concat();
    for (name, src) in [
        ("mod.rs", include_str!("mod.rs")),
        ("backdrop.rs", include_str!("backdrop.rs")),
        ("canvas_shape.rs", include_str!("canvas_shape.rs")),
        ("decorated_box.rs", include_str!("decorated_box.rs")),
        ("gpu_timer.rs", include_str!("gpu_timer.rs")),
        ("ripple.rs", include_str!("ripple.rs")),
        ("text_glyph.rs", include_str!("text_glyph.rs")),
        ("texture_sampler.rs", include_str!("texture_sampler.rs")),
    ] {
        assert!(
            !src.contains(&forbidden_call),
            "{name} must never call into layout recomputation (INV-8)"
        );
    }
}

// ── BoxInstance layout ────────────────────────────────────────────────────
//
// The GPU relies on the byte layout of BoxInstance being exactly what
// `BoxInstance::layout()` declares. Any mismatch silently corrupts every
// rendered rectangle. These tests catch such regressions at compile time.

#[test]
fn box_instance_size_and_alignment() {
    // 3 fields × [f32; 4] × 4 bytes = 48, + Transform's 8 × f32 × 4 bytes
    // = 32, + `smooth`'s 4 (RFC-0031 §S1), for 84 bytes total. The GPU
    // stride declaration in `layout()` hardcodes this value.
    assert_eq!(
        std::mem::size_of::<BoxInstance>(),
        84,
        "BoxInstance must be exactly 84 bytes"
    );
    // f32 requires 4-byte alignment; wgpu vertex attributes assume this.
    assert_eq!(std::mem::align_of::<BoxInstance>(), 4);
}

#[test]
fn box_instance_field_offsets_match_shader_locations() {
    // `BoxInstance::layout()` declares offsets 0, 16, 32, 48 for
    // rect/color/radii/transform. If any field is reordered or padded,
    // the shader sees garbage.
    assert_eq!(std::mem::offset_of!(BoxInstance, rect), 0);
    assert_eq!(std::mem::offset_of!(BoxInstance, color), 16);
    assert_eq!(std::mem::offset_of!(BoxInstance, radii), 32);
    assert_eq!(std::mem::offset_of!(BoxInstance, transform), 48);
    // `smooth` is declared *after* `transform` precisely so the four
    // offsets above are the ones they have always been (RFC-0031 §S1).
    assert_eq!(std::mem::offset_of!(BoxInstance, smooth), 80);
    assert_eq!(std::mem::offset_of!(Transform, translate), 0);
    assert_eq!(std::mem::offset_of!(Transform, scale), 8);
    assert_eq!(std::mem::offset_of!(Transform, rotate), 16);
    assert_eq!(std::mem::offset_of!(Transform, origin), 20);
    assert_eq!(std::mem::offset_of!(Transform, opacity), 28);
}

#[test]
fn box_instance_layout_stride_step_mode_and_attributes() {
    let layout = BoxInstance::layout();

    assert_eq!(
        layout.array_stride, 84,
        "stride must equal size_of::<BoxInstance>()"
    );
    assert_eq!(
        layout.step_mode,
        wgpu::VertexStepMode::Instance,
        "must advance per instance, not per vertex"
    );

    // Verify each attribute's (shader_location, offset) pair.
    let attrs = layout.attributes;
    assert_eq!(attrs.len(), 9);

    assert_eq!(attrs[0].shader_location, 1); // rect
    assert_eq!(attrs[0].offset, 0);
    assert_eq!(attrs[0].format, wgpu::VertexFormat::Float32x4);

    assert_eq!(attrs[1].shader_location, 2); // color
    assert_eq!(attrs[1].offset, 16);
    assert_eq!(attrs[1].format, wgpu::VertexFormat::Float32x4);

    assert_eq!(attrs[2].shader_location, 3); // radii
    assert_eq!(attrs[2].offset, 32);
    assert_eq!(attrs[2].format, wgpu::VertexFormat::Float32x4);

    assert_eq!(attrs[3].shader_location, 4); // transform.translate
    assert_eq!(attrs[3].offset, 48);
    assert_eq!(attrs[3].format, wgpu::VertexFormat::Float32x2);

    assert_eq!(attrs[4].shader_location, 5); // transform.scale
    assert_eq!(attrs[4].offset, 56);
    assert_eq!(attrs[4].format, wgpu::VertexFormat::Float32x2);

    assert_eq!(attrs[5].shader_location, 6); // transform.rotate
    assert_eq!(attrs[5].offset, 64);
    assert_eq!(attrs[5].format, wgpu::VertexFormat::Float32);

    assert_eq!(attrs[6].shader_location, 7); // transform.origin
    assert_eq!(attrs[6].offset, 68);
    assert_eq!(attrs[6].format, wgpu::VertexFormat::Float32x2);

    assert_eq!(attrs[7].shader_location, 8); // transform.opacity
    assert_eq!(attrs[7].offset, 76);
    assert_eq!(attrs[7].format, wgpu::VertexFormat::Float32);

    // Location 9 is the parallel draw-order depth buffer's, so `smooth`
    // takes 10 (RFC-0031 §S1).
    assert_eq!(attrs[8].shader_location, 10); // smooth
    assert_eq!(attrs[8].offset, 80);
    assert_eq!(attrs[8].format, wgpu::VertexFormat::Float32);
}

#[test]
fn box_instance_bytemuck_cast_produces_correct_byte_count() {
    // bytemuck::cast_slice is used in encode_frame to upload instances.
    // A wrong Pod impl (e.g. accidental padding) would give the wrong length.
    let instances = [
        BoxInstance {
            rect: [0.0, 0.0, 100.0, 50.0],
            color: [1.0, 0.0, 0.5, 1.0],
            radii: [8.0, 8.0, 8.0, 8.0],
            transform: Transform::IDENTITY,
            smooth: 0.0,
        },
        BoxInstance {
            rect: [10.0, 20.0, 200.0, 80.0],
            color: [0.0, 1.0, 0.0, 0.8],
            radii: [0.0; 4],
            transform: Transform::IDENTITY,
            smooth: 0.0,
        },
    ];
    let bytes: &[u8] = bytemuck::cast_slice(&instances);
    assert_eq!(bytes.len(), 2 * 84, "2 instances × 84 bytes each");
}

#[test]
// Exact bit-level zero is what Zeroable guarantees, so strict equality is
// correct here: we are not comparing computed floats but literal bit patterns.
#[allow(clippy::float_cmp)]
fn box_instance_zeroed_is_valid_and_all_zero() {
    // bytemuck::Zeroable guarantees that all-zero bytes form a valid
    // BoxInstance. Used implicitly when zero-filling instance buffers.
    let z: BoxInstance = bytemuck::Zeroable::zeroed();
    assert_eq!(z.rect, [0.0; 4]);
    assert_eq!(z.color, [0.0; 4]);
    assert_eq!(z.radii, [0.0; 4]);
}

// ── QUAD_VERTICES ─────────────────────────────────────────────────────────

#[test]
// QUAD_VERTICES is a compile-time constant array of exact integer-valued
// floats (0.0 and 1.0). Strict equality is intentional: we are verifying
// that no rounding crept in, not comparing computed results.
#[allow(clippy::float_cmp)]
fn quad_vertices_form_unit_square() {
    // 4 vertices × 2 coords (x, y) = 8 floats.
    assert_eq!(QUAD_VERTICES.len(), 8);

    // Every coordinate must be exactly 0.0 or 1.0.
    for &v in QUAD_VERTICES {
        assert!(
            v == 0.0 || v == 1.0,
            "unexpected quad vertex coordinate: {v}"
        );
    }

    // All four corners of the unit square [0,1]² must be present.
    let pairs: Vec<(f32, f32)> = QUAD_VERTICES.chunks(2).map(|c| (c[0], c[1])).collect();

    assert!(pairs.contains(&(0.0, 0.0)), "missing top-left  (0,0)");
    assert!(pairs.contains(&(1.0, 0.0)), "missing top-right  (1,0)");
    assert!(pairs.contains(&(0.0, 1.0)), "missing bottom-left  (0,1)");
    assert!(pairs.contains(&(1.0, 1.0)), "missing bottom-right (1,1)");
}

// ── SDF ───────────────────────────────────────────────────────────────────

/// CPU reimplementation of the `sd_rounded_box` WGSL function.
///
/// Mirrors the shader logic exactly so algebraic properties can be
/// asserted in unit tests without spinning up a GPU backend.
fn cpu_sd_rounded_box(p: [f32; 2], b: [f32; 2], r: [f32; 4]) -> f32 {
    // Screen Y increases downward, so top half has p[1] < 0.
    // Default case (p.x <= 0 && p.y <= 0) → top-left radius.
    let mut r_corner = r[0]; // Top-Left
    if p[0] > 0.0 && p[1] < 0.0 {
        r_corner = r[1]; // Top-Right
    }
    if p[0] > 0.0 && p[1] > 0.0 {
        r_corner = r[2]; // Bottom-Right
    }
    if p[0] < 0.0 && p[1] > 0.0 {
        r_corner = r[3]; // Bottom-Left
    }

    let q_x = p[0].abs() - b[0] + r_corner;
    let q_y = p[1].abs() - b[1] + r_corner;

    let length_max_q = (q_x.max(0.0) * q_x.max(0.0) + q_y.max(0.0) * q_y.max(0.0)).sqrt();

    q_x.max(q_y).min(0.0) + length_max_q - r_corner
}

#[test]
fn sdf_zero_radii_degenerates_to_axis_aligned_rect() {
    // With r=[0,0,0,0] the SDF must equal the plain AABB SDF.
    // This is the most common production case (solid rectangle, no rounding).
    let half = [50.0_f32, 50.0];
    let r = [0.0_f32; 4];

    // Centre: strictly inside.
    assert!(cpu_sd_rounded_box([0.0, 0.0], half, r) < 0.0);

    // Right edge midpoint: on the boundary → SDF = 0.
    // q_x = 50 − 50 + 0 = 0, q_y = 0 − 50 + 0 = −50
    // → min(max(0, −50), 0) + length((0, 0)) − 0 = 0
    let d = cpu_sd_rounded_box([50.0, 0.0], half, r);
    assert!(d.abs() < 0.001, "right edge: {d}");

    // Outside right edge (x=55, y=0): SDF = 5.
    // q_x = 55 − 50 = 5, q_y = 0 − 50 = −50
    // → min(max(5, −50), 0) + length((5, 0)) − 0 = 0 + 5 = 5
    let d = cpu_sd_rounded_box([55.0, 0.0], half, r);
    assert!((d - 5.0).abs() < 0.001, "right exterior: {d}");

    // Outside corner (x=55, y=55): SDF = √(5²+5²) ≈ 7.071.
    // q_x = 5, q_y = 5 → 0 + √50 ≈ 7.071
    let d = cpu_sd_rounded_box([55.0, 55.0], half, r);
    assert!((d - (50.0_f32).sqrt()).abs() < 0.001, "sharp corner: {d}");
}

#[test]
fn sdf_all_four_quadrants_select_correct_radius() {
    // Fully asymmetric radii: TL=10, TR=20, BR=30, BL=40.
    // For each quadrant, place a point at a distance that depends only
    // on the corner radius of that quadrant and verify the expected SDF.
    //
    // Strategy: at (±45, ∓45) (all inside the box), the SDF depends on
    // r_corner because q_* includes `+ r_corner`. By varying only the
    // active radius we can isolate each quadrant.
    //
    // Each expected value computed analytically:
    // q = 45 − 50 + r = r − 5  (same for both axes at this symmetric point)
    // When r ≥ 5: both q components ≥ 0 → result = √2·(r−5) − r
    // When r < 5: both q components < 0 → result = (r−5) − r = −5
    fn expected(r: f32) -> f32 {
        let q = 45.0 - 50.0 + r; // = r - 5
        if q <= 0.0 {
            q - r
        } else {
            (2.0_f32).sqrt() * q - r
        }
    }

    let half = [50.0_f32, 50.0];
    let r = [10.0_f32, 20.0, 30.0, 40.0]; // TL, TR, BR, BL

    // Top-Left (p.x < 0, p.y < 0) → r[0] = 10
    let d = cpu_sd_rounded_box([-45.0, -45.0], half, r);
    assert!((d - expected(10.0)).abs() < 0.001, "TL: {d}");

    // Top-Right (p.x > 0, p.y < 0) → r[1] = 20
    let d = cpu_sd_rounded_box([45.0, -45.0], half, r);
    assert!((d - expected(20.0)).abs() < 0.001, "TR: {d}");

    // Bottom-Right (p.x > 0, p.y > 0) → r[2] = 30
    let d = cpu_sd_rounded_box([45.0, 45.0], half, r);
    assert!((d - expected(30.0)).abs() < 0.001, "BR: {d}");

    // Bottom-Left (p.x < 0, p.y > 0) → r[3] = 40
    let d = cpu_sd_rounded_box([-45.0, 45.0], half, r);
    assert!((d - expected(40.0)).abs() < 0.001, "BL: {d}");
}

#[test]
// The recovered values are the same bytes written as the original,
// no arithmetic involved, so strict bit-equality is the correct assertion.
#[allow(clippy::float_cmp)]
fn box_instance_bytemuck_round_trip_preserves_values() {
    // Verifies that casting BoxInstance → &[u8] → BoxInstance returns
    // identical field values. Catches any Pod impl that shuffles bytes.
    let original = BoxInstance {
        rect: [1.0, 2.0, 300.0, 400.0],
        color: [0.25, 0.5, 0.75, 1.0],
        radii: [8.0, 16.0, 24.0, 32.0],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    };

    let bytes: &[u8] = bytemuck::bytes_of(&original);
    let recovered: &BoxInstance = bytemuck::from_bytes(bytes);

    assert_eq!(recovered.rect, original.rect);
    assert_eq!(recovered.color, original.color);
    assert_eq!(recovered.radii, original.radii);
}

#[test]
fn sdf_mathematical_quadrants_and_boundaries() {
    // 100×100 box centred at origin → half_size = (50, 50).
    let half_size = [50.0_f32, 50.0];
    // Asymmetric radii to verify per-corner selection.
    let radii = [10.0_f32, 15.0, 20.0, 25.0];

    // Centre is deep inside, SDF must be strongly negative.
    let dist_center = cpu_sd_rounded_box([0.0, 0.0], half_size, radii);
    assert!(dist_center < -20.0, "centre: {dist_center}");

    // Top edge midpoint (x=0, y=−50): SDF ≈ 0.
    // q_x = 0 − 50 + 10 = −40, q_y = 50 − 50 + 10 = 10
    // → min(max(−40, 10), 0) + length((0, 10)) − 10 = 0 + 10 − 10 = 0
    let dist_edge = cpu_sd_rounded_box([0.0, -50.0], half_size, radii);
    assert!(dist_edge.abs() < 0.001, "top edge: {dist_edge}");

    // Far outside corner, SDF must be substantially positive.
    let dist_outer = cpu_sd_rounded_box([100.0, 100.0], half_size, radii);
    assert!(dist_outer > 40.0, "outer: {dist_outer}");

    // Asymmetry: TL radius=10 gives a different SDF than BR radius=20
    // at the same distance from their respective corners.
    let dist_tl = cpu_sd_rounded_box([-45.0, -45.0], half_size, radii);
    let dist_br = cpu_sd_rounded_box([45.0, 45.0], half_size, radii);
    assert!(
        (dist_tl - dist_br).abs() > 1.0,
        "TL={dist_tl}, BR={dist_br} should differ"
    );

    // Axis boundary (x=0, y=−45): default case (TL radius=10) must apply.
    // q_x = 0 − 50 + 10 = −40, q_y = 45 − 50 + 10 = 5
    // → min(max(−40, 5), 0) + length((0, 5)) − 10 = 0 + 5 − 10 = −5
    let dist_boundary = cpu_sd_rounded_box([0.0, -45.0], half_size, radii);
    assert!(
        (dist_boundary - (-5.0)).abs() < 0.001,
        "axis boundary: {dist_boundary}"
    );
}

// ── SDF: corner arc ───────────────────────────────────────────────────────

#[test]
fn sdf_point_exactly_on_corner_arc_has_zero_distance() {
    // For the BR corner with r=20 and half=[50,50]:
    //   arc centre = (50−20, 50−20) = (30, 30)
    //   point at 45° on the arc: p = (30 + 20/√2, 30 + 20/√2) ≈ (44.14, 44.14)
    //
    // Manual calculation (BR quadrant → r_corner=20):
    //   q_x = 44.14 − 50 + 20 = 14.14
    //   q_y = same
    //   result = 0 + √(14.14² + 14.14²) − 20 = √400 − 20 = 0
    let half = [50.0_f32, 50.0];
    let r = [0.0_f32, 0.0, 20.0, 0.0]; // only BR has radius
    let offset = 20.0_f32 / (2.0_f32).sqrt();
    let p = [30.0 + offset, 30.0 + offset];
    let d = cpu_sd_rounded_box(p, half, r);
    assert!(d.abs() < 0.001, "on arc: {d}");
}

// ── SDF: degenerate radii ─────────────────────────────────────────────────

#[test]
fn sdf_full_radius_equals_half_size_acts_like_circle() {
    // When r == half for all corners the rounded rect degenerates to a circle.
    // For half=[25,25] and r=[25,25,25,25] the boundary lies at distance 25
    // from the origin in every direction.
    //
    // Right midpoint p=(25,0), falls into the TL default case since p.y==0:
    //   q_x = 25−25+25 = 25, q_y = 0−25+25 = 0
    //   result = 0 + length((25,0)) − 25 = 25 − 25 = 0 (on boundary) ✓
    //
    // TR diagonal p=(25/√2, −25/√2), TR case (p.x>0, p.y<0):
    //   q_x = q_y = 25/√2 − 25 + 25 = 25/√2 ≈ 17.68
    //   result = 0 + √(17.68²+17.68²) − 25 = 25 − 25 = 0 (on boundary) ✓
    let half = [25.0_f32, 25.0];
    let r = [25.0_f32; 4];

    let d = cpu_sd_rounded_box([25.0, 0.0], half, r);
    assert!(d.abs() < 0.001, "right midpoint: {d}");

    let diag = 25.0_f32 / (2.0_f32).sqrt();
    let d = cpu_sd_rounded_box([diag, -diag], half, r);
    assert!(d.abs() < 0.001, "TR diagonal: {d}");

    let d = cpu_sd_rounded_box([0.0, 0.0], half, r);
    assert!((d - (-25.0)).abs() < 0.001, "centre: {d}");
}

#[test]
fn sdf_radius_exceeding_half_size_is_finite() {
    // The SDF function does not clamp radii to half-size. A radius larger
    // than half-size produces a mathematically valid (though visually odd)
    // value. This test documents that the function is total, it never
    // panics or returns NaN/±inf for any finite inputs.
    let half = [50.0_f32, 50.0];
    let r_big = [60.0_f32; 4];
    let r_huge = [1000.0_f32; 4];

    assert!(cpu_sd_rounded_box([0.0, 0.0], half, r_big).is_finite());
    assert!(cpu_sd_rounded_box([0.0, 0.0], half, r_huge).is_finite());
    assert!(cpu_sd_rounded_box([100.0, 100.0], half, r_big).is_finite());
}

// ── SDF: superelliptical corners (RFC-0031 §S1–S3) ────────────────────────

/// CPU twin of the WGSL `lp_norm`: the Lⁿ norm of a non-negative 2-vector
/// and the magnitude of its own gradient.
fn cpu_lp_norm(v: [f32; 2], n: f32) -> (f32, f32) {
    let a = v[0].powf(n) + v[1].powf(n);
    if a <= 0.0 {
        return (0.0, 1.0);
    }
    let f = a.powf(1.0 / n);
    let gx = (v[0] / f).powf(n - 1.0);
    let gy = (v[1] / f).powf(n - 1.0);
    (f, (gx * gx + gy * gy).sqrt().max(1e-4))
}

/// CPU twin of the WGSL `sd_rounded_box` **with** the RFC-0031 corner
/// exponent, structured exactly as the shader is, including the `n == 2`
/// short-circuit, which is the claim [`smooth_zero_is_the_historical_field`]
/// checks.
#[allow(clippy::many_single_char_names)]
// The `n == 2.0` test is the shader's, verbatim: RFC-0031 §S1 makes an
// exact comparison a *correctness* requirement, because `pow(x, 2)` and
// `x * x` differ in the last ULP and an epsilon here would let the
// approximate path run on the default profile.
#[allow(clippy::float_cmp)]
fn cpu_sd_rounded_box_n(p: [f32; 2], b: [f32; 2], r: [f32; 4], n: f32) -> f32 {
    let mut r_corner = r[0];
    if p[0] > 0.0 && p[1] < 0.0 {
        r_corner = r[1];
    }
    if p[0] > 0.0 && p[1] > 0.0 {
        r_corner = r[2];
    }
    if p[0] < 0.0 && p[1] > 0.0 {
        r_corner = r[3];
    }
    let q = [p[0].abs() - b[0] + r_corner, p[1].abs() - b[1] + r_corner];
    let corner = [q[0].max(0.0), q[1].max(0.0)];
    let inner = q[0].max(q[1]).min(0.0);
    if n == 2.0 {
        return inner + (corner[0] * corner[0] + corner[1] * corner[1]).sqrt() - r_corner;
    }
    let (value, grad) = cpu_lp_norm(corner, n);
    (inner + value - r_corner) / grad
}

/// INV-22, and the load-bearing test of RFC-0031 §S1: at `smooth: 0` the
/// field is the one that existed before the property did, **bitwise**, not
/// approximately. `pow(x, 2)` and `x * x` differ in the last ULP, and every
/// golden image in the repo would move with them, so the short-circuit is a
/// correctness requirement rather than an optimisation.
#[test]
fn smooth_zero_is_the_historical_field() {
    let half = [60.0_f32, 34.0];
    let radii = [16.0_f32, 4.0, 28.0, 0.0];
    let n = crate::frame::corner_exponent(0.0);
    assert_eq!(
        n.to_bits(),
        2.0_f32.to_bits(),
        "smooth: 0 must land on n = 2"
    );
    for iy in -50_i16..=50 {
        for ix in -80_i16..=80 {
            let p = [f32::from(ix), f32::from(iy)];
            let before = cpu_sd_rounded_box(p, half, radii);
            let after = cpu_sd_rounded_box_n(p, half, radii, n);
            assert_eq!(
                before.to_bits(),
                after.to_bits(),
                "field moved at {p:?}: {before} → {after}"
            );
        }
    }
}

/// §S1: above `n = 2` the corner bulges *outward*, a point that the
/// circular arc excludes is inside the squircle, and it does so
/// monotonically in `smooth`, which is what makes the property a slider
/// rather than a switch (§Q1).
#[test]
fn smoothing_pushes_the_corner_outward_monotonically() {
    let half = [50.0_f32, 50.0];
    let radii = [40.0_f32; 4];
    // On the corner diagonal, just outside the circular arc.
    let p = [50.0 - 40.0 + 40.0 / 2.0_f32.sqrt() + 1.0; 2];
    let mut previous = f32::INFINITY;
    for step in 0..=10 {
        #[allow(clippy::cast_precision_loss)]
        let smooth = step as f32 / 10.0;
        let d = cpu_sd_rounded_box_n(p, half, radii, crate::frame::corner_exponent(smooth));
        assert!(d < previous, "smooth {smooth} did not extend the corner");
        previous = d;
    }
    assert!(
        cpu_sd_rounded_box_n(p, half, radii, 2.0) > 0.0,
        "the reference point must start outside the circular corner"
    );
    assert!(
        cpu_sd_rounded_box_n(p, half, radii, crate::frame::corner_exponent(1.0)) < 0.0,
        "and end inside the squircle"
    );
}

/// §S2's reason for existing, with the sign of the artefact corrected.
/// On the corner diagonal the Lⁿ field's gradient is `2^(1/n - 1/2)`, which
/// *falls* to ≈0.79 at `n = 6`, so an uncorrected field draws the corner's
/// anti-aliased fringe ~26 % **wider** than the edge's: a smear at exactly
/// the corners the property exists to give a crisper profile. Normalising by
/// the analytic gradient inside the field restores unit slope, so one
/// screen-space coverage rule stays correct everywhere.
#[test]
fn the_corner_fringe_is_as_wide_as_the_edge_fringe() {
    let half = [50.0_f32, 50.0];
    let radii = [40.0_f32; 4];
    let n = crate::frame::corner_exponent(1.0);
    let slope = |p: [f32; 2]| {
        let h = 0.05_f32;
        let dx = (cpu_sd_rounded_box_n([p[0] + h, p[1]], half, radii, n)
            - cpu_sd_rounded_box_n([p[0] - h, p[1]], half, radii, n))
            / (2.0 * h);
        let dy = (cpu_sd_rounded_box_n([p[0], p[1] + h], half, radii, n)
            - cpu_sd_rounded_box_n([p[0], p[1] - h], half, radii, n))
            / (2.0 * h);
        (dx * dx + dy * dy).sqrt()
    };
    // A point on the straight edge, and one on the corner diagonal.
    let edge = slope([50.0, 0.0]);
    let corner = slope([46.0, 46.0]);
    assert!(
        (edge - 1.0).abs() < 0.02,
        "the straight edge must keep unit slope: {edge}"
    );
    assert!(
        (corner - 1.0).abs() < 0.05,
        "the corner's slope drives the fringe width: {corner}"
    );

    // And the uncorrected field is what that correction is *for*: without
    // the division the same point overshoots unit slope well past tolerance.
    let raw = |p: [f32; 2]| {
        let q = [
            p[0].abs() - half[0] + radii[0],
            p[1].abs() - half[1] + radii[0],
        ];
        cpu_lp_norm([q[0].max(0.0), q[1].max(0.0)], n).0 - radii[0]
    };
    let h = 0.05_f32;
    let dx = (raw([46.0 + h, 46.0]) - raw([46.0 - h, 46.0])) / (2.0 * h);
    let dy = (raw([46.0, 46.0 + h]) - raw([46.0, 46.0 - h])) / (2.0 * h);
    let uncorrected = (dx * dx + dy * dy).sqrt();
    // 2^(1/6 − 1/2) ≈ 0.7937, a 26 % wide fringe if left alone.
    assert!(
        uncorrected < 0.85,
        "the uncorrected Lⁿ field should undershoot unit slope: {uncorrected}"
    );
}

/// §Q2: a shadow uses its caster's exponent. The comparison that means
/// something is *shape*, not distance, a spread shadow is a bigger box, so
/// its absolute field differs by construction. What must match is the
/// silhouette: the boundary's reach along the corner diagonal relative to
/// its reach along the axis. That ratio is what the eye reads as "corner
/// profile", and a shadow whose profile differs from its caster's reads as a
/// rendering error.
#[test]
fn a_shadow_keeps_its_casters_corner_profile() {
    let half = [50.0_f32, 50.0];
    let radii = [24.0_f32; 4];
    let spread = 6.0_f32;
    let s_half = [half[0] + spread, half[1] + spread];
    let s_radii = [radii[0] + spread; 4];

    // Where the field crosses zero along `dir`, by bisection.
    let reach = |b: [f32; 2], r: [f32; 4], n: f32, dir: [f32; 2]| {
        let (mut lo, mut hi) = (0.0_f32, 400.0_f32);
        for _ in 0..60 {
            let mid = f32::midpoint(lo, hi);
            if cpu_sd_rounded_box_n([dir[0] * mid, dir[1] * mid], b, r, n) < 0.0 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        f32::midpoint(lo, hi)
    };
    let diagonal = [0.5_f32.sqrt(); 2];
    let profile =
        |b: [f32; 2], r: [f32; 4], n: f32| reach(b, r, n, diagonal) / reach(b, r, n, [1.0, 0.0]);

    let n = crate::frame::corner_exponent(0.8);
    let caster = profile(half, radii, n);
    let shadow = profile(s_half, s_radii, n);
    assert!(
        (caster - shadow).abs() < 0.01,
        "shadow profile {shadow} does not match its caster's {caster}"
    );

    // And the assertion is discriminating: had the shadow silently kept the
    // circular corner, the same comparison would have separated them.
    let circular = profile(s_half, s_radii, 2.0);
    assert!(
        (caster - circular).abs() > 0.02,
        "the test cannot tell n = {n} from n = 2 ({caster} vs {circular})"
    );
}

// ── bytemuck: empty slice and non-finite values ───────────────────────────

#[test]
fn box_instance_cast_slice_empty_gives_zero_bytes() {
    // encode_frame guards with `if !instances.is_empty()` before creating
    // a buffer. Verify that casting an empty slice is safe and produces
    // zero bytes, not UB, not a panic.
    let empty: &[BoxInstance] = &[];
    let bytes: &[u8] = bytemuck::cast_slice(empty);
    assert_eq!(bytes.len(), 0);
}

#[test]
fn box_instance_pod_accepts_non_finite_floats() {
    // bytemuck::Pod requires every bit pattern to be a valid value.
    // NaN and ±inf are valid f32 bit patterns, so Pod must accept them.
    // encode_frame calls bytemuck::cast_slice on instances it receives;
    // if the caller passes NaN coordinates, the cast must not panic.
    let inst = BoxInstance {
        rect: [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0],
        color: [f32::NAN; 4],
        radii: [f32::INFINITY; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    };
    let bytes = bytemuck::bytes_of(&inst);
    assert_eq!(bytes.len(), 84, "NaN/inf must not change struct size");
}

// ── QUAD_VERTICES: TriangleStrip geometry ────────────────────────────────

#[test]
fn quad_vertices_triangle_strip_tiles_unit_square_without_gaps() {
    // TriangleStrip with 4 vertices produces exactly 2 triangles:
    //   T1: indices 0,1,2 → (TL, TR, BL)
    //   T2: indices 1,2,3 → (TR, BL, BR)
    //
    // Verify their combined area equals 1.0 (the unit square), which
    // proves they tile the surface without gaps or overlaps.
    fn tri_area(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32) -> f32 {
        ((bx - ax) * (cy - ay) - (cx - ax) * (by - ay)).abs() * 0.5
    }

    let p: Vec<(f32, f32)> = QUAD_VERTICES.chunks(2).map(|c| (c[0], c[1])).collect();

    let a1 = tri_area(p[0].0, p[0].1, p[1].0, p[1].1, p[2].0, p[2].1);
    let a2 = tri_area(p[1].0, p[1].1, p[2].0, p[2].1, p[3].0, p[3].1);

    assert!((a1 - 0.5).abs() < 0.001, "T1 area = {a1} (expected 0.5)");
    assert!((a2 - 0.5).abs() < 0.001, "T2 area = {a2} (expected 0.5)");
    assert!(
        (a1 + a2 - 1.0).abs() < 0.001,
        "total area = {} (expected 1.0)",
        a1 + a2
    );
}

// ── #31 scissor clipping: pure decision/heuristic functions ──────────────
//
// None of these touch wgpu, they're the CPU-mirror logic that decides
// *what* `encode_frame` will do, extracted so it's testable without a
// GPU device (project convention, see `text_glyph::needs_reshape`).

/// Tolerance-based f32 comparison, mirroring `engine.rs`'s test helper
/// of the same name, used in place of `assert_eq!` on raw floats to
/// satisfy `clippy::float_cmp` without losing the precision these
/// tests actually need (well under one logical pixel).
#[track_caller]
fn assert_f32_eq(actual: f32, expected: f32) {
    let diff = (actual - expected).abs();
    assert!(
        diff < 0.001,
        "expected {expected}, got {actual} (diff = {diff})"
    );
}

fn line(x: f32, y: f32, text: &str, font_size: f32, dirty: bool) -> TextLine {
    TextLine {
        x,
        y,
        text: text.to_string(),
        font_size,
        color: [0.0, 0.0, 0.0, 1.0],
        dirty,
    }
}

#[test]
fn text_line_bounds_grows_with_character_count() {
    let short = text_line_bounds(&line(0.0, 0.0, "a", 16.0, false));
    let long = text_line_bounds(&line(0.0, 0.0, "a much longer string", 16.0, false));
    assert!(
        long.width > short.width,
        "more characters must widen the bound"
    );
    // height depends only on font_size, not character count.
    assert_f32_eq(short.height, long.height);
}

#[test]
fn text_line_bounds_grows_with_font_size() {
    let small = text_line_bounds(&line(0.0, 0.0, "label", 12.0, false));
    let large = text_line_bounds(&line(0.0, 0.0, "label", 48.0, false));
    assert!(large.width > small.width);
    assert!(large.height > small.height);
}

#[test]
fn text_line_bounds_is_positioned_at_the_line_origin() {
    let r = text_line_bounds(&line(123.0, 45.0, "x", 16.0, false));
    assert_f32_eq(r.x, 123.0);
    assert_f32_eq(r.y, 45.0);
}

#[test]
fn text_line_bounds_never_yields_negative_dimensions() {
    // Defensive: an empty string is a degenerate but legitimate TextLine.
    let r = text_line_bounds(&line(0.0, 0.0, "", 16.0, false));
    assert!(r.width >= 0.0);
    assert!(r.height >= 0.0);
}

#[test]
fn dirty_text_bounds_is_none_when_nothing_is_dirty() {
    let texts = [
        line(0.0, 0.0, "a", 16.0, false),
        line(50.0, 50.0, "b", 16.0, false),
    ];
    assert!(dirty_text_bounds(&texts, &[], &[]).is_none());
}

#[test]
fn dirty_text_bounds_is_none_for_empty_slice() {
    assert!(dirty_text_bounds(&[], &[], &[]).is_none());
}

#[test]
fn dirty_text_bounds_ignores_non_dirty_lines() {
    let dirty_line = line(10.0, 10.0, "dirty", 16.0, true);
    let clean_line = line(1000.0, 1000.0, "clean", 16.0, false);
    let bounds = dirty_text_bounds(&[dirty_line.clone(), clean_line], &[], &[]).unwrap();
    let expected = text_line_bounds(&dirty_line);
    assert_eq!(bounds, expected, "clean line must not widen the union");
}

#[test]
fn dirty_text_bounds_merges_multiple_dirty_lines() {
    let a = line(0.0, 0.0, "a", 16.0, true);
    let b = line(200.0, 300.0, "b", 16.0, true);
    let merged = dirty_text_bounds(&[a.clone(), b.clone()], &[], &[]).unwrap();
    let expected = text_line_bounds(&a).union(&text_line_bounds(&b));
    assert_eq!(merged, expected);
}

#[test]
fn dirty_text_bounds_unions_with_previous_frame_bounds() {
    // A line that shrinks between frames: its NEW bounds alone would
    // leave the old (wider) footprint outside the scissor rect,
    // exactly the bug behind the shrinking-line visual-verification finding.
    let shrunk = line(0.0, 0.0, "a", 16.0, true);
    let previous_bounds = text_line_bounds(&line(0.0, 0.0, "a much longer string", 16.0, false));
    let bounds = dirty_text_bounds(std::slice::from_ref(&shrunk), &[], &[previous_bounds]).unwrap();
    let current = text_line_bounds(&shrunk);
    let expected = current.union(&previous_bounds);
    assert_eq!(
        bounds, expected,
        "must cover both current and previous bounds for a dirty line"
    );
    assert!(
        bounds.width >= previous_bounds.width,
        "must not be narrower than the previous frame's footprint"
    );
}

#[test]
fn dirty_text_bounds_unions_when_line_grows_between_frames() {
    // The inverse of the shrink case above: when the new bounds fully
    // contain the old ones, the union must still equal the new bounds
    // (not be artificially clamped back down to the smaller, previous
    // footprint).
    let grown = line(0.0, 0.0, "a much longer string", 16.0, true);
    let previous_bounds = text_line_bounds(&line(0.0, 0.0, "a", 16.0, false));
    let bounds = dirty_text_bounds(std::slice::from_ref(&grown), &[], &[previous_bounds]).unwrap();
    let current = text_line_bounds(&grown);
    assert_eq!(
        bounds, current,
        "previous bounds are fully contained, so union must equal current bounds"
    );
}

#[test]
fn dirty_text_bounds_unions_when_line_moves_without_resizing() {
    // A line that translates (same size, new position) between frames,
    // current and previous bounds do not overlap at all, so the union
    // must be the bounding box that spans both, not just one of them.
    let moved = line(500.0, 500.0, "a", 16.0, true);
    let previous_bounds = text_line_bounds(&line(0.0, 0.0, "a", 16.0, false));
    let bounds = dirty_text_bounds(std::slice::from_ref(&moved), &[], &[previous_bounds]).unwrap();
    let current = text_line_bounds(&moved);
    let expected = current.union(&previous_bounds);
    assert_eq!(bounds, expected);
    // Sanity: the union must still reach back to the old (top-left)
    // position, not just the new one.
    assert_f32_eq(bounds.x, 0.0);
    assert_f32_eq(bounds.y, 0.0);
}

#[test]
fn dirty_text_bounds_handles_previous_shorter_than_texts() {
    // `previous` is positionally aligned with `texts`, but a brand-new
    // line added this frame has no corresponding entry from the last
    // call (the slice is shorter than `texts`). `previous.get(i)` must
    // return `None` for it rather than panicking on an out-of-bounds
    // index, and the line's current bounds alone must be used.
    let existing = line(0.0, 0.0, "a", 16.0, false);
    let new_line = line(50.0, 50.0, "b", 16.0, true);
    let previous = [text_line_bounds(&existing)];
    let bounds = dirty_text_bounds(&[existing, new_line.clone()], &[], &previous).unwrap();
    let expected = text_line_bounds(&new_line);
    assert_eq!(
        bounds, expected,
        "a newly added dirty line with no previous entry must use only its current bounds"
    );
}

#[test]
fn logical_rect_to_physical_scissor_scales_by_dpi_factor() {
    let rect = Rect::new(10.0, 20.0, 30.0, 40.0);
    let scissor = logical_rect_to_physical_scissor(rect, 2.0, 10_000, 10_000);
    assert_eq!(scissor, (20, 40, 60, 80));
}

#[test]
fn logical_rect_to_physical_scissor_clamps_to_target_bounds() {
    // A rect that overshoots the physical target (e.g. from rounding,
    // or a heuristic text bound near the edge of the window) must be
    // clamped, wgpu rejects a scissor rect that exceeds the target.
    let rect = Rect::new(90.0, 90.0, 50.0, 50.0);
    let (scissor_x, scissor_y, scissor_w, scissor_h) =
        logical_rect_to_physical_scissor(rect, 1.0, 100, 100);
    assert_eq!(scissor_x, 90);
    assert_eq!(scissor_y, 90);
    assert_eq!(scissor_w, 10, "clamped to max_w - x");
    assert_eq!(scissor_h, 10, "clamped to max_h - y");
}

#[test]
fn logical_rect_to_physical_scissor_handles_origin_outside_bounds() {
    // A rect entirely past the target's edge collapses to a zero-size
    // scissor rather than going negative-width.
    let r = Rect::new(200.0, 200.0, 50.0, 50.0);
    let (_, _, w, h) = logical_rect_to_physical_scissor(r, 1.0, 100, 100);
    assert_eq!(w, 0);
    assert_eq!(h, 0);
}

#[test]
fn needs_full_redraw_true_when_sticky_flag_set() {
    assert!(needs_full_redraw_this_frame(true, 3, 3, 3, 3));
}

#[test]
fn needs_full_redraw_false_when_nothing_changed_and_not_sticky() {
    assert!(!needs_full_redraw_this_frame(false, 3, 3, 3, 3));
}

#[test]
fn needs_full_redraw_true_when_instance_count_changes() {
    assert!(needs_full_redraw_this_frame(false, 3, 4, 3, 3));
    assert!(needs_full_redraw_this_frame(false, 4, 3, 3, 3));
}

#[test]
fn needs_full_redraw_true_when_text_count_changes() {
    assert!(needs_full_redraw_this_frame(false, 3, 3, 2, 3));
    assert!(needs_full_redraw_this_frame(false, 3, 3, 3, 2));
}

// ── compute_scissor ───────────────────────────────────────────────────────
//
// `encode_frame` never calls `dirty_text_bounds` or
// `logical_rect_to_physical_scissor` directly on an incremental frame,
// it goes through `compute_scissor`, so the composition of the two
// (including the zero-size-rect rejection) needs its own coverage, not
// just each half in isolation.

#[test]
fn compute_scissor_is_none_when_nothing_is_dirty() {
    let texts = [line(0.0, 0.0, "a", 16.0, false)];
    assert!(compute_scissor(&ScissorInputs::text_only(&texts, &[]), 1.0, 1000, 1000).is_none());
}

#[test]
fn compute_scissor_is_none_for_empty_texts() {
    assert!(compute_scissor(&ScissorInputs::text_only(&[], &[]), 1.0, 1000, 1000).is_none());
}

#[test]
fn compute_scissor_returns_physical_rect_for_a_dirty_line() {
    let texts = [line(10.0, 20.0, "hello", 16.0, true)];
    let (bounds, x, y, w, h) =
        compute_scissor(&ScissorInputs::text_only(&texts, &[]), 2.0, 1000, 1000).unwrap();
    let expected_bounds = text_line_bounds(&texts[0]);
    assert_eq!(
        bounds,
        inflate(expected_bounds, AA_MARGIN_PX),
        "logical bounds must be unscaled, and grown by the antialiasing margin"
    );
    let (expected_x, expected_y, expected_w, expected_h) =
        logical_rect_to_physical_scissor(inflate(expected_bounds, AA_MARGIN_PX), 2.0, 1000, 1000);
    assert_eq!(
        (x, y, w, h),
        (expected_x, expected_y, expected_w, expected_h)
    );
}

#[test]
// `previous_bounds.width.ceil() as u32` mirrors the same lossless-in-practice
// cast already allowed in `logical_rect_to_physical_scissor` (no real text
// bound is anywhere near 2^24 logical pixels wide).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn compute_scissor_unions_with_previous_bounds() {
    // Mirrors `dirty_text_bounds_unions_with_previous_frame_bounds`, but
    // through the full `compute_scissor` path that `encode_frame`
    // actually calls, a shrinking line's stale footprint must still be
    // covered by the resulting *physical* scissor rect, not just the
    // logical bounds in isolation.
    let shrunk = line(0.0, 0.0, "a", 16.0, true);
    let previous_bounds = text_line_bounds(&line(0.0, 0.0, "a much longer string", 16.0, false));
    let texts = [shrunk];
    let (bounds, _, _, w, _) = compute_scissor(
        &ScissorInputs::text_only(&texts, &[previous_bounds]),
        1.0,
        1000,
        1000,
    )
    .unwrap();
    assert!(
        bounds.width >= previous_bounds.width,
        "logical union must retain the previous (wider) footprint"
    );
    assert!(
        w >= previous_bounds.width.ceil() as u32,
        "physical scissor width must be wide enough to cover the stale footprint"
    );
}

#[test]
fn compute_scissor_is_none_when_dirty_rect_lies_entirely_outside_target() {
    // The dirty bounds are non-empty but fall entirely past the
    // physical target's edge, so `logical_rect_to_physical_scissor`
    // collapses them to a zero-size rect, wgpu rejects a zero-size
    // scissor, so `compute_scissor` must surface `None` rather than a
    // degenerate `Some((..., 0, 0))`.
    let texts = [line(2000.0, 2000.0, "offscreen", 16.0, true)];
    assert!(compute_scissor(&ScissorInputs::text_only(&texts, &[]), 1.0, 100, 100).is_none());
}

// ── M26/M27: box / decorated / texture dirty bounds + combined scissor ────

/// Builds a `BoxInstance` at `(x, y, w, h)` (colour/radii irrelevant to
/// the bounds helpers under test).
fn box_at(x: f32, y: f32, w: f32, h: f32) -> BoxInstance {
    BoxInstance {
        rect: [x, y, w, h],
        color: [0.0; 4],
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    }
}

fn decorated_at(x: f32, y: f32, w: f32, h: f32, dirty: bool) -> crate::frame::DecoratedBox {
    crate::frame::DecoratedBox {
        base: box_at(x, y, w, h),
        dirty,
        ..Default::default()
    }
}

fn texture_at(x: f32, y: f32, w: f32, h: f32, dirty: bool) -> crate::frame::TextureSampler {
    crate::frame::TextureSampler {
        rect: [x, y, w, h],
        src: String::new(),
        fit: crate::frame::ImageFit::Fill,
        radii: [0.0; 4],
        opacity: 1.0,
        dirty,
        smooth: 0.0,
    }
}

#[test]
fn dirty_box_bounds_is_none_when_nothing_is_dirty() {
    let boxes = [box_at(0.0, 0.0, 10.0, 10.0), box_at(50.0, 50.0, 10.0, 10.0)];
    assert!(dirty_box_bounds(&boxes, &[false, false], &[]).is_none());
}

#[test]
fn dirty_box_bounds_ignores_non_dirty_boxes() {
    let boxes = [
        box_at(10.0, 10.0, 20.0, 20.0),
        box_at(900.0, 900.0, 5.0, 5.0),
    ];
    let bounds = dirty_box_bounds(&boxes, &[true, false], &[]).unwrap();
    assert_eq!(
        bounds,
        rect_of(boxes[0].rect),
        "a clean box must not widen the union"
    );
}

#[test]
fn dirty_box_bounds_unions_with_previous_frame_bounds() {
    // A box that shrinks between frames: its old (wider) footprint must
    // still be covered, mirroring `dirty_text_bounds_unions_with_previous_*`.
    let shrunk = box_at(0.0, 0.0, 10.0, 10.0);
    let previous = Rect::new(0.0, 0.0, 200.0, 200.0);
    let bounds = dirty_box_bounds(std::slice::from_ref(&shrunk), &[true], &[previous]).unwrap();
    assert_eq!(bounds, rect_of(shrunk.rect).union(&previous));
    assert!(bounds.width >= previous.width);
}

#[test]
fn compute_scissor_is_some_when_only_a_box_is_dirty_and_no_text_exists() {
    // The M26 regression test: no text at all, one dirty box. The old
    // text-only `compute_scissor` returned `None` here, so `should_draw`
    // was false and the box mutation never reached the screen.
    let boxes = [box_at(10.0, 20.0, 30.0, 40.0)];
    let inputs = ScissorInputs {
        instances: &boxes,
        instances_dirty: &[true],
        ..ScissorInputs::text_only(&[], &[])
    };
    let scissor = compute_scissor(&inputs, 1.0, 1000, 1000);
    assert!(
        scissor.is_some(),
        "a box-only mutation must produce a non-empty scissor"
    );
    let (bounds, ..) = scissor.unwrap();
    assert_eq!(bounds, inflate(rect_of(boxes[0].rect), AA_MARGIN_PX));
}

#[test]
fn compute_scissor_unions_box_and_text_dirty_regions() {
    // One dirty box far from one dirty text line; the scissor must cover
    // both regions, not just one.
    let texts = [line(0.0, 0.0, "a", 16.0, true)];
    let boxes = [box_at(500.0, 500.0, 40.0, 40.0)];
    let inputs = ScissorInputs {
        instances: &boxes,
        instances_dirty: &[true],
        ..ScissorInputs::text_only(&texts, &[])
    };
    let (bounds, ..) = compute_scissor(&inputs, 1.0, 1000, 1000).unwrap();
    let expected = text_line_bounds(&texts[0]).union(&rect_of(boxes[0].rect));
    assert_eq!(bounds, inflate(expected, AA_MARGIN_PX));
}

#[test]
fn a_wrapped_line_reports_bounds_several_lines_tall() {
    // The dirty union's height for a paragraph used to be one line's worth
    // whatever the paragraph said, so the tail of every wrapped `Text` sat
    // outside the scissor. Harmless while the union covered the frame; a
    // stale-glyph defect the moment it stopped.
    let paragraph = line(0.0, 0.0, &"word ".repeat(30), 12.0, true);
    let single = text_line_bounds_wrapped(&paragraph, None);
    let wrapped = text_line_bounds_wrapped(&paragraph, Some(120.0));
    assert!(
        wrapped.height > single.height * 2.0,
        "a paragraph wrapped to 120px must report several lines of height, \
             got {} vs the single-line {}",
        wrapped.height,
        single.height
    );
    assert!(
        wrapped.width <= 120.0,
        "and no more width than it was offered"
    );
}

#[test]
fn a_shadow_grows_a_decorated_boxs_dirty_bounds() {
    // A drop shadow paints outside the box it belongs to by construction,
    // so a union over `base.rect` alone leaves the shadow's outer half on
    // screen when the box moves.
    let mut d = decorated_at(100.0, 100.0, 50.0, 50.0, true);
    d.shadow_color = [0.0, 0.0, 0.0, 0.5];
    d.shadow_dx = 4.0;
    d.shadow_dy = 6.0;
    d.shadow_blur = 8.0;
    d.shadow_spread = 2.0;
    let bounds = decorated_paint_bounds(&d);
    assert!(
        bounds.x <= 90.0,
        "left edge must clear the blur: {bounds:?}"
    );
    assert!(
        bounds.x + bounds.width >= 164.0,
        "right edge must clear offset + blur + spread: {bounds:?}"
    );
    assert!(bounds.y + bounds.height >= 166.0, "{bounds:?}");
}

#[test]
fn a_transparent_shadow_does_not_grow_the_bounds() {
    // The other direction: an unset shadow must not inflate every
    // decoration's dirty region by its default blur.
    let d = decorated_at(100.0, 100.0, 50.0, 50.0, true);
    assert_eq!(decorated_paint_bounds(&d), rect_of(d.base.rect));
}

#[test]
fn the_scissor_covers_the_antialiased_fringe() {
    // Every analytic pipeline here softens its edge over about half a
    // pixel. A scissor cut exactly at a primitive's rect clips that
    // fringe and leaves a one-pixel halo of the previous frame around
    // anything that moves, which is what a golden-image parity run
    // against a full redraw actually catches.
    let boxes = [box_at(100.0, 100.0, 40.0, 40.0)];
    let inputs = ScissorInputs {
        instances: &boxes,
        instances_dirty: &[true],
        ..ScissorInputs::text_only(&[], &[])
    };
    let (bounds, ..) = compute_scissor(&inputs, 1.0, 1000, 1000).unwrap();
    assert!(bounds.x < 100.0 && bounds.y < 100.0);
    assert!(bounds.x + bounds.width > 140.0);
    assert!(bounds.y + bounds.height > 140.0);
}

#[test]
fn dirty_texture_bounds_is_none_when_nothing_is_dirty() {
    let textures = [texture_at(0.0, 0.0, 10.0, 10.0, false)];
    assert!(dirty_texture_bounds(&textures, &[]).is_none());
}

#[test]
fn dirty_texture_bounds_ignores_non_dirty_textures() {
    let textures = [
        texture_at(10.0, 10.0, 20.0, 20.0, true),
        texture_at(900.0, 900.0, 5.0, 5.0, false),
    ];
    let bounds = dirty_texture_bounds(&textures, &[]).unwrap();
    assert_eq!(bounds, rect_of(textures[0].rect));
}

#[test]
fn dirty_texture_bounds_unions_with_previous_frame_bounds() {
    let shrunk = texture_at(0.0, 0.0, 10.0, 10.0, true);
    let previous = Rect::new(0.0, 0.0, 200.0, 200.0);
    let bounds = dirty_texture_bounds(std::slice::from_ref(&shrunk), &[previous]).unwrap();
    assert_eq!(bounds, rect_of(shrunk.rect).union(&previous));
}

#[test]
fn compute_scissor_does_not_force_full_redraw_when_a_clean_decorated_box_is_present() {
    // The actual point of M27: a scene with one *non-dirty* DecoratedBox
    // and one dirty text line must scissor to the text's bounds only, not
    // the whole viewport (which is what the old forced-`full_redraw` block,
    // now deleted, effectively did).
    let texts = [line(10.0, 20.0, "hi", 16.0, true)];
    let decorated = [decorated_at(0.0, 0.0, 999.0, 999.0, false)];
    let inputs = ScissorInputs {
        decorated: &decorated,
        ..ScissorInputs::text_only(&texts, &[])
    };
    let (bounds, ..) = compute_scissor(&inputs, 1.0, 1000, 1000).unwrap();
    assert_eq!(
        bounds,
        inflate(text_line_bounds(&texts[0]), AA_MARGIN_PX),
        "a clean decorated box must not expand the scissor"
    );
}

#[test]
fn compute_scissor_includes_a_dirty_decorated_box() {
    let decorated = [decorated_at(100.0, 100.0, 50.0, 50.0, true)];
    let inputs = ScissorInputs {
        decorated: &decorated,
        ..ScissorInputs::text_only(&[], &[])
    };
    let (bounds, ..) = compute_scissor(&inputs, 1.0, 1000, 1000).unwrap();
    assert_eq!(
        bounds,
        inflate(rect_of(decorated[0].base.rect), AA_MARGIN_PX)
    );
}
