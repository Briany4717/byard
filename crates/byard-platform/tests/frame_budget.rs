//! The steady-state frame budget, enforced (INV-21).
//!
//! # Why this file is the one that matters
//!
//! `support/AUDIT_incremental_paths_and_memory_model.md` found three
//! incremental layers that had been built, validated in isolation, and then
//! silently bypassed for entire phases without anything failing. The fixes are
//! elsewhere; this file is the only part of that work that changes whether it
//! can happen *again*.
//!
//! Every layer had unit tests. Every layer had benchmarks. What none of them
//! had was an assertion that fails when production stops taking the path, so
//! the paths went inert, stayed inert, and the only reason anyone found out was
//! that someone eventually went looking.
//!
//! # The ratchet rule
//!
//! A ceiling below may be **lowered** by any PR that improves things, quietly,
//! as part of the change.
//!
//! A ceiling may be **raised only** by a PR whose description states the new
//! value, the old value, and why the regression is acceptable. A raise with no
//! justification is a review blocker, not a merge conflict. It will be
//! tempting exactly once: the first time this file goes red on a change that
//! "obviously" should not have made anything worse. That is the moment the file
//! exists for.
//!
//! # What is enforced where
//!
//! **Allocation and counter ceilings are enforced everywhere**, because they
//! are deterministic and they are the ones that actually caught these defects.
//!
//! **Timing ceilings are advisory on CI**, shared runners are noisy, and a
//! flaky budget test gets disabled, which is strictly worse than no budget
//! test. They are recorded in `support/PERF_frame_budget.md` and read locally.
//!
//! # Why it lives in `byard-platform`
//!
//! The reference scene is a `.byd` file, so the suite needs the compiler; the
//! GPU counters need a device. `byard-platform` is the only crate that has
//! both, and it already owns this workspace's end-to-end GPU tests. It runs
//! under the existing `cargo test --workspace` CI job, no new wiring, which
//! is also one fewer thing that can be switched off.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::atlas::layout::path_counters;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};

// ── The ceilings ───────────────────────────────────────────────────────────
//
// Recorded on Apple M2, debug build. See this file's header for the rule that
// governs changing them, and `support/PERF_frame_budget.md` for their history.

/// Heap allocations during one steady-state frame (tick + render + encode).
///
/// Not zero, and that is the honest part: PR #148's erratum established that
/// "allocation-free hot path" was never true and was never measured. It is
/// **262** on this scene on Apple M2.
///
/// The ceiling is set well above that on purpose, and the reason is written
/// down rather than left as slack: CI runs this on Linux and Windows too, and
/// the font stack underneath `glyphon` differs per platform in ways that
/// cannot be measured from here. A budget test that goes flaky gets disabled,
/// which is strictly worse than a loose one. **Whoever first reads the printed
/// count on Linux and Windows should tighten this**, lowering a ceiling needs
/// no ceremony, which is the point of a ratchet.
const MAX_ALLOCATIONS_PER_FRAME: usize = 600;

/// GPU buffers created during one steady-state frame (RFC-0033 §G5).
///
/// **Zero, and this one is not a ratchet, it is an invariant.** Raising it
/// means a pipeline started allocating GPU memory at the display rate again,
/// which is the thing RFC-0001 §2's "sin spikes de VRAM" rules out.
const MAX_GPU_BUFFER_CREATIONS_PER_FRAME: u32 = 0;

/// Full layout passes during one steady-state frame (RFC-0032 §R7).
///
/// Zero. A rebuild on a frame where nothing structural happened means the
/// retained path's eligibility broke, and every one of its clauses has a test
/// in `byard-compiler`'s `incremental_paths.rs` that will say which.
const MAX_FULL_COMPUTES_PER_FRAME: u64 = 0;

/// Retained builds opened and then discarded, per steady-state frame
/// (RFC-0032 §R4).
///
/// Zero, and it measures something none of the counters above can. A frame the
/// §R4 whitelist wrongly admits is refused by `end_retained_build` and rebuilt,
/// which is *correct*, and lands on exactly the same `clears`,
/// `full_computes` and `retained_recomputes` as a frame the whitelist rejected
/// outright. The only difference is that the build walk ran twice. Without this
/// ceiling that difference is invisible, which is how a whitelist can quietly
/// stop rejecting anything.
const MAX_RETAINED_ROLLBACKS_PER_FRAME: u64 = 0;

const W: f32 = 640.0;
const H: f32 = 480.0;
const PHYS_W: u32 = 640;
const PHYS_H: u32 = 480;

const SCENE: &str = include_str!("fixtures/budget_scene.byd");

// ── Counting allocator ─────────────────────────────────────────────────────

/// A `System` allocator that counts allocations while armed.
///
/// Armed around exactly the region being measured, so the harness's own
/// allocations, parsing, device setup, the readback buffers, are not billed
/// to the frame.
///
/// SAFETY: a thin pass-through wrapper around `System`. Every method forwards
/// its arguments unchanged to the corresponding `System` method, which is a
/// valid `GlobalAlloc`; the only addition is a relaxed counter bump. Same
/// shape, and the same justification, as the counting allocator in
/// `byard-core`'s `benches/atlas.rs`. Test-only.
#[allow(unsafe_code)]
struct CountingAllocator;

thread_local! {
    /// Allocations on this thread while armed.
    ///
    /// **Thread-local, not global.** A process-wide counter also counts the
    /// other tests in this binary (they run in parallel), plus whatever
    /// `wgpu`'s and `tokio`'s own threads are doing, so the measurement moves
    /// depending on how the harness happens to schedule, which is the one
    /// thing a ratchet must not do. `const` initialisers keep the TLS
    /// non-lazy, so touching it from inside the allocator cannot itself
    /// allocate.
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

/// Bumps this thread's counter if it is armed.
///
/// `try_with` rather than `with`: during thread teardown the TLS slot is gone,
/// and a destructor that allocates must not panic the process.
fn note_allocation() {
    let armed = COUNTING.try_with(Cell::get).unwrap_or(false);
    if armed {
        let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
    }
}

#[allow(unsafe_code)]
// SAFETY: see `CountingAllocator`'s doc comment.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: forwarding an unchanged `layout` to the system allocator,
        // which is the only thing this wrapper does.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` above with this `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        // SAFETY: same contract as `dealloc`, plus a non-zero `new_size`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator;

/// Runs `f` with allocation counting armed and returns how many it performed.
fn count_allocations<R>(f: impl FnOnce() -> R) -> (R, usize) {
    ALLOCATIONS.set(0);
    COUNTING.set(true);
    let out = f();
    COUNTING.set(false);
    (out, ALLOCATIONS.get())
}

// ── Harness ────────────────────────────────────────────────────────────────

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("frame budget device"),
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

fn build() -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(SCENE);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    (interp, tree)
}

fn flip(interp: &mut Interpreter, name: &str) {
    let sig = interp
        .var_signal(&Symbol::intern(name))
        .unwrap_or_else(|| panic!("the budget scene declares `{name}`"));
    let current = interp.peek(sig).as_bool().unwrap_or(false);
    interp.write_var(sig, Value::Bool(!current));
}

/// The whole per-frame path: tick, render into a fresh frame, encode, submit.
fn drive_frame(
    interp: &mut Interpreter,
    tree: &[RenderNode],
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
    frame: &mut RenderFrame,
) {
    interp.tick();
    frame.clear();
    interp.render(tree, frame, W, H);
    let cmd = enc.encode_frame_from_relay(target, frame).unwrap();
    queue.submit(std::iter::once(cmd));
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
}

fn make_target(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("frame budget target"),
        size: wgpu::Extent3d {
            width: PHYS_W,
            height: PHYS_H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Everything a budget run needs, warmed up past the first-frame costs.
struct Warm {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    enc: EncoderSubsystem,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    target: wgpu::Texture,
    frame: RenderFrame,
}

/// Builds the scene and drives enough frames that every cache, pool and arena
/// has reached this scene's high-water mark. Returns `None` with a printed
/// notice when there is no GPU.
fn warm_up() -> Option<Warm> {
    let (device, queue) = try_device()?;
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(&device),
        Arc::clone(&queue),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        1.0,
        PHYS_W,
        PHYS_H,
    ))
    .expect("encoder init");
    enc.update_viewport(Viewport::new(W, H), PHYS_W, PHYS_H, 1.0);

    let (mut interp, tree) = build();
    let target = make_target(&device);
    let mut frame = RenderFrame::new();

    // Warm-up must include at least one of each *kind* of change, or a cache
    // that only fills on the second colour would be billed to the first
    // measured frame.
    for i in 0..6 {
        if i % 2 == 1 {
            flip(&mut interp, "hot");
        }
        drive_frame(
            &mut interp,
            &tree,
            &mut enc,
            &device,
            &queue,
            &target,
            &mut frame,
        );
    }

    Some(Warm {
        interp,
        tree,
        enc,
        device,
        queue,
        target,
        frame,
    })
}

macro_rules! skip_without_gpu {
    ($warm:expr) => {
        match $warm {
            Some(w) => w,
            None => {
                eprintln!("no GPU adapter, skipping the frame budget suite");
                return;
            }
        }
    };
}

// ── The ceilings, asserted ─────────────────────────────────────────────────

#[test]
fn a_steady_state_frame_stays_within_its_allocation_ceiling() {
    let mut w = skip_without_gpu!(warm_up());

    // A value-only frame: one colour changes, nothing moves.
    flip(&mut w.interp, "hot");
    let ((), allocations) = count_allocations(|| {
        drive_frame(
            &mut w.interp,
            &w.tree,
            &mut w.enc,
            &w.device,
            &w.queue,
            &w.target,
            &mut w.frame,
        );
    });

    assert!(
        allocations <= MAX_ALLOCATIONS_PER_FRAME,
        "a steady-state frame performed {allocations} heap allocations, over \
         the recorded ceiling of {MAX_ALLOCATIONS_PER_FRAME}.\n\n\
         If this is a deliberate regression, raise the ceiling *and say so in \
         the PR description*, the old value, the new value, and why. If it is \
         not deliberate, something on the per-frame path started allocating; \
         the usual causes are a `Vec` built per frame instead of reused, or a \
         cache that stopped hitting."
    );
    eprintln!("frame budget: {allocations} allocations (ceiling {MAX_ALLOCATIONS_PER_FRAME})");
}

#[test]
fn a_steady_state_frame_creates_no_gpu_buffers() {
    let mut w = skip_without_gpu!(warm_up());

    let before = w.enc.arena().buffer_creations();
    let grows_before = w.enc.arena().grows_this_session();
    for _ in 0..5 {
        flip(&mut w.interp, "hot");
        drive_frame(
            &mut w.interp,
            &w.tree,
            &mut w.enc,
            &w.device,
            &w.queue,
            &w.target,
            &mut w.frame,
        );
    }

    assert_eq!(
        w.enc.arena().buffer_creations() - before,
        MAX_GPU_BUFFER_CREATIONS_PER_FRAME,
        "a steady-state frame created a GPU buffer. This ceiling is not a \
         ratchet, it is RFC-0001 §2's 'sin spikes de VRAM' expressed as a \
         number, and the fix is to route the new pipeline through the instance \
         arena, not to raise it."
    );
    assert_eq!(
        w.enc.arena().grows_this_session(),
        grows_before,
        "the arena grew after warm-up on a scene of constant size"
    );
}

#[test]
fn a_steady_state_frame_never_rebuilds_the_layout_tree() {
    let mut w = skip_without_gpu!(warm_up());

    for i in 0..5 {
        flip(&mut w.interp, "hot");
        path_counters::reset();
        drive_frame(
            &mut w.interp,
            &w.tree,
            &mut w.enc,
            &w.device,
            &w.queue,
            &w.target,
            &mut w.frame,
        );
        let counts = path_counters::snapshot();
        assert_eq!(
            counts.full_computes, MAX_FULL_COMPUTES_PER_FRAME,
            "frame {i} ran a full layout pass. Nothing structural changed, so \
             the retained path's eligibility broke, `byard-compiler`'s \
             `incremental_paths.rs` has one test per clause and will say which."
        );
        assert_eq!(counts.clears, 0, "frame {i} tore the atlas down");
        assert_eq!(counts.retained_recomputes, 1, "frame {i} skipped layout");
        assert_eq!(
            counts.retained_rollbacks, MAX_RETAINED_ROLLBACKS_PER_FRAME,
            "frame {i} opened a retained build and then threw it away, so the \
             tree was walked twice. The frame is correct and costs double; the \
             §R4 whitelist admitted something it should have rejected."
        );
        assert_eq!(
            counts.retained_attempts, 1,
            "frame {i} never opened a retained build at all"
        );
    }
}

#[test]
fn a_value_only_frame_takes_the_scissored_path() {
    let mut w = skip_without_gpu!(warm_up());

    flip(&mut w.interp, "hot");
    drive_frame(
        &mut w.interp,
        &w.tree,
        &mut w.enc,
        &w.device,
        &w.queue,
        &w.target,
        &mut w.frame,
    );

    assert!(
        w.enc.last_frame_scissored(),
        "a one-colour change repainted the whole window. The encoder's \
         incremental scissor is the third of the audit's inert layers, and a \
         timing cannot tell you it stopped being taken."
    );
    let dirty = w.frame.instances_dirty().iter().filter(|d| **d).count();
    assert_eq!(
        dirty,
        1,
        "exactly one solid box changed colour, so exactly one may be dirty; \
         {dirty} of {} were",
        w.frame.instances().len()
    );
    assert!(
        w.frame.texts().iter().all(|t| !t.dirty),
        "no text changed, so none may be re-shaped, this is the assertion \
         that stands between the scene costing 3 ms and costing 45 ms"
    );
}

#[test]
fn a_layout_affecting_frame_reaches_populate_frame_with_real_targets() {
    // RFC-0032 §R7's `targets_received > 0`, stated for the frame it is true
    // of. A *paint-only* frame correctly marks nothing, a colour is not a
    // layout input, so the criterion belongs to a frame that moves geometry.
    let mut w = skip_without_gpu!(warm_up());

    flip(&mut w.interp, "tall");
    path_counters::reset();
    drive_frame(
        &mut w.interp,
        &w.tree,
        &mut w.enc,
        &w.device,
        &w.queue,
        &w.target,
        &mut w.frame,
    );
    let counts = path_counters::snapshot();

    assert!(
        counts.populate_dirty_targets > 0,
        "a height change reached `populate_frame` with an empty dirty set, \
         the layer PR #148 found had no producer at all"
    );
    assert_eq!(
        counts.populate_dirty_matched, counts.populate_dirty_targets,
        "every target must match a live node. A ratio below 1 means the \
         targets are generation-stale, which is a different bug from passing \
         none and is why the matched count exists"
    );
    assert_eq!(counts.full_computes, 0, "and it must still not rebuild");
}

#[test]
fn an_idle_frame_marks_nothing_and_draws_nothing() {
    // The floor of the budget: a frame in which the app did nothing at all.
    // If this ever reports dirty primitives, every ceiling above is measuring
    // a frame that is doing work it should not be.
    let mut w = skip_without_gpu!(warm_up());

    path_counters::reset();
    drive_frame(
        &mut w.interp,
        &w.tree,
        &mut w.enc,
        &w.device,
        &w.queue,
        &w.target,
        &mut w.frame,
    );
    let counts = path_counters::snapshot();

    assert_eq!(counts.full_computes, 0);
    assert_eq!(counts.populate_dirty_targets, 0);
    assert!(
        w.frame.instances_dirty().iter().all(|d| !d),
        "nothing changed, so no box may be reported dirty"
    );
    assert!(w.frame.texts().iter().all(|t| !t.dirty));
}
