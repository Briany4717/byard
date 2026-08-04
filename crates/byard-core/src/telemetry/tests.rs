use super::*;

#[test]
fn push_writes_a_sample_and_increments_len() {
    // Isolate from other tests sharing the same thread-local ring by
    // draining first.
    let _ = drain_samples();
    push_sample(Sample::cpu(ScopeId(1), 0, 1, 2));
    assert_eq!(ring_len(), 1);
    let block = drain_samples();
    assert_eq!(block.samples.len(), 1);
    assert_eq!(block.samples[0].scope, ScopeId(1));
}

#[test]
fn full_ring_drops_newest_and_honors_capacity() {
    let _ = drain_samples();
    for i in 0..RING_CAPACITY {
        #[allow(clippy::cast_possible_truncation)]
        push_sample(Sample::cpu(ScopeId(0), 0, i as u64, i as u64));
    }
    assert_eq!(ring_len(), RING_CAPACITY);
    assert_eq!(ring_dropped(), 0);

    // One more push once full: dropped, not overwriting slot 0.
    push_sample(Sample::cpu(ScopeId(99), 0, 999, 999));
    assert_eq!(
        ring_len(),
        RING_CAPACITY,
        "capacity is honored, not exceeded"
    );
    assert_eq!(ring_dropped(), 1, "the overflowing sample is dropped");

    let block = drain_samples();
    assert_eq!(block.samples.len(), RING_CAPACITY);
    assert_eq!(block.samples[0].start, 0, "slot 0 was never overwritten");
    assert_eq!(block.dropped, 1);

    // Draining resets the ring for the next tick.
    assert_eq!(ring_len(), 0);
    assert_eq!(ring_dropped(), 0);
}

#[test]
fn drain_samples_into_reuses_the_callers_vec_allocation() {
    let _ = drain_samples();
    let mut block = SampleBlock::default();
    block.samples.reserve(RING_CAPACITY);
    let reused_capacity = block.samples.capacity();
    assert!(reused_capacity >= RING_CAPACITY);

    for _ in 0..RING_CAPACITY {
        push_sample(Sample::default());
    }
    drain_samples_into(&mut block);
    assert_eq!(block.samples.len(), RING_CAPACITY);
    assert_eq!(
        block.samples.capacity(),
        reused_capacity,
        "draining into an already-sized buffer must not reallocate"
    );

    // A second tick with fewer samples reuses the same allocation again.
    push_sample(Sample::default());
    drain_samples_into(&mut block);
    assert_eq!(block.samples.len(), 1);
    assert_eq!(block.samples.capacity(), reused_capacity);
}

#[test]
fn scope_id_is_stable_and_interned_per_name() {
    let a1 = scope_id("telemetry.test.scope_a");
    let a2 = scope_id("telemetry.test.scope_a");
    let b = scope_id("telemetry.test.scope_b");
    assert_eq!(a1, a2, "the same name always resolves to the same id");
    assert_ne!(a1, b, "distinct names get distinct ids");
}

#[test]
fn scope_id_tagged_records_its_kind() {
    let interp = scope_id_tagged("telemetry.test.kind.interp", ScopeKind::Interpreter);
    let native = scope_id_tagged("telemetry.test.kind.native", ScopeKind::Native);
    let gpu = scope_id_tagged("telemetry.test.kind.gpu", ScopeKind::Gpu);
    assert_eq!(scope_kind(interp), Some(ScopeKind::Interpreter));
    assert_eq!(scope_kind(native), Some(ScopeKind::Native));
    assert_eq!(scope_kind(gpu), Some(ScopeKind::Gpu));
    assert_eq!(
        scope_kind(scope_id("telemetry.test.kind.default_native")),
        Some(ScopeKind::Native),
        "plain scope_id defaults to Native"
    );
}

#[test]
fn scope_name_round_trips_through_scope_id() {
    let id = scope_id("telemetry.test.name.round_trip");
    assert_eq!(scope_name(id), Some("telemetry.test.name.round_trip"));
}

#[test]
#[should_panic(expected = "re-registered with a different ScopeKind")]
fn scope_id_tagged_rejects_a_kind_change_for_an_existing_name() {
    let _ = scope_id_tagged("telemetry.test.kind.stable", ScopeKind::Native);
    let _ = scope_id_tagged("telemetry.test.kind.stable", ScopeKind::Interpreter);
}

#[test]
fn interpreter_tax_sums_only_interpreter_tagged_samples() {
    let interp = scope_id_tagged("telemetry.test.tax.interp", ScopeKind::Interpreter);
    let native = scope_id_tagged("telemetry.test.tax.native", ScopeKind::Native);
    let gpu = scope_id_tagged("telemetry.test.tax.gpu", ScopeKind::Gpu);
    let block = SampleBlock {
        samples: vec![
            Sample::cpu(interp, 0, 0, 100),
            Sample::cpu(native, 0, 0, 50),
            Sample::gpu_duration(gpu, 30),
            Sample::cpu(interp, 0, 100, 175),
        ],
        dropped: 0,
    };
    assert_eq!(block.interpreter_tax_ns(), 100 + 75);
    assert_eq!(block.sum_by_kind(ScopeKind::Native), 50);
    assert_eq!(block.sum_by_kind(ScopeKind::Gpu), 30);
}

#[test]
fn project_aot_replaces_measured_interpreter_cost_with_its_native_equivalent() {
    let calibration = Calibration {
        basis: "test calibration",
        interpreter_to_native_ratio: 0.5,
    };
    // total = 10ms, of which 6ms was interpreter; native equivalent is
    // half that (3ms), so the projection is 10 - 6 + 3 = 7ms.
    let projection = project_aot(10_000_000, 6_000_000, &calibration);
    assert_eq!(projection.projected_ns, 7_000_000);
    assert_eq!(projection.basis, "test calibration");
}

#[test]
fn project_aot_saturates_instead_of_overflowing() {
    // A pathological ratio/measurement combination must clamp to
    // u64::MAX rather than silently wrapping around to a tiny number.
    let calibration = Calibration {
        basis: "test calibration",
        interpreter_to_native_ratio: 1e9,
    };
    let projection = project_aot(u64::MAX, u64::MAX, &calibration);
    assert_eq!(projection.projected_ns, u64::MAX);
}

#[test]
fn profile_scope_writes_one_sample_via_guard() {
    let _ = drain_samples();
    {
        crate::profile_scope!("telemetry.test.profile_scope_writes_one_sample_via_guard");
    }
    #[cfg(feature = "telemetry")]
    assert_eq!(ring_len(), 1, "the guard's Drop wrote exactly one sample");
    #[cfg(not(feature = "telemetry"))]
    assert_eq!(ring_len(), 0, "with telemetry off, the macro is a no-op");
}

#[test]
#[cfg(not(feature = "telemetry"))]
fn profile_scope_is_noop_without_telemetry_feature() {
    // Exercised via `cargo test -p byard-core --no-default-features`.
    let _ = drain_samples();
    crate::profile_scope!("telemetry.test.noop");
    assert_eq!(ring_len(), 0, "no guard is constructed without the feature");
}

#[test]
fn sample_block_is_send_pod_data() {
    const fn assert_send<T: Send>() {}
    assert_send::<SampleBlock>();
    assert_send::<Sample>();

    let sample = Sample::cpu(ScopeId(3), 0, 10, 20);
    // Round-trips as raw bytes, as it will across the frame swap.
    let bytes: &[u8] = bytemuck::bytes_of(&sample);
    let back: Sample = bytemuck::pod_read_unaligned(bytes);
    assert_eq!(back, sample);
    assert_eq!(std::mem::size_of::<Sample>(), 24, "no implicit padding");
}

// ── Nesting depth and self-time (RFC-0030 §I2 / §I2b) ──────────────────

/// Builds a block from `(name, kind, depth, duration_ns)` rows given in
/// push (i.e. `Drop`) order, so a child always precedes its parent, the
/// exact shape the ring produces.
fn block(rows: &[(&'static str, ScopeKind, u8, u64)]) -> SampleBlock {
    SampleBlock {
        samples: rows
            .iter()
            .map(|&(name, kind, depth, dur)| {
                Sample::cpu(scope_id_tagged(name, kind), depth, 0, dur)
            })
            .collect(),
        dropped: 0,
    }
}

#[test]
// With the feature off, `profile_scope!` constructs no guard at all, so
// there is no depth to nest and nothing to record. The assertions below
// are about the guard's behaviour, not about the macro's, and asserting
// them against a no-op is asserting that the no-op is not one.
#[cfg(feature = "telemetry")]
fn a_nested_guard_records_one_deeper_than_its_parent() {
    let _ = drain_samples();
    {
        crate::profile_scope!("telemetry.test.depth.outer");
        assert_eq!(current_depth(), 1, "one guard is live");
        {
            crate::profile_scope!("telemetry.test.depth.inner");
            assert_eq!(current_depth(), 2, "two guards are live");
        }
        assert_eq!(current_depth(), 1, "the inner guard restored the depth");
    }
    assert_eq!(current_depth(), 0, "the outer guard restored the depth");

    let block = drain_samples();
    assert_eq!(block.samples.len(), 2);
    // Drop order: the inner guard drops first, so it is pushed first.
    assert_eq!(block.samples[0].depth(), 1);
    assert_eq!(block.samples[1].depth(), 0);
    assert_eq!(
        scope_name(block.samples[0].scope),
        Some("telemetry.test.depth.inner")
    );
}

#[test]
fn self_time_subtracts_direct_children_and_the_block_total_is_the_parent() {
    // A 10 ms parent containing a 4 ms child: parent inclusive 10 ms,
    // parent self 6 ms, block total 10 ms, never 14 ms.
    let b = block(&[
        ("telemetry.test.self.child", ScopeKind::Native, 1, 4_000_000),
        (
            "telemetry.test.self.parent",
            ScopeKind::Native,
            0,
            10_000_000,
        ),
    ]);
    assert_eq!(b.samples[1].duration_ns(), 10_000_000, "inclusive");
    assert_eq!(b.self_ns(1), 6_000_000, "self");
    assert_eq!(b.self_ns(0), 4_000_000, "a leaf's self time is its own");
    assert_eq!(b.total_ns(), 10_000_000, "the total is the frame, not 14ms");
}

#[test]
fn a_grandchild_is_counted_once_inside_its_own_parent() {
    // P(0) ⊃ { A(1) ⊃ A1(2), B(1) }. Push order is A1, A, B, P.
    let b = block(&[
        ("telemetry.test.gc.a1", ScopeKind::Native, 2, 1_000_000),
        ("telemetry.test.gc.a", ScopeKind::Native, 1, 3_000_000),
        ("telemetry.test.gc.b", ScopeKind::Native, 1, 2_000_000),
        ("telemetry.test.gc.p", ScopeKind::Native, 0, 10_000_000),
    ]);
    assert_eq!(b.self_ns(0), 1_000_000, "A1 is a leaf");
    assert_eq!(b.self_ns(1), 2_000_000, "A minus A1");
    assert_eq!(b.self_ns(2), 2_000_000, "B is a leaf");
    assert_eq!(b.self_ns(3), 5_000_000, "P minus A and B, not minus A1");
    // Every nanosecond is attributed exactly once.
    let total_self: u64 = (0..b.samples.len()).map(|i| b.self_ns(i)).sum();
    assert_eq!(total_self, b.total_ns());
}

#[test]
fn a_preceding_sibling_subtree_is_not_mistaken_for_a_child() {
    // Two independent depth-0 scopes, the first with a child. The second
    // must not absorb the first one's subtree.
    let b = block(&[
        ("telemetry.test.sib.child", ScopeKind::Native, 1, 4_000_000),
        ("telemetry.test.sib.first", ScopeKind::Native, 0, 6_000_000),
        ("telemetry.test.sib.second", ScopeKind::Native, 0, 3_000_000),
    ]);
    assert_eq!(b.self_ns(1), 2_000_000);
    assert_eq!(b.self_ns(2), 3_000_000, "no children to subtract");
    assert_eq!(b.total_ns(), 9_000_000);
}

#[test]
fn the_interpreter_tax_excludes_a_native_child_of_an_interpreter_parent() {
    // The RFC-0030 §I2b case: `layout.taffy` is Native and nests inside
    // `interp.render`, which is Interpreter. An AOT build still pays for
    // Taffy, so the tax must be 10ms − 4ms.
    let b = block(&[
        (
            "telemetry.test.taxnest.layout",
            ScopeKind::Native,
            1,
            4_000_000,
        ),
        (
            "telemetry.test.taxnest.render",
            ScopeKind::Interpreter,
            0,
            10_000_000,
        ),
    ]);
    assert_eq!(b.interpreter_tax_ns(), 6_000_000);
    assert_eq!(
        b.sum_by_kind(ScopeKind::Interpreter),
        10_000_000,
        "the inclusive accessor still reports inclusive time"
    );
    assert_eq!(b.sum_self_by_kind(ScopeKind::Native), 4_000_000);
}

#[test]
fn an_interpreter_child_of_an_interpreter_parent_is_still_fully_taxed() {
    // `interp.tick` re-pulled inside `interp.render`: both are interpreter
    // work, so splitting them between self and inclusive must not lose
    // any of it.
    let b = block(&[
        (
            "telemetry.test.taxsame.tick",
            ScopeKind::Interpreter,
            1,
            4_000_000,
        ),
        (
            "telemetry.test.taxsame.render",
            ScopeKind::Interpreter,
            0,
            10_000_000,
        ),
    ]);
    assert_eq!(b.interpreter_tax_ns(), 10_000_000);
}

#[test]
fn gpu_samples_are_depth_zero_and_unaffected_by_the_cpu_nesting() {
    let gpu = scope_id_tagged("telemetry.test.depth.gpu", ScopeKind::Gpu);
    let sample = Sample::gpu_duration(gpu, 700_000);
    assert_eq!(sample.depth(), 0, "RFC-0030 Q6: set explicitly, not left");
    let b = SampleBlock {
        samples: vec![sample],
        dropped: 0,
    };
    assert_eq!(b.self_ns(0), 700_000);
    assert_eq!(b.sum_by_kind(ScopeKind::Gpu), 700_000);
}

#[test]
fn direct_children_are_replayed_in_entry_order_not_drop_order() {
    // P(0) ⊃ { A(1) ⊃ A1(2), B(1) }, stored A1, A, B, P. A consumer
    // rendering a tree needs A before B (they were entered in that order)
    // even though the block holds them the other way round relative to
    // their parent.
    let b = block(&[
        ("telemetry.test.order.a1", ScopeKind::Native, 2, 1_000_000),
        ("telemetry.test.order.a", ScopeKind::Native, 1, 3_000_000),
        ("telemetry.test.order.b", ScopeKind::Native, 1, 2_000_000),
        ("telemetry.test.order.p", ScopeKind::Native, 0, 10_000_000),
    ]);
    let mut seen = Vec::new();
    b.for_each_direct_child(3, |i, s| seen.push((i, scope_name(s.scope).unwrap())));
    assert_eq!(
        seen,
        vec![(1, "telemetry.test.order.a"), (2, "telemetry.test.order.b")],
        "direct children only, in entry order"
    );

    let mut grandchildren = Vec::new();
    b.for_each_direct_child(1, |i, _| grandchildren.push(i));
    assert_eq!(grandchildren, vec![0], "A's only child is A1");

    let mut roots = Vec::new();
    b.for_each_root(|i, _| roots.push(i));
    assert_eq!(roots, vec![3]);
}

#[test]
fn a_leaf_reports_no_children() {
    let b = block(&[("telemetry.test.leaf.only", ScopeKind::Native, 0, 1_000)]);
    let mut any = false;
    b.for_each_direct_child(0, |_, _| any = true);
    assert!(!any);
}

#[test]
fn self_ns_is_zero_for_an_index_past_the_end() {
    assert_eq!(SampleBlock::default().self_ns(7), 0);
}

// ── Owner attribution (RFC-0030 erratum "self-accounting") ─────────────

#[test]
fn a_sample_defaults_to_the_app_and_costs_no_extra_bytes_to_say_so() {
    assert_eq!(Sample::cpu(ScopeId(1), 0, 0, 10).owner(), Owner::App);
    assert_eq!(Sample::default().owner(), Owner::App);
    assert_eq!(
        std::mem::size_of::<Sample>(),
        24,
        "`owner` lives in padding that was already reserved"
    );
    // `Sample` is `Pod`, so every bit pattern has to mean something. An
    // owner byte nobody wrote reads as the app rather than inventing
    // dev-tool overhead out of a corrupt frame.
    let mut bytes = [0u8; 24];
    bytes.copy_from_slice(bytemuck::bytes_of(&Sample::cpu(ScopeId(1), 0, 0, 10)));
    bytes[3] = 0xAB;
    let corrupt: Sample = bytemuck::pod_read_unaligned(&bytes);
    assert_eq!(corrupt.owner(), Owner::App);
}

#[test]
fn attribute_to_stamps_the_whole_subtree_including_code_that_never_heard_of_it() {
    // The property the design rests on: the HUD's cost is the interpreter
    // and the layout atlas doing their ordinary jobs, in code shared with
    // the app. Nothing below the boundary opts in.
    let _ = drain_samples();
    {
        crate::profile_scope!("telemetry.test.owner.app_outer");
        {
            let _dev = attribute_to(Owner::DevTools);
            crate::profile_scope!("telemetry.test.owner.dev_outer");
            crate::profile_scope!("telemetry.test.owner.dev_inner");
        }
        crate::profile_scope!("telemetry.test.owner.app_after");
    }
    assert_eq!(
        current_owner(),
        Owner::App,
        "the guard restores rather than resets"
    );

    #[cfg(feature = "telemetry")]
    {
        let block = drain_samples();
        let owner_of = |name: &str| {
            block
                .samples
                .iter()
                .find(|s| scope_name(s.scope) == Some(name))
                .unwrap_or_else(|| panic!("{name} was not sampled"))
                .owner()
        };
        assert_eq!(owner_of("telemetry.test.owner.app_outer"), Owner::App);
        assert_eq!(owner_of("telemetry.test.owner.dev_outer"), Owner::DevTools);
        assert_eq!(owner_of("telemetry.test.owner.dev_inner"), Owner::DevTools);
        assert_eq!(
            owner_of("telemetry.test.owner.app_after"),
            Owner::App,
            "attribution ends with the guard, not with the enclosing scope"
        );
    }
}

#[test]
fn the_two_owner_buckets_reconstruct_the_frame_rather_than_exceeding_it() {
    // The §I2b lesson applied one level out. `encode.glyphs.dev` is
    // dev-owned and nests inside app-owned `encode.glyphs`, which nests
    // inside app-owned `encode.frame`. Push order is innermost-first.
    let dev = scope_id("telemetry.test.owner.split.glyphs_dev");
    let glyphs = scope_id("telemetry.test.owner.split.glyphs");
    let frame = scope_id("telemetry.test.owner.split.frame");
    let hud = scope_id("telemetry.test.owner.split.hud");
    let b = SampleBlock {
        samples: vec![
            Sample::owned(dev, Owner::DevTools, 2, 0, 900_000),
            Sample::cpu(glyphs, 1, 0, 1_200_000),
            Sample::cpu(frame, 0, 0, 2_000_000),
            Sample::owned(hud, Owner::DevTools, 0, 0, 300_000),
        ],
        dropped: 0,
    };
    assert_eq!(b.total_ns(), 2_300_000);
    assert_eq!(
        b.owner_total_ns(Owner::DevTools),
        1_200_000,
        "the dev bucket is its own depth-0 scope *plus* its share of a \
             scope the app owns, which is the whole point"
    );
    assert_eq!(b.owner_total_ns(Owner::App), 1_100_000);
    assert_eq!(
        b.owner_total_ns(Owner::App) + b.owner_total_ns(Owner::DevTools),
        b.total_ns(),
        "every nanosecond is attributed to exactly one owner"
    );
}

#[test]
fn an_empty_dev_bucket_is_zero_rather_than_the_whole_frame() {
    // "The HUD is closed" must read as "it cost nothing", never as a
    // fallback to some other total.
    let b = block(&[(
        "telemetry.test.owner.app_only",
        ScopeKind::Native,
        0,
        5_000_000,
    )]);
    assert_eq!(b.owner_total_ns(Owner::DevTools), 0);
    assert_eq!(b.owner_total_ns(Owner::App), b.total_ns());
}

#[test]
fn a_dropped_parent_never_makes_a_child_negative() {
    // Ring overflow can leave a child whose parent was dropped, or a
    // parent whose measured span is shorter than the children attributed
    // to it (clock granularity at sub-microsecond scopes). Saturating
    // subtraction must clamp at zero rather than wrap to ~18 exaseconds.
    let b = block(&[
        ("telemetry.test.sat.child", ScopeKind::Native, 1, 9_000_000),
        ("telemetry.test.sat.parent", ScopeKind::Native, 0, 1_000_000),
    ]);
    assert_eq!(b.self_ns(1), 0);
}

// ── Allocation-free push (RFC-0013 P1: "no allocation in push") ────────

#[allow(unsafe_code)] // SAFETY: thin passthrough wrapper around `System`, test-only.
mod counting_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    // Thread-local, not a shared atomic: `cargo test` runs many unrelated
    // tests concurrently on other threads, all sharing one global
    // allocator, so a process-wide counter would be polluted by them.
    // Isolating the count per-thread lets this test see only the
    // allocations its own calling thread performed.
    thread_local! {
        static COUNT: Cell<usize> = const { Cell::new(0) };
    }

    /// Number of allocations observed on the calling thread since
    /// process start. Test-only: this crate ships no global allocator
    /// outside `cfg(test)`.
    pub fn count() -> usize {
        COUNT.with(Cell::get)
    }

    pub struct CountingAllocator;

    // SAFETY: forwards every call unchanged to `System`, which is
    // itself a valid `GlobalAlloc`; the only addition is a thread-local
    // counter increment with no effect on the allocation contract.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            COUNT.with(|c| c.set(c.get() + 1));
            // SAFETY: `layout` is passed through unchanged from the caller.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr`/`layout` are passed through unchanged from the caller.
            unsafe { System.dealloc(ptr, layout) }
        }
    }
}

#[global_allocator]
static GLOBAL: counting_alloc::CountingAllocator = counting_alloc::CountingAllocator;

#[test]
fn push_does_not_allocate() {
    let _ = drain_samples();
    // Warm the ring so any one-time thread-local init has already run.
    push_sample(Sample::default());
    let _ = drain_samples();

    let before = counting_alloc::count();
    for _ in 0..64 {
        push_sample(Sample::default());
    }
    let after = counting_alloc::count();
    assert_eq!(after, before, "push must not allocate");

    let _ = drain_samples();
}
