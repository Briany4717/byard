//! The expanded `--profile` block for `byard dev` (RFC-0030 §V1, §I2/§I2b;
//! RFC-0013 §"Overlay format").
//!
//! ```text
//!  frame   3.500ms ▏ budget  16.667ms    21%
//!  relay.publish              0.002ms ░░░░░░░░░░░░░░░░░░░░
//!  interp.dispatch_events     0.000ms ░░░░░░░░░░░░░░░░░░░░
//!  interp.tick                0.030ms ░░░░░░░░░░░░░░░░░░░░
//!  interp.render              0.230ms ░░░░░░░░░░░░░░░░░░░░  (0.630ms incl.)
//!    layout.taffy             0.400ms ░░░░░░░░░░░░░░░░░░░░
//!  present.acquire            9.920ms ███████████░░░░░░░░░  idle
//!  encode.frame               0.600ms ░░░░░░░░░░░░░░░░░░░░  (6.550ms incl.)
//!    encode.glyphs            5.690ms ██████░░░░░░░░░░░░░░
//!  …
//!  interpreter tax            0.260ms  self-time, 3 scope(s) — 0 in release
//!  layout path               retained  3 node(s) marked · 3/3 matched
//! ```
//!
//! # Pure formatting
//!
//! No window, no `wgpu` device, fully unit-testable. `byard dev`'s event loop
//! composes this and hands it to `statusline`'s painter, which is the only
//! thing in the CLI that writes control sequences.
//!
//! # Two readings per row, and why both are shown
//!
//! Scopes nest: `layout.taffy` runs inside `interp.render`, and `encode.glyphs`
//! inside `encode.frame`. A flat sum over such a set reports an 8 ms total for
//! a 4 ms frame, and a profiler that overstates the frame it measures is worse
//! than no profiler. So:
//!
//! - each **row** charts self-time, with its inclusive time in parentheses when
//!   the two differ;
//! - the **interpreter tax** sums the self-time of `Interpreter` scopes only,
//!   so Taffy's cost — `Native`, but nested inside an `Interpreter` parent — is
//!   not billed to a tax an AOT build would not pay (§I2b).
//!
//! # The row order is fixed, and the row *count* is constant
//!
//! Two separate properties, both load-bearing, both easy to lose.
//!
//! **Fixed order.** Rows are listed in the order the frame executes them, never
//! sorted by duration. A self-sorting profiler is unreadable in real time: rows
//! swap places between repaints, so the eye has to re-find every row it was
//! tracking, ten times a second.
//!
//! **Constant count.** The block redraws in place by moving the cursor up N
//! lines. If N changed between repaints the redraw would erase the wrong lines
//! and eat scrollback — which is where parse errors live, and the reason this
//! design exists at all. So [`ROWS`] is a *schema*: a scope that did not run
//! this frame still gets its row, showing zero, and a scope that ran several
//! times (`encode.buffers`, once per draw group) is aggregated into one. The
//! aggregate carries its occurrence count, so nothing is hidden by it.
//!
//! Showing an absent scope as `0.000ms` is also the honest rendering: "this
//! scope was not entered" is exactly the failure the instrumentation exists to
//! make visible, and a row that silently disappears communicates it to nobody.
//!
//! No AOT projection is ever included (RFC-0013 **P3**): it is opt-in by
//! design and this module never calls `project_aot`.

use std::fmt::Write as _;

use byard_core::telemetry::{SampleBlock, ScopeKind, scope_kind, scope_name};

use crate::style::{Glyphs, Palette};

/// Bar width in cells (§V1).
const BAR_CELLS: usize = 20;
/// Column the timing starts at, so an indented row's number stays aligned.
const NAME_WIDTH: usize = 24;

/// The frame's scopes, in execution order, with their nesting depth.
///
/// This is a schema, not a discovery: see the module docs for why the row set
/// has to be fixed rather than derived from whatever happened to be sampled.
/// Adding a `profile_scope!` without adding it here means it will not appear —
/// which is deliberate friction, because a row appearing and disappearing
/// between repaints is the exact failure this table prevents.
const ROWS: &[(&str, u8)] = &[
    ("relay.publish", 0),
    ("interp.dispatch_events", 0),
    ("interp.tick", 0),
    ("interp.render", 0),
    ("layout.taffy", 1),
    ("present.acquire", 0),
    ("encode.frame", 0),
    ("encode.uploads", 1),
    ("encode.glyphs", 1),
    ("encode.passes", 1),
    ("encode.buffers", 2),
    ("encode.submit", 0),
    ("present.submit", 0),
    ("gpu.ui_pass", 0),
];

/// How many terminal lines [`format_profile_block`] always produces: the
/// header, every schema row, and the two footer lines.
///
/// Constant by construction, and asserted — the in-place redraw depends on it.
pub const PROFILE_LINES: usize = ROWS.len() + 3;

/// One row's aggregated timings.
#[derive(Default, Clone, Copy)]
struct Row {
    self_ns: u64,
    inclusive_ns: u64,
    occurrences: u32,
    kind: Option<ScopeKind>,
}

/// What the block needs beyond the sample blocks themselves.
#[derive(Debug, Clone, Copy)]
pub struct ProfileContext {
    /// The frame budget in nanoseconds — the display's refresh interval unless
    /// `[dev] frame_budget` overrode it (RFC-0030 §V2/§Q3).
    pub budget_ns: u64,
    /// The frame period, as the developer perceives it.
    pub frame_ns: u64,
    /// Whether the device can time GPU passes at all (RFC-0013 **P5**).
    pub gpu_available: bool,
    /// Which layout path the frame took (RFC-0032 §R7).
    pub atlas: byard_core::atlas::layout::path_counters::Counts,
}

/// Formats the expanded profile block: always [`PROFILE_LINES`] lines, with no
/// trailing newline.
#[must_use]
pub fn format_profile_block(
    logic: &SampleBlock,
    render: &SampleBlock,
    ctx: ProfileContext,
    p: &Palette,
    g: &Glyphs,
) -> String {
    let rows = aggregate(logic, render);
    let mut out = String::with_capacity(PROFILE_LINES * 96);

    write_header(&mut out, &ctx, p, g);
    for (i, (name, depth)) in ROWS.iter().enumerate() {
        out.push('\n');
        write_row(&mut out, name, *depth, rows[i], &ctx, p);
    }
    out.push('\n');
    write_tax(&mut out, &rows, logic, render, p);
    out.push('\n');
    write_atlas(&mut out, &ctx, p);

    debug_assert_eq!(
        out.lines().count(),
        PROFILE_LINES,
        "the in-place redraw depends on a constant line count"
    );
    out
}

/// Sums each schema row's samples across both threads' blocks.
fn aggregate(logic: &SampleBlock, render: &SampleBlock) -> [Row; ROWS.len()] {
    let mut rows = [Row::default(); ROWS.len()];
    for block in [logic, render] {
        for (i, sample) in block.samples.iter().enumerate() {
            let Some(name) = scope_name(sample.scope) else {
                continue;
            };
            let Some(slot) = ROWS.iter().position(|(n, _)| *n == name) else {
                continue;
            };
            let row = &mut rows[slot];
            row.self_ns += block.self_ns(i);
            row.inclusive_ns += sample.duration_ns();
            row.occurrences += 1;
            row.kind = scope_kind(sample.scope);
        }
    }
    rows
}

fn write_header(out: &mut String, ctx: &ProfileContext, p: &Palette, g: &Glyphs) {
    let over = ctx.frame_ns > ctx.budget_ns;
    let colour = if over { p.err } else { p.ok };
    let pct = ctx
        .frame_ns
        .saturating_mul(100)
        .checked_div(ctx.budget_ns)
        .unwrap_or(0)
        .min(9999);
    let _ = write!(
        out,
        " {colour}frame {:>9}{} {}{}{} budget {:>9}  {colour}{pct:>4}%{}",
        fmt_ms(ctx.frame_ns),
        p.reset,
        p.dim,
        g.fact,
        p.reset,
        fmt_ms(ctx.budget_ns),
        p.reset,
    );
}

fn write_row(out: &mut String, name: &str, depth: u8, row: Row, ctx: &ProfileContext, p: &Palette) {
    let indent = usize::from(depth) * 2;
    let width = NAME_WIDTH.saturating_sub(indent);
    // A row whose self-time *alone* exceeds the budget is the finding; anything
    // else, however large, is just a share of a frame that fits.
    let over = row.self_ns > ctx.budget_ns;
    let colour = if over {
        p.err
    } else if row.occurrences == 0 {
        p.dim
    } else {
        p.reset
    };
    let _ = write!(out, " {:indent$}{colour}{name:<width$}{}", "", p.reset);
    let _ = write!(out, " {}{:>9}{} ", p.metric, fmt_ms(row.self_ns), p.reset);
    write_bar(out, row.self_ns, ctx.budget_ns, colour, p);

    // A GPU row on a device with no timestamp query is not zero, it is
    // *unknown*, and printing `0.000ms` for it would be a fabricated
    // measurement rather than a missing one.
    if row.kind == Some(ScopeKind::Gpu) || name.starts_with("gpu.") {
        if ctx.gpu_available {
            let _ = write!(out, "  {}async −2f{}", p.dim, p.reset);
        } else {
            let _ = write!(out, "  {}no timestamp query{}", p.dim, p.reset);
        }
    } else if row.inclusive_ns != row.self_ns {
        let _ = write!(
            out,
            "  {}({} incl.){}",
            p.dim,
            fmt_ms(row.inclusive_ns),
            p.reset
        );
    } else if row.occurrences > 1 {
        // The aggregate says so rather than quietly presenting several samples
        // as one.
        let _ = write!(out, "  {}×{}{}", p.dim, row.occurrences, p.reset);
    } else if name == "present.acquire" && row.occurrences > 0 {
        // The one row whose *large* value is health rather than cost: the
        // engine finished early and the display made it wait.
        let _ = write!(out, "  {}idle{}", p.dim, p.reset);
    }
}

/// Draws a 20-cell bar filled to `value / budget`.
///
/// Against the **budget**, never against the largest row. Bars scaled to the
/// biggest thing on screen are always full somewhere and therefore say nothing
/// about whether the frame is in trouble; against a budget, a fast frame
/// renders as visibly empty space, which is the point.
fn write_bar(out: &mut String, value_ns: u64, budget_ns: u64, colour: &str, p: &Palette) {
    let budget = budget_ns.max(1);
    let cells = BAR_CELLS as u64;
    let filled = usize::try_from((value_ns.saturating_mul(cells)) / budget).unwrap_or(BAR_CELLS);
    let filled = filled.min(BAR_CELLS);
    out.push_str(colour);
    for _ in 0..filled {
        out.push('█');
    }
    out.push_str(p.dim);
    for _ in filled..BAR_CELLS {
        out.push('░');
    }
    out.push_str(p.reset);
}

fn write_tax(
    out: &mut String,
    rows: &[Row; ROWS.len()],
    logic: &SampleBlock,
    render: &SampleBlock,
    p: &Palette,
) {
    let tax = logic.interpreter_tax_ns() + render.interpreter_tax_ns();
    let scopes = rows
        .iter()
        .filter(|r| r.kind == Some(ScopeKind::Interpreter) && r.occurrences > 0)
        .count();
    let _ = write!(
        out,
        " {:NAME_WIDTH$} {}{:>9}{}  {}self-time, {scopes} scope(s) — 0 in release{}",
        "interpreter tax",
        p.metric,
        fmt_ms(tax),
        p.reset,
        p.dim,
        p.reset
    );
}

fn write_atlas(out: &mut String, ctx: &ProfileContext, p: &Palette) {
    let (path, colour) = if ctx.atlas.populate_calls == 0 {
        ("idle", p.dim)
    } else if ctx.atlas.clears > 0 {
        ("rebuild", p.warn)
    } else {
        ("retained", p.ok)
    };
    let _ = write!(
        out,
        " {:NAME_WIDTH$} {colour}{path:>9}{}  {}{} marked · {}/{} matched{}",
        "layout path",
        p.reset,
        p.dim,
        ctx.atlas.populate_dirty_targets,
        ctx.atlas.populate_dirty_matched,
        ctx.atlas.populate_dirty_targets,
        p.reset
    );
}

/// Three decimals, always milliseconds.
///
/// Two decimals rounded `interp.dispatch_events` and `relay.publish` — both
/// genuinely a few microseconds — to a flat `0.00ms`, which is exactly the
/// reading "this scope is no longer being entered" produces. A profiler must
/// not render a working scope and a dead one identically.
#[allow(clippy::cast_precision_loss)] // frame times never approach f64's limit
fn fmt_ms(ns: u64) -> String {
    format!("{:.3}ms", ns as f64 / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::display_width;
    use byard_core::atlas::layout::path_counters::Counts;
    use byard_core::telemetry::{Sample, ScopeId, scope_id_tagged};

    const BUDGET: u64 = 16_667_000;

    fn ctx() -> ProfileContext {
        ProfileContext {
            budget_ns: BUDGET,
            frame_ns: 3_500_000,
            gpu_available: true,
            atlas: Counts {
                clears: 0,
                populate_calls: 1,
                populate_dirty_targets: 3,
                populate_dirty_matched: 3,
                ..Default::default()
            },
        }
    }

    fn scope(name: &'static str, kind: ScopeKind) -> ScopeId {
        scope_id_tagged(name, kind)
    }

    fn show(logic: &SampleBlock, render: &SampleBlock, ctx: ProfileContext) -> String {
        format_profile_block(logic, render, ctx, &Palette::plain(), &Glyphs::unicode())
    }

    fn block(samples: Vec<Sample>) -> SampleBlock {
        SampleBlock {
            samples,
            dropped: 0,
        }
    }

    #[test]
    fn the_line_count_is_constant_whatever_was_sampled() {
        // The in-place redraw moves the cursor up N lines. If N varied, a
        // repaint would erase the wrong lines and eat scrollback — which is
        // where parse errors live, and the reason this whole design exists.
        let empty = show(&SampleBlock::default(), &SampleBlock::default(), ctx());
        assert_eq!(empty.lines().count(), PROFILE_LINES);

        let busy = block(vec![
            Sample::cpu(scope("layout.taffy", ScopeKind::Native), 1, 1_000, 401_000),
            Sample::cpu(
                scope("interp.render", ScopeKind::Interpreter),
                0,
                0,
                630_000,
            ),
            // Three draw groups: three `encode.buffers` samples, one row.
            Sample::cpu(scope("encode.buffers", ScopeKind::Native), 2, 0, 100_000),
            Sample::cpu(scope("encode.buffers", ScopeKind::Native), 2, 0, 90_000),
            Sample::cpu(scope("encode.buffers", ScopeKind::Native), 2, 0, 40_000),
        ]);
        let out = show(&busy, &SampleBlock::default(), ctx());
        assert_eq!(out.lines().count(), PROFILE_LINES);
        assert_eq!(
            out.matches("encode.buffers").count(),
            1,
            "repeated samples must aggregate into one row:\n{out}"
        );
        assert!(
            out.contains("×3"),
            "and the aggregate must say how many it folded:\n{out}"
        );
    }

    #[test]
    fn rows_are_in_execution_order_and_never_sorted_by_duration() {
        // A self-sorting profiler is unreadable at 10 Hz: rows swap places
        // between repaints, so the eye re-finds every row it was tracking.
        let b = block(vec![
            // First in the schema and by far the smallest.
            Sample::cpu(scope("relay.publish", ScopeKind::Native), 0, 0, 2_000),
            // Much later in the schema and by far the largest.
            Sample::cpu(scope("present.acquire", ScopeKind::Native), 0, 0, 9_900_000),
        ]);
        let out = show(&b, &SampleBlock::default(), ctx());
        let at = |n: &str| out.lines().position(|l| l.contains(n)).expect(n);
        assert!(at("relay.publish") < at("present.acquire"));
        assert!(at("interp.render") < at("layout.taffy"));
        assert!(at("encode.frame") < at("encode.glyphs"));
    }

    #[test]
    fn a_parents_self_plus_its_childrens_inclusive_is_its_own_inclusive() {
        // The arithmetic the whole block rests on.
        let s = |n: &'static str, d, a, b| Sample::cpu(scope(n, ScopeKind::Native), d, a, b);
        let b = block(vec![
            s("encode.uploads", 1, 10_000, 20_000),
            s("encode.glyphs", 1, 20_000, 5_710_000),
            s("encode.passes", 1, 5_710_000, 5_960_000),
            s("encode.frame", 0, 0, 6_550_000),
        ]);
        let rows = aggregate(&SampleBlock::default(), &b);
        let idx = |n: &str| ROWS.iter().position(|(x, _)| *x == n).unwrap();
        let frame = rows[idx("encode.frame")];
        let children: u64 = ["encode.uploads", "encode.glyphs", "encode.passes"]
            .iter()
            .map(|n| rows[idx(n)].inclusive_ns)
            .sum();
        assert_eq!(frame.self_ns + children, frame.inclusive_ns);
        assert_eq!(frame.self_ns, 600_000);
    }

    #[test]
    fn an_over_budget_row_renders_in_err() {
        let b = block(vec![Sample::cpu(
            scope("encode.glyphs", ScopeKind::Native),
            1,
            0,
            45_000_000,
        )]);
        let p = Palette::ansi();
        let out = format_profile_block(
            &b,
            &SampleBlock::default(),
            ProfileContext {
                frame_ns: 46_000_000,
                ..ctx()
            },
            &p,
            &Glyphs::unicode(),
        );
        let row = out
            .lines()
            .find(|l| l.contains("encode.glyphs"))
            .expect("row");
        assert!(
            row.contains(p.err),
            "a row that alone exceeds the budget must be unmissable: {row}"
        );
        let header = out.lines().next().unwrap();
        assert!(header.contains(p.err), "and so must the frame: {header}");
    }

    #[test]
    fn bars_are_drawn_against_the_budget_not_the_largest_row() {
        // Bars scaled to the biggest thing on screen are always full somewhere
        // and say nothing about whether the frame is in trouble.
        let small = block(vec![Sample::cpu(
            scope("interp.tick", ScopeKind::Interpreter),
            0,
            0,
            100_000,
        )]);
        let out = show(&small, &SampleBlock::default(), ctx());
        let row = out.lines().find(|l| l.contains("interp.tick")).unwrap();
        assert!(
            !row.contains('█'),
            "0.1ms against a 16.7ms budget must render as empty space: {row}"
        );
        // And the only row present is still drawn at full width, so the column
        // does not rescale itself between repaints.
        assert_eq!(
            row.chars().filter(|c| *c == '░' || *c == '█').count(),
            BAR_CELLS
        );
    }

    #[test]
    fn a_scope_that_did_not_run_still_gets_its_row() {
        // "This scope was not entered" is exactly the failure the
        // instrumentation exists to surface. A row that silently disappears
        // communicates it to nobody — and would change the line count.
        let out = show(&SampleBlock::default(), &SampleBlock::default(), ctx());
        for (name, _) in ROWS {
            assert!(out.contains(name), "missing row {name}:\n{out}");
        }
        assert!(out.contains("0.000ms"));
    }

    #[test]
    fn the_interpreter_tax_excludes_a_native_child_of_an_interpreter_parent() {
        // The load-bearing case: layout is Native but nests inside render,
        // which is Interpreter. An AOT build still pays for Taffy in full, so
        // the tax is 10ms − 4ms, never 10ms.
        let b = block(vec![
            Sample::cpu(
                scope("layout.taffy", ScopeKind::Native),
                1,
                1_000_000,
                5_000_000,
            ),
            Sample::cpu(
                scope("interp.render", ScopeKind::Interpreter),
                0,
                0,
                10_000_000,
            ),
        ]);
        let out = show(&b, &SampleBlock::default(), ctx());
        let row = out
            .lines()
            .find(|l| l.contains("interpreter tax"))
            .expect("tax row");
        assert!(row.contains("6.000ms"), "{row}");
    }

    #[test]
    fn an_unavailable_gpu_timer_reports_unknown_rather_than_zero() {
        let out = show(
            &SampleBlock::default(),
            &SampleBlock::default(),
            ProfileContext {
                gpu_available: false,
                ..ctx()
            },
        );
        let row = out.lines().find(|l| l.contains("gpu.ui_pass")).unwrap();
        assert!(
            row.contains("no timestamp query"),
            "a device that cannot time GPU passes must not be reported as one \
             that measured zero: {row}"
        );
    }

    #[test]
    fn the_layout_path_is_named_not_inferred() {
        let retained = show(&SampleBlock::default(), &SampleBlock::default(), ctx());
        assert!(retained.contains("retained"), "{retained}");
        let rebuilt = show(
            &SampleBlock::default(),
            &SampleBlock::default(),
            ProfileContext {
                atlas: Counts {
                    clears: 1,
                    populate_calls: 1,
                    ..Default::default()
                },
                ..ctx()
            },
        );
        assert!(rebuilt.contains("rebuild"), "{rebuilt}");
    }

    #[test]
    fn every_line_fits_eighty_columns() {
        // Composed, not detected, for the same reason the statusline is (§Q7).
        // A wrapped row breaks the cursor-up redraw the same way a wrapped
        // statusline does.
        let b = block(vec![
            Sample::cpu(scope("encode.frame", ScopeKind::Native), 0, 0, 999_000_000),
            Sample::cpu(scope("encode.glyphs", ScopeKind::Native), 1, 0, 998_000_000),
        ]);
        for p in [Palette::plain(), Palette::ansi()] {
            let out = format_profile_block(
                &b,
                &SampleBlock::default(),
                ProfileContext {
                    frame_ns: 999_000_000,
                    ..ctx()
                },
                &p,
                &Glyphs::unicode(),
            );
            for line in out.lines() {
                assert!(
                    display_width(line) <= 80,
                    "{} columns: {line:?}",
                    display_width(line)
                );
            }
        }
    }

    #[test]
    fn never_includes_an_aot_projection() {
        // RFC-0013 P3: the projection is opt-in, and this module never calls
        // `project_aot` itself.
        let out = show(&SampleBlock::default(), &SampleBlock::default(), ctx());
        assert!(!out.to_lowercase().contains("projection"));
    }

    #[test]
    fn a_few_microseconds_is_not_rendered_the_same_as_a_dead_scope() {
        // A scope that stopped being entered and one that costs 4µs must not
        // print identically — the first is the failure this exists to reveal.
        let b = block(vec![Sample::cpu(
            scope("relay.publish", ScopeKind::Native),
            0,
            0,
            4_000,
        )]);
        let out = show(&b, &SampleBlock::default(), ctx());
        let row = out.lines().find(|l| l.contains("relay.publish")).unwrap();
        assert!(row.contains("0.004ms"), "{row}");
    }

    #[test]
    fn a_gpu_sample_is_never_added_to_the_cpu_frame() {
        // The GPU timeline resolves two frames later against a different
        // clock. Its row exists; its number must not be folded into anything
        // the CPU rows sum to.
        let b = block(vec![
            Sample::gpu_duration(scope("gpu.ui_pass", ScopeKind::Gpu), 700_000),
            Sample::cpu(scope("encode.frame", ScopeKind::Native), 0, 0, 300_000),
        ]);
        let rows = aggregate(&SampleBlock::default(), &b);
        let idx = |n: &str| ROWS.iter().position(|(x, _)| *x == n).unwrap();
        assert_eq!(rows[idx("gpu.ui_pass")].kind, Some(ScopeKind::Gpu));
        assert_eq!(rows[idx("encode.frame")].self_ns, 300_000);
    }
}
