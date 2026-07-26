//! Telemetry overlay formatting for `byard dev` (RFC-0013 §"Overlay format",
//! RFC-0030 §I2/§I2b).
//!
//! Pure text formatting — no window, no `wgpu` device, fully unit-testable.
//! `byard dev`'s event loop (`commands/dev.rs`) drains and prints this
//! periodically on the render/main thread, combining two sources that live
//! on different threads:
//!
//! - the last published frame's CPU (`Interpreter`/`Native`) samples,
//!   drained on the **logic** thread at publish time
//!   ([`byard_core::Engine::latest_cpu_telemetry`]);
//! - the render/main thread's own ring, drained here via
//!   [`byard_core::telemetry::drain_samples`]. It carries this thread's
//!   `encode.frame` scope *and* the `Gpu`-tagged pass samples pushed by
//!   `EncoderSubsystem` — the latter never cross into the `RenderFrame`
//!   itself, since GPU timing resolves asynchronously, independent of any
//!   particular tick.
//!
//! # Two readings per row, and why both are shown
//!
//! Scopes nest: `layout.taffy` runs inside `interp.render`, and the
//! interpreter's fixpoint re-pulls run `interp.tick` inside it too. A flat sum
//! over such a set reports an 8 ms total for a 4 ms frame, and a profiler that
//! overstates the frame it measures is worse than no profiler. So:
//!
//! - the **total** sums only depth-0 samples, inclusively
//!   ([`SampleBlock::total_ns`]);
//! - each **row** shows self-time, with its inclusive time in parentheses when
//!   the two differ, and is indented by its nesting depth;
//! - the **interpreter tax** sums the self-time of `Interpreter` scopes only,
//!   so Taffy's cost — `Native`, but nested inside an `Interpreter` parent —
//!   is not billed to a tax an AOT build would not pay (RFC-0030 §I2b).
//!
//! Flat per-scope list by default (RFC-0013 **P2**); no AOT projection is
//! ever included unless a caller explicitly opts in by calling
//! [`byard_core::telemetry::project_aot`] itself and appending its own line
//! (**P3**) — this module never calls it.

use std::fmt::Write as _;

use byard_core::telemetry::{Sample, SampleBlock, ScopeKind, scope_kind, scope_name};

/// Column the duration is written at, so a row's indent never shifts its
/// number out of alignment.
const NAME_WIDTH: usize = 28;

/// Formats a per-scope telemetry breakdown for one tick.
///
/// `cpu` is the logic thread's [`SampleBlock`] for the most recently
/// published frame; `render` is the calling thread's own ring, holding its
/// `encode.frame` scope and this frame's `Gpu`-tagged samples (drained
/// separately — see the module docs for why). `gpu_available` selects between
/// listing the `Gpu` rows and a "GPU timing unavailable" notice (RFC-0013
/// **P5**) — pass [`byard_core::Engine::gpu_timing_available`]. It gates the
/// GPU rows **only**: a render-thread CPU scope is a real measurement on
/// every device and is always shown.
#[must_use]
pub fn format_telemetry_overlay(
    cpu: &SampleBlock,
    render: &SampleBlock,
    gpu_available: bool,
    atlas: byard_core::atlas::layout::path_counters::Counts,
) -> String {
    // Depth-0 inclusive on each CPU ring; `Gpu` samples are a separate
    // timeline (they resolve two frames later) and are totalled apart, per
    // RFC-0013 P5.
    let cpu_total = cpu.total_ns() + cpu_native_total_ns(render);
    let gpu_total = render.sum_by_kind(ScopeKind::Gpu);
    let interp_tax = cpu.interpreter_tax_ns() + render.interpreter_tax_ns();

    let mut out = format!(
        "measured total {}  (interp tax {})\n",
        fmt_ms(cpu_total),
        fmt_ms(interp_tax),
    );
    if gpu_total > 0 {
        let _ = writeln!(out, "gpu total      {}  (async, -2f)", fmt_ms(gpu_total));
    }

    write_scope_tree(&mut out, cpu);
    write_scope_tree(&mut out, render);
    write_atlas_line(&mut out, atlas);

    if gpu_available {
        for (i, sample) in render.samples.iter().enumerate() {
            if scope_kind(sample.scope) != Some(ScopeKind::Gpu) {
                continue;
            }
            let _ = writeln!(
                out,
                "  {:<NAME_WIDTH$} {}  (async, -2f)",
                scope_label(sample),
                fmt_ms(render.self_ns(i))
            );
        }
    } else {
        out.push_str("  GPU timing unavailable (device lacks TIMESTAMP_QUERY)\n");
    }

    let dropped = cpu.dropped + render.dropped;
    if dropped > 0 {
        let _ = writeln!(out, "  ({dropped} sample(s) dropped this tick — ring full)");
    }

    out
}

/// One line naming which layout path the frame took (RFC-0032 §R7).
///
/// The answer to "am I on the fast path?" belongs on the readout, not in an
/// inference from a timing that got smaller. `rebuild` on a frame where
/// nothing structural happened is the thing a developer wants to see and can
/// usually fix; `retained` with a handful of marked nodes is the healthy
/// steady state.
fn write_atlas_line(out: &mut String, atlas: byard_core::atlas::layout::path_counters::Counts) {
    if atlas.populate_calls == 0 {
        // Nothing rendered this tick — say nothing rather than print a row of
        // zeros that reads like "the fast path did nothing".
        return;
    }
    let path = if atlas.clears > 0 {
        "rebuild"
    } else {
        "retained"
    };
    let _ = writeln!(
        out,
        "  {:<NAME_WIDTH$} {path} · {} node(s) marked · {}/{} matched",
        "atlas",
        atlas.populate_dirty_targets,
        atlas.populate_dirty_matched,
        atlas.populate_dirty_targets,
    );
}

/// Writes `block`'s non-`Gpu` samples as an indented tree, parents first.
///
/// The ring holds samples in `Drop` order, so a child physically precedes its
/// parent. Printing them in that order puts `layout.taffy` *above* the
/// `interp.render` it ran inside, which reads as a sequence when it is a
/// containment — so the rows are walked as the forest they describe instead
/// (`for_each_root` + `for_each_direct_child`), leaving the block itself
/// untouched.
fn write_scope_tree(out: &mut String, block: &SampleBlock) {
    block.for_each_root(|i, sample| write_subtree(out, block, i, sample));
}

fn write_subtree(out: &mut String, block: &SampleBlock, index: usize, sample: &Sample) {
    if scope_kind(sample.scope) != Some(ScopeKind::Gpu) {
        write_row(out, block, index, sample);
    }
    block.for_each_direct_child(index, |child_index, child| {
        write_subtree(out, block, child_index, child);
    });
}

/// One row: self-time as the headline number, because that is the quantity
/// that sums to the frame, with the inclusive figure in parentheses whenever
/// a scope contains children — so the two readings are never confusable and
/// neither has to be reconstructed by the reader.
fn write_row(out: &mut String, block: &SampleBlock, index: usize, sample: &Sample) {
    let self_ns = block.self_ns(index);
    let inclusive_ns = sample.duration_ns();
    let indent = "  ".repeat(1 + usize::from(sample.depth()));
    let name_width = NAME_WIDTH.saturating_sub(2 * usize::from(sample.depth()));
    let _ = write!(
        out,
        "{indent}{:<name_width$} {}",
        scope_label(sample),
        fmt_ms(self_ns)
    );
    if inclusive_ns != self_ns {
        let _ = write!(out, "  ({} incl.)", fmt_ms(inclusive_ns));
    }
    if matches!(scope_kind(sample.scope), Some(ScopeKind::Interpreter)) {
        out.push_str("  [INTERPRETER — 0 in release]");
    }
    out.push('\n');
}

/// Depth-0 inclusive total of a block's **non-`Gpu`** samples — the CPU cost
/// of a ring that also carries GPU pass timings, which belong to a different
/// timeline and must not be added to a wall-clock frame total.
fn cpu_native_total_ns(block: &SampleBlock) -> u64 {
    block
        .samples
        .iter()
        .filter(|s| s.depth() == 0 && scope_kind(s.scope) != Some(ScopeKind::Gpu))
        .map(Sample::duration_ns)
        .sum()
}

fn scope_label(sample: &Sample) -> &'static str {
    scope_name(sample.scope).unwrap_or("<unknown scope>")
}

/// Three decimals, always in milliseconds.
///
/// Two decimals rounded `interp.dispatch_events` and `relay.publish` — both
/// genuinely a few microseconds — to a flat `0.00ms`, which is exactly the
/// reading "this scope is no longer being entered" produces. A profiler must
/// not render a working scope and a dead one identically. One unit throughout
/// (rather than switching to µs below a threshold) keeps the column
/// comparable at a glance, which is the whole reason the rows are aligned.
#[allow(clippy::cast_precision_loss)] // frame times never approach f64's precision limit
fn fmt_ms(ns: u64) -> String {
    format!("{:.3}ms", ns as f64 / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use byard_core::atlas::layout::path_counters::Counts;
    use byard_core::telemetry::{ScopeId, scope_id_tagged};

    fn sample(scope: ScopeId, duration_ns: u64) -> Sample {
        Sample::cpu(scope, 0, 0, duration_ns)
    }

    fn nested(scope: ScopeId, depth: u8, start: u64, end: u64) -> Sample {
        Sample::cpu(scope, depth, start, end)
    }

    /// A snapshot describing a retained frame that marked `marked` nodes.
    fn retained(marked: u64) -> Counts {
        Counts {
            clears: 0,
            full_computes: 0,
            retained_recomputes: 1,
            populate_calls: 1,
            populate_dirty_targets: marked,
            populate_dirty_matched: marked,
        }
    }

    #[test]
    fn the_atlas_line_names_the_path_the_frame_took() {
        let out = format_telemetry_overlay(
            &SampleBlock::default(),
            &SampleBlock::default(),
            true,
            retained(3),
        );
        assert!(out.contains("atlas"), "{out}");
        assert!(out.contains("retained"), "{out}");
        assert!(out.contains("3 node(s) marked"), "{out}");
    }

    #[test]
    fn the_atlas_line_says_rebuild_when_the_tree_was_torn_down() {
        let counts = Counts {
            clears: 1,
            full_computes: 1,
            ..retained(0)
        };
        let out = format_telemetry_overlay(
            &SampleBlock::default(),
            &SampleBlock::default(),
            true,
            counts,
        );
        assert!(out.contains("rebuild"), "{out}");
    }

    #[test]
    fn a_tick_that_rendered_nothing_prints_no_atlas_line() {
        // A row of zeros reads like "the fast path did nothing", which is a
        // different — and alarming — statement from "nothing rendered".
        let out = format_telemetry_overlay(
            &SampleBlock::default(),
            &SampleBlock::default(),
            true,
            Counts::default(),
        );
        assert!(!out.contains("atlas"), "{out}");
    }

    #[test]
    fn lists_every_cpu_scope_with_its_duration() {
        let native = scope_id_tagged("overlay.test.native_scope", ScopeKind::Native);
        let cpu = SampleBlock {
            samples: vec![sample(native, 1_500_000)],
            dropped: 0,
        };
        let out = format_telemetry_overlay(&cpu, &SampleBlock::default(), true, Counts::default());
        assert!(out.contains("overlay.test.native_scope"));
        assert!(out.contains("1.500ms"));
    }

    #[test]
    fn tags_interpreter_scopes_with_the_zero_in_release_note() {
        let interp = scope_id_tagged("overlay.test.interp_scope", ScopeKind::Interpreter);
        let cpu = SampleBlock {
            samples: vec![sample(interp, 2_000_000)],
            dropped: 0,
        };
        let out = format_telemetry_overlay(&cpu, &SampleBlock::default(), true, Counts::default());
        assert!(out.contains("[INTERPRETER — 0 in release]"));
        assert!(out.contains("interp tax 2.000ms"));
    }

    #[test]
    fn gpu_rows_are_marked_async_minus_two_frames() {
        let gpu_scope = scope_id_tagged("overlay.test.gpu_scope", ScopeKind::Gpu);
        let gpu = SampleBlock {
            samples: vec![Sample::gpu_duration(gpu_scope, 900_000)],
            dropped: 0,
        };
        let out = format_telemetry_overlay(&SampleBlock::default(), &gpu, true, Counts::default());
        assert!(out.contains("overlay.test.gpu_scope"));
        assert!(out.contains("(async, -2f)"));
    }

    #[test]
    fn shows_the_unavailable_notice_instead_of_fabricating_gpu_rows() {
        let gpu_scope = scope_id_tagged("overlay.test.gpu_scope_unavailable", ScopeKind::Gpu);
        let gpu = SampleBlock {
            samples: vec![Sample::gpu_duration(gpu_scope, 900_000)],
            dropped: 0,
        };
        let out = format_telemetry_overlay(&SampleBlock::default(), &gpu, false, Counts::default());
        assert!(out.contains("GPU timing unavailable"));
        assert!(
            !out.contains("overlay.test.gpu_scope_unavailable"),
            "an unavailable timer must never report fabricated GPU rows"
        );
    }

    #[test]
    fn reports_dropped_samples_from_either_ring() {
        let cpu = SampleBlock {
            samples: vec![],
            dropped: 3,
        };
        let gpu = SampleBlock {
            samples: vec![],
            dropped: 2,
        };
        let out = format_telemetry_overlay(&cpu, &gpu, true, Counts::default());
        assert!(out.contains("5 sample(s) dropped"));
    }

    #[test]
    fn never_includes_an_aot_projection_by_default() {
        // RFC-0013 P3: the projection is opt-in; this formatter never calls
        // `project_aot` itself, so its output must never claim a projected
        // number unless a caller appends one explicitly (out of scope here).
        let out = format_telemetry_overlay(
            &SampleBlock::default(),
            &SampleBlock::default(),
            true,
            Counts::default(),
        );
        assert!(!out.to_lowercase().contains("proj"));
    }

    // ── RFC-0030 §I2 / §I2b ────────────────────────────────────────────────

    #[test]
    fn the_total_is_the_frame_not_the_sum_of_every_nested_row() {
        // `interp.render` (10ms, depth 0) containing `layout.taffy` (4ms,
        // depth 1). A flat sum would report 14ms for a 10ms frame.
        let parent = scope_id_tagged("overlay.test.nest.parent", ScopeKind::Interpreter);
        let child = scope_id_tagged("overlay.test.nest.child", ScopeKind::Native);
        let cpu = SampleBlock {
            // Drop order: the child's guard drops first, so it precedes its
            // parent in the block.
            samples: vec![
                nested(child, 1, 1_000_000, 5_000_000),
                nested(parent, 0, 0, 10_000_000),
            ],
            dropped: 0,
        };
        let out = format_telemetry_overlay(&cpu, &SampleBlock::default(), true, Counts::default());
        assert!(
            out.contains("measured total 10.000ms"),
            "expected a 10ms frame total, got:\n{out}"
        );
        assert!(!out.contains("14.000ms"), "double-counted:\n{out}");
    }

    #[test]
    fn a_parent_row_shows_self_time_with_its_inclusive_time_beside_it() {
        let parent = scope_id_tagged("overlay.test.rows.parent", ScopeKind::Interpreter);
        let child = scope_id_tagged("overlay.test.rows.child", ScopeKind::Native);
        let cpu = SampleBlock {
            samples: vec![
                nested(child, 1, 1_000_000, 5_000_000),
                nested(parent, 0, 0, 10_000_000),
            ],
            dropped: 0,
        };
        let out = format_telemetry_overlay(&cpu, &SampleBlock::default(), true, Counts::default());
        assert!(
            out.contains("overlay.test.rows.parent") && out.contains("6.000ms  (10.000ms incl.)"),
            "expected 6ms self / 10ms inclusive, got:\n{out}"
        );
        // A leaf has no children, so it carries no parenthesised second
        // reading — the two numbers would be identical and the noise would
        // make the parents' annotation less legible.
        assert!(
            out.contains("    overlay.test.rows.child"),
            "a depth-1 row is indented beneath its parent:\n{out}"
        );
        assert!(!out.contains("4.000ms  (4.000ms incl.)"), "noise:\n{out}");
    }

    #[test]
    fn the_interpreter_tax_excludes_a_native_child_of_an_interpreter_parent() {
        // The load-bearing case: layout is Native but nests inside render,
        // which is Interpreter. An AOT build still pays for layout, so the
        // tax is 10ms − 4ms, never 10ms.
        let parent = scope_id_tagged("overlay.test.tax.parent", ScopeKind::Interpreter);
        let child = scope_id_tagged("overlay.test.tax.child", ScopeKind::Native);
        let cpu = SampleBlock {
            samples: vec![
                nested(child, 1, 1_000_000, 5_000_000),
                nested(parent, 0, 0, 10_000_000),
            ],
            dropped: 0,
        };
        let out = format_telemetry_overlay(&cpu, &SampleBlock::default(), true, Counts::default());
        assert!(
            out.contains("interp tax 6.000ms"),
            "expected the tax to exclude the nested native child, got:\n{out}"
        );
    }

    #[test]
    fn a_render_thread_cpu_scope_is_never_labelled_a_gpu_pass() {
        // `encode.frame` shares the render thread's ring with the GPU pass
        // samples. It is a wall-clock CPU measurement, so it must be listed
        // plainly, counted in the frame total, and shown even on a device
        // with no timestamp query.
        let encode = scope_id_tagged("overlay.test.render.encode", ScopeKind::Native);
        let gpu_scope = scope_id_tagged("overlay.test.render.gpu", ScopeKind::Gpu);
        let render = SampleBlock {
            samples: vec![
                Sample::gpu_duration(gpu_scope, 700_000),
                sample(encode, 300_000),
            ],
            dropped: 0,
        };
        let out =
            format_telemetry_overlay(&SampleBlock::default(), &render, false, Counts::default());
        assert!(
            out.contains("overlay.test.render.encode"),
            "a render-thread CPU scope must survive an unavailable GPU timer:\n{out}"
        );
        assert!(
            out.contains("measured total 0.300ms"),
            "the CPU total must include the render thread and exclude the GPU pass:\n{out}"
        );
        let encode_row = out
            .lines()
            .find(|l| l.contains("overlay.test.render.encode"))
            .expect("encode row");
        assert!(
            !encode_row.contains("async"),
            "a CPU scope must not be labelled as an async GPU pass: {encode_row}"
        );
    }

    #[test]
    fn rows_are_printed_parent_first_even_though_children_are_stored_first() {
        // The ring stores `layout.taffy` before `interp.render` (drop order).
        // Printed in that order the child appears *above* the scope that
        // contains it, which reads as a sequence rather than a containment.
        let parent = scope_id_tagged("overlay.test.order.parent", ScopeKind::Interpreter);
        let child = scope_id_tagged("overlay.test.order.child", ScopeKind::Native);
        let after = scope_id_tagged("overlay.test.order.after", ScopeKind::Native);
        let cpu = SampleBlock {
            samples: vec![
                nested(child, 1, 1_000_000, 5_000_000),
                nested(parent, 0, 0, 10_000_000),
                nested(after, 0, 10_000_000, 11_000_000),
            ],
            dropped: 0,
        };
        let out = format_telemetry_overlay(&cpu, &SampleBlock::default(), true, Counts::default());
        let row = |name: &str| out.lines().position(|l| l.contains(name)).expect(name);
        assert!(
            row("overlay.test.order.parent") < row("overlay.test.order.child"),
            "a parent must be printed above the child it contains:\n{out}"
        );
        assert!(
            row("overlay.test.order.child") < row("overlay.test.order.after"),
            "a subtree must stay together, ahead of the next top-level scope:\n{out}"
        );
    }

    #[test]
    fn a_few_microseconds_is_not_rendered_the_same_as_a_dead_scope() {
        // A scope that stopped being entered and one that costs 4µs must not
        // print identically — the first is the failure this whole slice
        // exists to make visible.
        let alive = scope_id_tagged("overlay.test.precision.alive", ScopeKind::Native);
        let cpu = SampleBlock {
            samples: vec![sample(alive, 4_000)],
            dropped: 0,
        };
        let out = format_telemetry_overlay(&cpu, &SampleBlock::default(), true, Counts::default());
        assert!(
            out.contains("0.004ms"),
            "expected µs resolution, got:\n{out}"
        );
    }

    #[test]
    fn gpu_time_is_totalled_apart_from_the_cpu_frame() {
        let gpu_scope = scope_id_tagged("overlay.test.total.gpu", ScopeKind::Gpu);
        let render = SampleBlock {
            samples: vec![Sample::gpu_duration(gpu_scope, 700_000)],
            dropped: 0,
        };
        let cpu = SampleBlock {
            samples: vec![sample(
                scope_id_tagged("overlay.test.total.cpu", ScopeKind::Native),
                2_000_000,
            )],
            dropped: 0,
        };
        let out = format_telemetry_overlay(&cpu, &render, true, Counts::default());
        assert!(out.contains("measured total 2.000ms"), "{out}");
        assert!(out.contains("gpu total      0.700ms"), "{out}");
    }
}
