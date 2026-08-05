//! GPU readback proofs for radial and conic gradients (RFC-0035).
//!
//! Four claims that only pixels can settle, because the instance data is the
//! same shape either way and it is the fragment shader that reads it right or
//! wrong:
//!
//! 1. **A radial glow falls off from its centre**, and is *circular in the box's
//!    own aspect*: on a 2:1 card the falloff has to reach the same colour at the
//!    same fraction of the half-width and the half-height, or the "glow" is an
//!    ellipse the element's proportions imposed on it.
//! 2. **A conic sweep wraps without a seam.** `fract` of a turn is a
//!    discontinuity in the parameter; it is only invisible if the stops meet.
//!    Sampling across the start angle is the only way to know they do.
//! 3. **`smooth` still works on a gradient box.** RFC-0035 as written puts the
//!    kind tag in `misc.w`, which RFC-0031 took for corner smoothing. If the
//!    tag ever lands there again, the corner of a gradient-filled box hardens
//!    and this fails.
//! 4. **A linear ramp is unchanged**, sampled against the profile it has always
//!    produced rather than against itself.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use byard_core::BoxInstance;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{DecoratedBox, Gradient, GradientKind, RenderFrame, Transform, Viewport};
use std::sync::Arc;

const LOGICAL_W: f32 = 240.0;
const LOGICAL_H: f32 = 120.0;
const SCALE: f32 = 2.0;

/// The probe card: a wide 2:1 rect, so an aspect mistake is a large error
/// rather than a rounding one.
const CARD: [f32; 4] = [20.0, 20.0, 200.0, 80.0];

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gradient kinds readback device"),
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
    /// `(b, g, r, a)` at a logical point.
    fn at(&self, lx: f32, ly: f32) -> (u8, u8, u8, u8) {
        let px = (lx * SCALE) as u32;
        let py = (ly * SCALE) as u32;
        let idx = (py * self.bpr + px * 4) as usize;
        (
            self.data[idx],
            self.data[idx + 1],
            self.data[idx + 2],
            self.data[idx + 3],
        )
    }

    /// The green channel at a logical point, `0..1`.
    fn green(&self, lx: f32, ly: f32) -> f32 {
        f32::from(self.at(lx, ly).1) / 255.0
    }

    /// The red channel at a logical point, `0..1`.
    fn red(&self, lx: f32, ly: f32) -> f32 {
        f32::from(self.at(lx, ly).2) / 255.0
    }

    /// The green channel decoded back to linear space, which is the space the
    /// stops were mixed in. Comparing an sRGB-encoded byte against a linear
    /// expectation is how a correct falloff reads as a wrong number.
    fn green_linear(&self, lx: f32, ly: f32) -> f32 {
        let c = self.green(lx, ly);
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
}

fn render(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>, frame: &RenderFrame) -> Readback {
    let phys_w = (LOGICAL_W * SCALE) as u32;
    let phys_h = (LOGICAL_H * SCALE) as u32;
    let fmt = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        fmt,
        SCALE,
        phys_w,
        phys_h,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: LOGICAL_W,
            height: LOGICAL_H,
        },
        phys_w,
        phys_h,
        SCALE,
    );
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gradient kinds target"),
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
        label: Some("gradient kinds readback"),
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
    buffer.unmap();
    Readback { data, bpr }
}

/// The probe card, filled black, carrying `gradient` and `smooth`.
fn card(gradient: Option<Gradient>, smooth: f32) -> RenderFrame {
    let mut frame = RenderFrame::new();
    frame.push_decorated(DecoratedBox {
        base: BoxInstance {
            rect: CARD,
            color: [0.0, 0.0, 0.0, 1.0],
            radii: [24.0; 4],
            transform: Transform::IDENTITY,
            smooth,
        },
        opacity: 1.0,
        gradient,
        dirty: true,
        ..Default::default()
    });
    frame
}

/// An opaque green glow at the card's centre, reaching the edge of its own
/// radius and nothing beyond it.
fn radial(radius: f32) -> Gradient {
    Gradient {
        kind: GradientKind::Radial,
        center: [0.5, 0.5],
        radius,
        from: [0.0, 1.0, 0.0, 1.0],
        mid: [0.0, 0.5, 0.0, 1.0],
        to: [0.0, 0.0, 0.0, 1.0],
        mid_pos: 0.5,
        ..Gradient::two_stop(0.0, [0.0; 4], [0.0; 4])
    }
}

#[test]
fn a_radial_glow_falls_off_from_its_centre() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    let rb = render(&device, &queue, &card(Some(radial(1.0)), 0.0));
    let (cx, cy) = (CARD[0] + CARD[2] / 2.0, CARD[1] + CARD[3] / 2.0);

    let centre = rb.green_linear(cx, cy);
    let quarter = rb.green_linear(cx + CARD[2] * 0.25, cy);
    let edge = rb.green_linear(CARD[0] + CARD[2] - 4.0, cy);
    assert!(
        centre > 0.9,
        "the centre is the first stop, got {centre} (a linear reading would not peak here)"
    );
    assert!(
        centre > quarter && quarter > edge,
        "the falloff is monotone outward: {centre} → {quarter} → {edge}"
    );
    assert!(edge < 0.1, "and reaches the last stop by the rim: {edge}");
}

#[test]
fn a_radial_centre_lands_where_it_was_asked_to() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    // `center` is normalized element space: `(1, 0)` is the top-right corner
    // whatever the card's size. An inverted axis, a half-scale mistake or a
    // forgotten aspect term all leave the falloff looking plausible and put its
    // peak somewhere else, which is a bug you can stare straight at.
    let corner = Gradient {
        center: [1.0, 0.0],
        ..radial(0.9)
    };
    let rb = render(&device, &queue, &card(Some(corner), 0.0));
    let inset = 12.0;
    let (l, t) = (CARD[0] + inset, CARD[1] + inset);
    let (r, b) = (CARD[0] + CARD[2] - inset, CARD[1] + CARD[3] - inset);
    let top_right = rb.green_linear(r, t);
    for (name, x, y) in [
        ("top-left", l, t),
        ("bottom-right", r, b),
        ("bottom-left", l, b),
    ] {
        assert!(
            top_right > rb.green_linear(x, y) + 0.2,
            "the peak is at the top-right corner, not {name}: \
             {top_right} vs {}",
            rb.green_linear(x, y)
        );
    }
}

#[test]
fn a_radial_glow_stays_circular_on_a_wide_card() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    // The card is 200×80. Without aspect correction the falloff is measured in
    // normalized box space, so the same *fraction* of the half-width and the
    // half-height would land on the same colour, i.e. an ellipse stretched to
    // the element. Corrected, equal *pixel* distances match instead.
    let rb = render(&device, &queue, &card(Some(radial(1.0)), 0.0));
    let (cx, cy) = (CARD[0] + CARD[2] / 2.0, CARD[1] + CARD[3] / 2.0);
    let d = 30.0; // pixels, well inside both half-extents (100 and 40)
    let horizontal = rb.green_linear(cx + d, cy);
    let vertical = rb.green_linear(cx, cy + d);
    assert!(
        (horizontal - vertical).abs() < 0.06,
        "equal pixel distances are equal colours in a circular falloff: \
         {horizontal} across vs {vertical} down"
    );
}

#[test]
fn a_conic_sweep_wraps_without_a_seam() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    // A dial that starts and ends on the same colour: red at the start angle,
    // green half a turn later, red again on the way back. `start: -90deg` puts
    // the seam at twelve o'clock, which is where a dial's seam belongs and
    // where this test samples.
    let sweep = Gradient {
        kind: GradientKind::Conic,
        center: [0.5, 0.5],
        angle: -std::f32::consts::FRAC_PI_2,
        from: [1.0, 0.0, 0.0, 1.0],
        mid: [0.0, 1.0, 0.0, 1.0],
        to: [1.0, 0.0, 0.0, 1.0],
        mid_pos: 0.5,
        ..Gradient::two_stop(0.0, [0.0; 4], [0.0; 4])
    };
    let rb = render(&device, &queue, &card(Some(sweep), 0.0));
    let (cx, cy) = (CARD[0] + CARD[2] / 2.0, CARD[1] + CARD[3] / 2.0);

    // Straight up from the centre is the start angle: the seam.
    let r = 20.0;
    let just_left = rb.red(cx - 3.0, cy - r);
    let just_right = rb.red(cx + 3.0, cy - r);
    assert!(
        just_left > 0.8 && just_right > 0.8,
        "both sides of the wrap are the shared stop: {just_left} / {just_right}"
    );
    assert!(
        (just_left - just_right).abs() < 0.1,
        "and they meet, rather than stepping across the seam: \
         {just_left} vs {just_right}"
    );
    // A quarter turn along, the sweep is well into the middle stop, which is
    // what makes the two samples above a continuity claim and not a flat fill.
    let quarter = rb.green(cx + r, cy);
    assert!(quarter > 0.4, "the sweep actually sweeps: {quarter}");
}

#[test]
fn a_gradient_box_still_has_smoothed_corners() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    // RFC-0035 §Instance-data puts the kind tag in `misc.w`. RFC-0031 took that
    // lane for `smooth`, so a tag written there is a corner-profile change on
    // every gradient box: `smooth: 1` would read back as `smooth: 0.25` (the
    // radial tag) or `0.5` (conic). The squircle fills its corner further out
    // than the circle does, so one sample at 25 % of the radius along the
    // diagonal separates them.
    let radius = 24.0_f32;
    let probe = (
        CARD[0] + radius * 0.25,
        CARD[1] + radius * 0.25, // inside the corner box, outside the circle
    );
    let circular = render(&device, &queue, &card(Some(radial(0.9)), 0.0));
    let squircle = render(&device, &queue, &card(Some(radial(0.9)), 1.0));
    assert!(
        circular.at(probe.0, probe.1).3 < 40,
        "a circular corner leaves this point empty, got {:?}",
        circular.at(probe.0, probe.1)
    );
    assert!(
        squircle.at(probe.0, probe.1).3 > 200,
        "a squircle fills it, got {:?}. If this is empty on a gradient box but \
         full on a plain one, the kind tag is being written into `smooth`",
        squircle.at(probe.0, probe.1)
    );
}

#[test]
fn a_linear_ramp_still_draws_the_profile_it_always_did() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    // The parity claim of the milestone, sampled rather than compared against
    // itself: a left→right black→green ramp is a straight line in the fragment
    // parameter, so the green channel at 25 %, 50 % and 75 % of the card is
    // the mix at those fractions. If the kind branch had changed the linear
    // path in any way, these three numbers are what would move.
    let ramp = Gradient {
        kind: GradientKind::Linear,
        angle: 0.0,
        from: [0.0, 0.0, 0.0, 1.0],
        mid: [0.0, 0.5, 0.0, 1.0],
        to: [0.0, 1.0, 0.0, 1.0],
        mid_pos: 0.5,
        ..Gradient::two_stop(0.0, [0.0; 4], [0.0; 4])
    };
    let rb = render(&device, &queue, &card(Some(ramp), 0.0));
    let cy = CARD[1] + CARD[3] / 2.0;
    for (fraction, expected) in [(0.25_f32, 0.25_f32), (0.5, 0.5), (0.75, 0.75)] {
        let x = CARD[0] + CARD[2] * fraction;
        let linear = rb.green_linear(x, cy);
        assert!(
            (linear - expected).abs() < 0.06,
            "at {fraction} of the ramp the mix is {expected}, read back {linear}"
        );
    }
}
