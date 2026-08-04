//! GPU readback proofs for superelliptical corners (RFC-0031 §S1–S3).
//!
//! Three claims that only pixels can settle, because the frame data is
//! identical either way and it is the shader that gets them right or wrong:
//!
//! 1. **`smooth: 0` still draws a circular arc.** The CPU mirror in
//!    `byard-core` proves the *field* is bit-identical; this proves the field
//!    that ships is the one that was mirrored.
//! 2. **`smooth: 1` fills the corner the circle leaves empty**, and does so on
//!    a continuous-curvature profile rather than by growing the radius.
//! 3. **The corner's anti-aliased fringe is as wide as the edge's** (§S2). An
//!    uncorrected Lⁿ field draws it ~26 % wider, which reads as a smeared
//!    corner on exactly the shapes the property exists to improve.
//!
//! Plus §Q2: a shadow's silhouette follows its caster's profile.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use byard_core::BoxInstance;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{DecoratedBox, RenderFrame, Transform, Viewport};
use std::sync::Arc;

const LOGICAL: f32 = 160.0;
const SCALE: f32 = 4.0;

/// The probe box: a big radius on a big square, so the corner arc is many
/// pixels long and its profile is measurable rather than inferred.
const BOX_X: f32 = 30.0;
const BOX_Y: f32 = 30.0;
const BOX_SIDE: f32 = 100.0;
const RADIUS: f32 = 40.0;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("corner smoothing readback device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

struct Readback {
    data: Vec<u8>,
    bpr: u32,
}

impl Readback {
    /// Coverage `0..=1` at a logical point, read from the alpha channel.
    fn alpha(&self, lx: f32, ly: f32) -> f32 {
        let px = (lx * SCALE) as u32;
        let py = (ly * SCALE) as u32;
        let idx = (py * self.bpr + px * 4) as usize;
        f32::from(self.data[idx + 3]) / 255.0
    }

    /// Walks outward from `from` along the unit direction `dir` and returns the
    /// logical distance at which coverage first drops below `level`.
    fn edge_along(&self, from: [f32; 2], dir: [f32; 2], level: f32) -> f32 {
        let step = 1.0 / SCALE;
        let mut t = 0.0_f32;
        while t < 120.0 {
            let p = [from[0] + dir[0] * t, from[1] + dir[1] * t];
            if self.alpha(p[0], p[1]) < level {
                return t;
            }
            t += step;
        }
        t
    }
}

fn render(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>, frame: &RenderFrame) -> Readback {
    let phys = (LOGICAL * SCALE) as u32;
    let fmt = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        fmt,
        SCALE,
        phys,
        phys,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: LOGICAL,
            height: LOGICAL,
        },
        phys,
        phys,
        SCALE,
    );
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("corner smoothing target"),
        size: wgpu::Extent3d {
            width: phys,
            height: phys,
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

    let bpr = 256 * (phys * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("corner smoothing readback"),
        size: u64::from(bpr) * u64::from(phys),
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
                rows_per_image: Some(phys),
            },
        },
        wgpu::Extent3d {
            width: phys,
            height: phys,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    Readback { data, bpr }
}

/// One opaque white box with the given corner smoothing.
fn box_frame(smooth: f32) -> RenderFrame {
    let mut frame = RenderFrame::new();
    frame.push_instance(BoxInstance {
        rect: [BOX_X, BOX_Y, BOX_SIDE, BOX_SIDE],
        color: [1.0, 1.0, 1.0, 1.0],
        radii: [RADIUS; 4],
        transform: Transform::IDENTITY,
        smooth,
    });
    frame
}

/// The box centre, and the outward unit vector along the bottom-right diagonal.
const CENTRE: [f32; 2] = [BOX_X + BOX_SIDE / 2.0, BOX_Y + BOX_SIDE / 2.0];
const DIAGONAL: [f32; 2] = [std::f32::consts::FRAC_1_SQRT_2; 2];

#[test]
fn smooth_zero_draws_the_circular_corner_and_smooth_one_extends_it() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };

    // Where the circular corner's boundary crosses the diagonal: the corner
    // arc's centre is `radius` in from each edge, so the boundary sits at
    // `(half − radius)·√2 + radius` from the centre.
    let half = BOX_SIDE / 2.0;
    let circular = (half - RADIUS) * 2.0_f32.sqrt() + RADIUS;

    let flat = render(&device, &queue, &box_frame(0.0));
    let measured = flat.edge_along(CENTRE, DIAGONAL, 0.5);
    assert!(
        (measured - circular).abs() < 0.6,
        "smooth: 0 must still be the circular arc, expected {circular:.2}, got {measured:.2}"
    );

    // §S1: the squircle keeps the same straight edges and pushes the corner
    // outward towards the rect's own corner, which is at `half·√2`.
    let squircle = render(&device, &queue, &box_frame(1.0));
    let extended = squircle.edge_along(CENTRE, DIAGONAL, 0.5);
    assert!(
        extended > measured + 3.0,
        "smooth: 1 must extend the corner: {measured:.2} → {extended:.2}"
    );
    assert!(
        extended < half * 2.0_f32.sqrt(),
        "…but never past the rect's own corner: {extended:.2}"
    );

    // The straight edges are untouched: whatever the norm, the field along an
    // axis is the plain box distance.
    for smooth in [0.0_f32, 0.5, 1.0] {
        let rb = render(&device, &queue, &box_frame(smooth));
        let edge = rb.edge_along(CENTRE, [1.0, 0.0], 0.5);
        assert!(
            (edge - half).abs() < 0.4,
            "smooth {smooth} moved the straight edge: expected {half}, got {edge:.2}"
        );
    }
}

#[test]
fn the_corner_fringe_is_as_wide_as_the_edge_fringe() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    // §S2. The fringe is the band between nearly-opaque and nearly-clear.
    // Measured along the same ray in both places, so the only variable is the
    // field's own slope, which is exactly what the Lⁿ norm distorts and what
    // the analytic gradient normalisation puts back.
    let rb = render(&device, &queue, &box_frame(1.0));
    let width = |dir: [f32; 2]| rb.edge_along(CENTRE, dir, 0.1) - rb.edge_along(CENTRE, dir, 0.9);
    let edge = width([1.0, 0.0]);
    let corner = width(DIAGONAL);
    assert!(edge > 0.0, "the straight edge must have a fringe at all");
    assert!(
        (corner - edge).abs() <= 1.0 / SCALE + 1e-3,
        "the corner fringe ({corner:.3}) must match the edge fringe ({edge:.3}) \
         to within a physical pixel"
    );
}

#[test]
fn a_shadow_follows_its_casters_corner_profile() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    // §Q2. A transparent caster with an opaque, barely-blurred shadow isolates
    // the shadow's own silhouette: what lands on screen is the shadow field
    // alone, so its corner profile can be measured directly and compared
    // against the caster's. The blur is the shader's smallest (`max(blur, 0.5)`
    // is its floor), because a shadow with no blur, offset or spread at all is
    // skipped outright, there would be nothing to measure.
    let shadow_only = |smooth: f32| {
        let mut frame = RenderFrame::new();
        frame.push_decorated(DecoratedBox {
            base: BoxInstance {
                rect: [BOX_X, BOX_Y, BOX_SIDE, BOX_SIDE],
                color: [0.0; 4],
                radii: [RADIUS; 4],
                transform: Transform::IDENTITY,
                smooth,
            },
            shadow_color: [1.0, 1.0, 1.0, 1.0],
            shadow_blur: 0.5,
            opacity: 1.0,
            dirty: true,
            ..DecoratedBox::default()
        });
        frame
    };

    let caster = render(&device, &queue, &box_frame(1.0)).edge_along(CENTRE, DIAGONAL, 0.5);
    let shadow = render(&device, &queue, &shadow_only(1.0)).edge_along(CENTRE, DIAGONAL, 0.5);
    assert!(
        (caster - shadow).abs() < 0.75,
        "the shadow's corner ({shadow:.2}) must follow its caster's ({caster:.2})"
    );

    // Discriminating: a shadow that had silently kept the circular corner would
    // land several pixels short of this, so the tolerance above is not vacuous.
    let circular = render(&device, &queue, &shadow_only(0.0)).edge_along(CENTRE, DIAGONAL, 0.5);
    assert!(
        shadow > circular + 3.0,
        "the test cannot tell a smoothed shadow ({shadow:.2}) from a circular \
         one ({circular:.2})"
    );
}
