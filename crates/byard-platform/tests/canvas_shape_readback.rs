//! GPU readback proofs for the `CanvasShape` pipeline (RFC-0020 Tier 1).
//!
//! Renders synthetic frames of programmatic shapes and reads pixels back, so
//! the analytic-SDF fragment shader's stroke/fill/sweep behaviour is pinned
//! down on a real device, the CPU-mirror tests in `byard-core` cover the
//! same geometry deterministically without a GPU.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{
    CANVAS_CAP_ROUND, CANVAS_SHAPE_ARC, CANVAS_SHAPE_CIRCLE, CANVAS_SHAPE_RECT, CanvasShape,
    RenderFrame, Viewport,
};
use std::sync::Arc;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("canvas-shape readback device"),
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

/// A read-back framebuffer: physical-pixel BGRA bytes plus the row stride.
struct Readback {
    data: Vec<u8>,
    bpr: u32,
    scale: f32,
}

impl Readback {
    /// Samples the BGRA pixel at a *logical* coordinate as `(b, g, r, a)`.
    fn at(&self, lx: f32, ly: f32) -> (u8, u8, u8, u8) {
        let px = (lx * self.scale) as u32;
        let py = (ly * self.scale) as u32;
        let idx = (py * self.bpr + px * 4) as usize;
        (
            self.data[idx],
            self.data[idx + 1],
            self.data[idx + 2],
            self.data[idx + 3],
        )
    }
}

/// Encodes `frame` into an off-screen target and reads the whole thing back.
fn render(
    device: &Arc<wgpu::Device>,
    queue: &Arc<wgpu::Queue>,
    frame: &RenderFrame,
    logical_w: f32,
    logical_h: f32,
) -> Readback {
    let scale = 2.0_f32;
    let phys_w = (logical_w * scale) as u32;
    let phys_h = (logical_h * scale) as u32;
    let fmt = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        fmt,
        scale,
        phys_w,
        phys_h,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: logical_w,
            height: logical_h,
        },
        phys_w,
        phys_h,
        scale,
    );

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("canvas-shape target"),
        size: wgpu::Extent3d {
            width: phys_w,
            height: phys_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: fmt,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let cmd = enc.encode_frame_from_relay(&target, frame).unwrap();
    queue.submit(std::iter::once(cmd));

    let bpr = 256 * (phys_w * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("canvas-shape readback"),
        size: u64::from(bpr) * u64::from(phys_h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut ce = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    ce.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(phys_h),
            },
        },
        wgpu::Extent3d {
            width: phys_w,
            height: phys_h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range().to_vec();
    Readback { data, bpr, scale }
}

/// RFC-0031 §S4: a group head draws from the **shape-record pool**, not from
/// its own `params`.
///
/// The load-bearing claim of the structural milestone, and the one no CPU test
/// can make: the records have to reach the GPU, at the right element index, and
/// be readable from the fragment stage. The head is given deliberately absurd
/// `params`, a zero-radius circle at the origin, so anything that painted
/// from the instance instead of from the buffer would paint nothing at all.
#[test]
#[allow(clippy::many_single_char_names)]
fn a_group_head_draws_its_member_record_and_not_its_own_params() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-shape readback");
        return;
    };

    let (w, h) = (200.0_f32, 200.0_f32);
    let member = byard_core::frame::ShapeRecord::from_shape(&CanvasShape {
        kind: CANVAS_SHAPE_CIRCLE,
        params: [100.0, 100.0, 50.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        fill_color: [0.0, 1.0, 0.0, 1.0],
        ..CanvasShape::default()
    });
    let mut frame = RenderFrame::new();
    frame.push_shape_group(
        CanvasShape {
            kind: CANVAS_SHAPE_CIRCLE,
            // §S4: a head's `params` size its *quad*, not its shape, here the
            // union of its members' bounds, ten px larger than the member and
            // in a colour the member does not use, so painting from the head
            // instead of from the record is visible twice over.
            params: [100.0, 100.0, 60.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            group_mode: byard_core::frame::GROUP_FUSE,
            group_param: 0.0,
            fill_color: [1.0, 0.0, 1.0, 1.0],
            stroke_color: [0.0; 4],
            ..CanvasShape::default()
        },
        &[member],
    );
    let rb = render(&device, &queue, &frame, w, h);

    // Inside the member circle: filled, in the *member's* colour.
    let (b, g, r, a) = rb.at(100.0, 100.0);
    assert!(a > 200, "the member's fill must paint, got alpha {a}");
    assert!(
        g > 200 && r < 60 && b < 60,
        "the colour must come from the record, not from the head, got BGR=({b},{g},{r})"
    );
    // Between the member's radius (50) and the head's (60): inside the quad,
    // outside the shape. A head painting itself would fill this magenta.
    let (_, _, _, oa) = rb.at(100.0, 45.0);
    assert!(
        oa < 10,
        "the head's own geometry must not paint, got alpha {oa}"
    );
}

/// RFC-0031 §"`ngon`": `r` is the circumradius, exactly, whatever `corner` is,
/// and `inner` pulls the notches in without moving the points.
///
/// The exactness matters beyond tidiness: two `ngon`s of the same `r` morph
/// into each other without the pair appearing to breathe, which is the whole
/// reason a shape set can share one radius.
#[test]
#[allow(clippy::many_single_char_names)]
fn an_ngon_reaches_its_circumradius_at_its_points_and_its_inner_ratio_between() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-shape readback");
        return;
    };
    let (w, h) = (200.0_f32, 200.0_f32);
    let ngon = |n: f32, inner: f32, corner: f32| {
        let mut frame = RenderFrame::new();
        frame.push_canvas_shape(CanvasShape {
            kind: byard_core::frame::CANVAS_SHAPE_NGON,
            params: [100.0, 100.0, 60.0, corner, inner, 0.0, n, 0.0],
            fill_color: [1.0, 1.0, 1.0, 1.0],
            stroke_color: [0.0; 4],
            ..CanvasShape::default()
        });
        render(&device, &queue, &frame, w, h)
    };

    // A vertex points straight up, so the reach along -Y is the circumradius.
    // Sampled just inside and just outside it.
    for corner in [0.0_f32, 12.0] {
        let rb = ngon(5.0, 1.0, corner);
        let (_, _, _, inside) = rb.at(100.0, 100.0 - 57.0);
        let (_, _, _, outside) = rb.at(100.0, 100.0 - 63.0);
        assert!(
            inside > 200,
            "corner {corner}: the point must reach r = 60, got alpha {inside}"
        );
        assert!(
            outside < 20,
            "corner {corner}: and not past it, got alpha {outside}"
        );
    }

    // `inner` moves the notch, not the point. The notch of a pentagon sits at
    // 36° from a vertex; with `inner: 0.4` its radius is 0.4·r·cos(36°) ≈ 19.4.
    let star = ngon(5.0, 0.4, 0.0);
    let notch = 36.0_f32.to_radians();
    let dir = [notch.sin(), -notch.cos()];
    let near = star.at(100.0 + dir[0] * 14.0, 100.0 + dir[1] * 14.0).3;
    let far = star.at(100.0 + dir[0] * 26.0, 100.0 + dir[1] * 26.0).3;
    assert!(near > 200, "inside the notch must be filled, got {near}");
    assert!(far < 20, "outside the notch must be clear, got {far}");
    // …while the vertex is unmoved.
    assert!(star.at(100.0, 100.0 - 57.0).3 > 200);
}

/// §S10: `morph` blends the *fields* of two members, and its phase **wraps**.
///
/// Three claims in one frame each, because they are the three ways the feature
/// can be wrong: an endpoint that is not the shape drawn alone, a midpoint that
/// is not between them, and a sweep that stalls on the last shape instead of
/// returning to the first (§Q9, the Material 3 loader is a loop).
#[test]
#[allow(clippy::many_single_char_names)]
fn a_morph_reaches_its_endpoints_blends_between_them_and_wraps() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-shape readback");
        return;
    };
    let (w, h) = (200.0_f32, 200.0_f32);
    // Deliberately two *different kinds*: a circle and a seven-pointed star.
    // §S9's whole claim is that field interpolation asks nothing of the shapes
    // being related.
    let member = |shape: CanvasShape| byard_core::frame::ShapeRecord::from_shape(&shape);
    let circle = CanvasShape {
        kind: CANVAS_SHAPE_CIRCLE,
        params: [100.0, 100.0, 60.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        fill_color: [1.0, 1.0, 1.0, 1.0],
        ..CanvasShape::default()
    };
    let star = CanvasShape {
        kind: byard_core::frame::CANVAS_SHAPE_NGON,
        params: [100.0, 100.0, 60.0, 4.0, 0.5, 0.0, 7.0, 0.0],
        fill_color: [1.0, 1.0, 1.0, 1.0],
        ..CanvasShape::default()
    };
    let at_phase = |phase: f32| {
        let mut frame = RenderFrame::new();
        frame.push_shape_group(
            CanvasShape {
                kind: CANVAS_SHAPE_RECT,
                params: [20.0, 20.0, 160.0, 160.0, 0.0, 0.0, 0.0, 0.0],
                fill_color: [1.0, 1.0, 1.0, 1.0],
                stroke_color: [0.0; 4],
                group_mode: byard_core::frame::GROUP_MORPH,
                group_param: phase,
                ..CanvasShape::default()
            },
            &[member(circle.clone()), member(star.clone())],
        );
        render(&device, &queue, &frame, w, h)
    };
    // The notch direction of the seven-pointed star, where the two shapes
    // differ most: the circle reaches 60, the star about 0.5·60·cos(π/7) ≈ 27.
    let notch = std::f32::consts::PI / 7.0;
    let dir = [notch.sin(), -notch.cos()];
    let reach = |rb: &Readback| {
        let mut t = 10.0_f32;
        while t < 90.0 {
            if rb.at(100.0 + dir[0] * t, 100.0 + dir[1] * t).3 < 128 {
                return t;
            }
            t += 0.25;
        }
        t
    };

    let circle_alone = {
        let mut frame = RenderFrame::new();
        frame.push_canvas_shape(circle.clone());
        reach(&render(&device, &queue, &frame, w, h))
    };
    let star_alone = {
        let mut frame = RenderFrame::new();
        frame.push_canvas_shape(star.clone());
        reach(&render(&device, &queue, &frame, w, h))
    };
    assert!(
        circle_alone > star_alone + 10.0,
        "the two members must differ where this test measures ({circle_alone} vs {star_alone})"
    );

    // t = 0 and t = 1 are the endpoint shapes drawn alone.
    let at0 = reach(&at_phase(0.0));
    let at1 = reach(&at_phase(1.0));
    assert!(
        (at0 - circle_alone).abs() < 1.0,
        "phase 0 must be the first member: {at0} vs {circle_alone}"
    );
    assert!(
        (at1 - star_alone).abs() < 1.0,
        "phase 1 must be the second member: {at1} vs {star_alone}"
    );
    // …and the midpoint is strictly between them, which a snap would not be.
    let mid = reach(&at_phase(0.5));
    assert!(
        mid > star_alone + 2.0 && mid < circle_alone - 2.0,
        "phase 0.5 must be between the two shapes, got {mid}"
    );

    // §Q9: the phase wraps at `count`, so sweeping 0 → 2 returns to the first
    // shape rather than stalling on the last.
    let wrapped = reach(&at_phase(2.0));
    assert!(
        (wrapped - at0).abs() < 1.0,
        "phase 2 of a 2-member group must be phase 0 again: {wrapped} vs {at0}"
    );
    // Halfway back is the mirror of halfway out.
    let back = reach(&at_phase(1.5));
    assert!(
        (back - mid).abs() < 2.0,
        "the return leg must blend the same way: {back} vs {mid}"
    );
    // A negative phase wraps too, an animation with a negative `from:` must
    // not fall off the front of the sequence.
    let negative = reach(&at_phase(-1.0));
    assert!(
        (negative - at1).abs() < 1.0,
        "phase -1 of a 2-member group is phase 1: {negative} vs {at1}"
    );
}

/// §S7–§S8: two circles that bridge as they approach, blending colour by the
/// same factor that produced the geometry.
///
/// Four claims, in the order they can go wrong:
///
/// 1. **`fuse: 0` is ungrouped.** A zero smoothing radius degenerates to a hard
///    union, which is exactly the two shapes drawn separately (INV-22).
/// 2. **Far apart, nothing bridges.** Fusion is local; two circles at opposite
///    ends of a canvas must not grow a bar between them.
/// 3. **Close, they bridge.** The midpoint between two shapes that neither
///    touches becomes solid.
/// 4. **Colour crosses the bridge.** Differently-coloured members blend through
///    it, which is what makes fusion look deliberate rather than like
///    z-fighting.
#[test]
#[allow(clippy::many_single_char_names)]
fn fusion_bridges_nearby_shapes_and_carries_their_colours_across() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-shape readback");
        return;
    };
    let (w, h) = (240.0_f32, 120.0_f32);
    let disc = |cx: f32, fill: [f32; 4]| {
        byard_core::frame::ShapeRecord::from_shape(&CanvasShape {
            kind: CANVAS_SHAPE_CIRCLE,
            params: [cx, 60.0, 24.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            fill_color: fill,
            ..CanvasShape::default()
        })
    };
    let red = [1.0, 0.0, 0.0, 1.0];
    let blue = [0.0, 0.0, 1.0, 1.0];
    let fused = |left: f32, right: f32, k: f32| {
        let mut frame = RenderFrame::new();
        frame.push_shape_group(
            CanvasShape {
                kind: CANVAS_SHAPE_RECT,
                params: [0.0, 0.0, 240.0, 120.0, 0.0, 0.0, 0.0, 0.0],
                fill_color: red,
                stroke_color: [0.0; 4],
                group_mode: byard_core::frame::GROUP_FUSE,
                group_param: k,
                ..CanvasShape::default()
            },
            &[disc(left, red), disc(right, blue)],
        );
        render(&device, &queue, &frame, w, h)
    };

    // 1. `fuse: 0`, the gap between two separated circles stays empty.
    let unfused = fused(70.0, 170.0, 0.0);
    assert!(
        unfused.at(120.0, 60.0).3 < 20,
        "fuse: 0 must be the plain union, got alpha {}",
        unfused.at(120.0, 60.0).3
    );
    assert!(unfused.at(70.0, 60.0).3 > 200 && unfused.at(170.0, 60.0).3 > 200);

    // 2. The same generous `k`, but the shapes are far apart: still no bridge.
    //    Fusion is local, and a group that bridged across a whole canvas would
    //    be unusable.
    let distant = fused(40.0, 200.0, 32.0);
    assert!(
        distant.at(120.0, 60.0).3 < 20,
        "widely separated shapes must not fuse, got alpha {}",
        distant.at(120.0, 60.0).3
    );

    // 3. Brought within reach of a `k` that spans the gap, the midpoint fills
    //, although neither circle covers it: their edges are 12 px apart.
    let (left, right) = (90.0_f32, 150.0);
    let bridged = fused(left, right, 32.0);
    let mid = bridged.at(120.0, 60.0);
    assert!(
        mid.3 > 200,
        "shapes within the smoothing radius must bridge, got alpha {}",
        mid.3
    );
    // The same pair unfused leaves that point empty, so the bridge is the
    // fusion's doing and not the circles overlapping.
    assert!(
        fused(left, right, 0.0).at(120.0, 60.0).3 < 20,
        "the test point must be outside both circles"
    );

    // 4. …and the bridge carries the colour across. Red on the left, blue on
    //    the right, something between them in the middle.
    let (lb, _, lr, _) = bridged.at(left, 60.0);
    let (rb, _, rr, _) = bridged.at(right, 60.0);
    let (mb, _, mr, _) = mid;
    assert!(lr > 200 && lb < 60, "the left body stays red");
    assert!(rb > 200 && rr < 60, "the right body stays blue");
    assert!(
        i32::from(mr) < i32::from(lr) && i32::from(mb) > i32::from(lb),
        "the bridge must blend towards the far member (BGR mid = ({mb}, _, {mr}))"
    );
}

/// §S8: a fused stroke is the outline of the **union**, not a union of the
/// members' outlines.
///
/// The difference is the whole point: per-member strokes would draw the seams
/// through the interior of a body whose entire purpose is not to have any.
#[test]
#[allow(clippy::many_single_char_names)]
fn a_fused_stroke_outlines_the_union_and_not_its_members() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-shape readback");
        return;
    };
    let (w, h) = (240.0_f32, 120.0_f32);
    // Two circles that genuinely overlap, so each one's own outline would run
    // straight through the other's interior.
    let disc = |cx: f32| {
        byard_core::frame::ShapeRecord::from_shape(&CanvasShape {
            kind: CANVAS_SHAPE_CIRCLE,
            params: [cx, 60.0, 30.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            fill_color: [0.0; 4],
            stroke_color: [1.0, 1.0, 1.0, 1.0],
            stroke_width: 4.0,
            ..CanvasShape::default()
        })
    };
    let mut frame = RenderFrame::new();
    frame.push_shape_group(
        CanvasShape {
            kind: CANVAS_SHAPE_RECT,
            params: [0.0, 0.0, 240.0, 120.0, 0.0, 0.0, 0.0, 0.0],
            fill_color: [0.0; 4],
            stroke_color: [1.0, 1.0, 1.0, 1.0],
            stroke_width: 4.0,
            group_mode: byard_core::frame::GROUP_FUSE,
            group_param: 10.0,
            ..CanvasShape::default()
        },
        &[disc(100.0), disc(140.0)],
    );
    let rb = render(&device, &queue, &frame, w, h);

    // The outer boundary of the union is stroked.
    assert!(
        rb.at(100.0 - 30.0 + 1.0, 60.0).3 > 150,
        "the union's left edge must be stroked"
    );
    // The interior, where each member's own circle would have run, is not.
    // (140 − 30 = 110 is the right circle's left edge, deep inside the left
    // circle, which reaches 130.)
    let seam = rb.at(110.0, 60.0);
    assert!(
        seam.3 < 40,
        "a member's own outline must not run through the fused body, got alpha {}",
        seam.3
    );
    // …and neither is the centre.
    assert!(
        rb.at(120.0, 60.0).3 < 40,
        "the fused body is hollow, not seamed"
    );
}

/// §Q8: a morph's colour blends in **`OKLab`**, matching RFC-0025's keyframes and
/// RFC-0010's `bg`/`color` transitions.
///
/// The claim is testable because the two spaces disagree measurably: the
/// midpoint of red → blue is a different colour in `OKLab` than a component-wise
/// linear mix, and RFC-0031 §Q8's stated reason for choosing one is that a morph
/// blending in the other beside a `with` clause blending the same two colours
/// would disagree *visibly inside one frame*.
#[test]
#[allow(clippy::many_single_char_names)]
fn a_morphs_colour_blends_in_oklab_not_linearly() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-shape readback");
        return;
    };
    let (w, h) = (200.0_f32, 200.0_f32);
    let disc = |fill: [f32; 4]| CanvasShape {
        kind: CANVAS_SHAPE_CIRCLE,
        params: [100.0, 100.0, 60.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        fill_color: fill,
        stroke_color: [0.0; 4],
        ..CanvasShape::default()
    };
    let red = [1.0, 0.0, 0.0, 1.0];
    let blue = [0.0, 0.0, 1.0, 1.0];
    let mut frame = RenderFrame::new();
    frame.push_shape_group(
        CanvasShape {
            kind: CANVAS_SHAPE_RECT,
            params: [30.0, 30.0, 140.0, 140.0, 0.0, 0.0, 0.0, 0.0],
            fill_color: red,
            stroke_color: [0.0; 4],
            group_mode: byard_core::frame::GROUP_MORPH,
            group_param: 0.5,
            ..CanvasShape::default()
        },
        &[
            byard_core::frame::ShapeRecord::from_shape(&disc(red)),
            byard_core::frame::ShapeRecord::from_shape(&disc(blue)),
        ],
    );
    let rb = render(&device, &queue, &frame, w, h);
    let (b, g, r, a) = rb.at(100.0, 100.0);
    assert!(a > 200, "the blended disc must paint, got alpha {a}");

    // The two spaces give measurably different answers, which is what makes
    // this assertion mean anything. Halfway from red to blue:
    //   linear:  sRGB (188,   0, 188), equal channels, no green at all
    //   OKLab:   sRGB (140,  83, 162), darker, leaning blue, with real green
    // Green is the sharpest separator: a component-wise mix of pure red and
    // pure blue has *exactly* none.
    assert!(
        g > 50,
        "an OKLab blend of red→blue passes through a real purple; a linear mix \
         would have no green at all (BGR = ({b}, {g}, {r}))"
    );
    assert!(
        r < 170 && b < 190,
        "…and is darker than the linear midpoint's 188 (BGR = ({b}, {g}, {r}))"
    );
    assert!(
        i32::from(r) < i32::from(b),
        "…and leans towards the blue endpoint (BGR = ({b}, {g}, {r}))"
    );
}

/// A stroked circle paints its ring and leaves its interior untouched.
#[test]
#[allow(clippy::many_single_char_names)]
fn circle_stroke_paints_the_ring_and_not_the_interior() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-shape readback");
        return;
    };

    let (w, h) = (200.0_f32, 200.0_f32);
    let mut frame = RenderFrame::new();
    frame.push_canvas_shape(CanvasShape {
        kind: CANVAS_SHAPE_CIRCLE,
        params: [100.0, 100.0, 50.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        stroke_color: [1.0, 0.0, 0.0, 1.0],
        stroke_width: 8.0,
        ..CanvasShape::default()
    });
    let rb = render(&device, &queue, &frame, w, h);

    // On the ring (east point): strongly red.
    let (b, g, r, a) = rb.at(150.0, 100.0);
    assert!(a > 200, "ring pixel must be opaque, got alpha {a}");
    assert!(
        r > 200 && g < 60 && b < 60,
        "ring pixel must be red, got BGR=({b},{g},{r})"
    );
    // The centre is untouched (no fill).
    let (_, _, _, ca) = rb.at(100.0, 100.0);
    assert!(ca < 10, "unfilled interior must stay clear, got alpha {ca}");
    // Well outside the ring is untouched too.
    let (_, _, _, oa) = rb.at(10.0, 10.0);
    assert!(oa < 10, "outside pixel must stay clear, got alpha {oa}");
}

/// A 90° arc covers its swept quadrant and leaves the opposite one empty; a
/// filled rect covers its interior.
#[test]
#[allow(clippy::many_single_char_names)]
fn arc_sweep_and_rect_fill_cover_exactly_their_regions() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-shape readback");
        return;
    };

    let (w, h) = (200.0_f32, 200.0_f32);
    let mut frame = RenderFrame::new();
    // Quarter arc: 0°..90° sweeps the +X → +Y quadrant (screen-space
    // clockwise from east through south).
    frame.push_canvas_shape(CanvasShape {
        kind: CANVAS_SHAPE_ARC,
        params: [
            100.0,
            100.0,
            60.0,
            0.0,
            std::f32::consts::FRAC_PI_2,
            0.0,
            0.0,
            0.0,
        ],
        stroke_color: [0.0, 1.0, 0.0, 1.0],
        stroke_width: 10.0,
        cap: CANVAS_CAP_ROUND,
        ..CanvasShape::default()
    });
    // Filled blue rect in the top-left corner.
    frame.push_canvas_shape(CanvasShape {
        kind: CANVAS_SHAPE_RECT,
        params: [10.0, 10.0, 40.0, 30.0, 4.0, 0.0, 0.0, 0.0],
        fill_color: [0.0, 0.0, 1.0, 1.0],
        stroke_width: 0.0,
        ..CanvasShape::default()
    });
    let rb = render(&device, &queue, &frame, w, h);

    // 45° into the sweep: on the ring, green.
    let mid = 45f32.to_radians();
    let (b, g, r, a) = rb.at(100.0 + 60.0 * mid.cos(), 100.0 + 60.0 * mid.sin());
    assert!(
        a > 200 && g > 200 && r < 60 && b < 60,
        "in-sweep ring pixel must be green, got BGRA=({b},{g},{r},{a})"
    );
    // The un-swept west point of the same ring stays clear.
    let (_, _, _, wa) = rb.at(100.0 - 60.0, 100.0);
    assert!(
        wa < 10,
        "out-of-sweep ring pixel must stay clear, got alpha {wa}"
    );
    // Inside the filled rect: blue.
    let (fb, fg, fr, fa) = rb.at(30.0, 25.0);
    assert!(
        fa > 200 && fb > 200 && fr < 60 && fg < 60,
        "rect interior must be blue, got BGRA=({fb},{fg},{fr},{fa})"
    );
}
