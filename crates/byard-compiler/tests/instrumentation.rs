//! The instrumentation floor (RFC-0030 §I1/§I2/§I2b), asserted against the
//! **real** per-frame call sequence rather than against the profiler in
//! isolation.
//!
//! RFC-0013 shipped a complete, tested, zero-allocation profiler and then went
//! unused: `profile_scope!` had exactly one call site in the whole engine, and
//! it was inside a `#[cfg(test)]` block. A measurement facility nobody walks
//! past is indistinguishable from one that does not exist, and a scope that
//! silently stops being entered is indistinguishable from a subsystem that got
//! fast. So the scopes get the same treatment any other load-bearing path
//! gets: an assertion that fails if production stops producing them.
//!
//! What this file guards, specifically:
//!
//! - the logic thread's four scopes are entered by `dispatch_events` / `tick` /
//!   `render`, on an ordinary frame, with no test-only wiring;
//! - `layout.taffy` really does nest inside `interp.render`, the structural
//!   fact §I2b's correction rests on;
//! - the frame total is the frame, not the sum of every nested row;
//! - and the interpreter tax excludes the `Native` layout work nested inside
//!   its `Interpreter` parent, which is the number the RFC-0014 JIT decision
//!   will eventually be argued from.
//!
//! The GPU and encoder scopes live on the render thread and are covered by
//! `byard-core`'s own tests; nothing here needs a device.

#![cfg(feature = "telemetry")]

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use byard_core::telemetry::{
    Sample, SampleBlock, ScopeKind, drain_samples, scope_kind, scope_name,
};
use byard_core::{EventKind, InputEvent};

const W: f32 = 800.0;
const H: f32 = 600.0;

/// A view with a reactive value, a wrapping `Text` (so the measure path runs)
/// and a tappable region, enough that every scope below has real work to do.
const SOURCE: &str = r#"
View Probe() {
    var count = 0
    Column #[padding: 16, gap: 8] {
        Text("tapped ${count} times") #[size: 18]
        Text("A longer paragraph so the text measure protocol runs inside layout rather than short-circuiting on a single word.") #[size: 14]
        Box #[width: 120, height: 40, bg: 0x3366FF] {
            tap => count = count + 1
        }
    }
}
"#;

fn build() -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(SOURCE);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    (interp, tree)
}

fn tap(pos: (f32, f32), t: u64) -> InputEvent {
    InputEvent {
        kind: EventKind::Tap,
        pos,
        delta: (0.0, 0.0),
        payload: None,
        time_ms: t,
    }
}

/// Runs one full frame of the runner loop and returns the samples it produced.
///
/// The ring is drained (and discarded) first so the block holds this frame and
/// nothing that a previous phase of the same test left behind, exactly what
/// `Relay::publish` does before every swap.
fn profiled_frame(interp: &mut Interpreter, tree: &[RenderNode], inputs: &[InputEvent]) -> Samples {
    let _ = drain_samples();
    interp.dispatch_events(inputs);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(tree, &mut frame, W, H);
    Samples(drain_samples())
}

/// A drained block with the lookups these assertions need.
struct Samples(SampleBlock);

impl Samples {
    fn names(&self) -> Vec<&'static str> {
        self.0
            .samples
            .iter()
            .map(|s| scope_name(s.scope).unwrap_or("<unknown>"))
            .collect()
    }

    fn index_of(&self, name: &str) -> usize {
        self.0
            .samples
            .iter()
            .position(|s| scope_name(s.scope) == Some(name))
            .unwrap_or_else(|| {
                panic!(
                    "scope {name:?} was never entered on a real frame, production has stopped \
                     taking the instrumented path. Scopes seen: {:?}",
                    self.names()
                )
            })
    }

    fn sample(&self, name: &str) -> &Sample {
        &self.0.samples[self.index_of(name)]
    }
}

#[test]
fn a_real_frame_enters_every_logic_thread_scope() {
    let (mut interp, tree) = build();
    let samples = profiled_frame(&mut interp, &tree, &[tap((60.0, 200.0), 10)]);

    for name in [
        "interp.dispatch_events",
        "interp.tick",
        "interp.render",
        "layout.taffy",
    ] {
        let sample = samples.sample(name);
        assert!(
            sample.end >= sample.start,
            "{name} recorded a span that ends before it starts"
        );
    }
}

#[test]
fn the_scopes_carry_the_cost_bucket_their_call_site_declared() {
    let (mut interp, tree) = build();
    let samples = profiled_frame(&mut interp, &tree, &[]);

    for name in ["interp.dispatch_events", "interp.tick", "interp.render"] {
        assert_eq!(
            scope_kind(samples.sample(name).scope),
            Some(ScopeKind::Interpreter),
            "{name} is interpreter overhead, it must evaporate in an AOT build"
        );
    }
    assert_eq!(
        scope_kind(samples.sample("layout.taffy").scope),
        Some(ScopeKind::Native),
        "an AOT build still pays for Taffy in full; tagging layout as interpreter \
         overhead is what would make the AOT projection optimistic"
    );
}

#[test]
fn layout_taffy_nests_inside_interp_render() {
    // RFC-0030 §Q8. This is structural, not incidental call order: the
    // interpreter owns the `LayoutAtlas` and drives it from `render`. §I2b,
    // the interpreter-tax self-time correction, exists only because of it, so
    // if this ever stops holding, that correction needs revisiting rather than
    // silently becoming a no-op.
    let (mut interp, tree) = build();
    let samples = profiled_frame(&mut interp, &tree, &[]);

    let layout = samples.sample("layout.taffy");
    let render = samples.sample("interp.render");
    assert!(
        layout.depth() > render.depth(),
        "layout.taffy (depth {}) must be nested deeper than interp.render (depth {})",
        layout.depth(),
        render.depth()
    );
    assert!(
        samples.index_of("layout.taffy") < samples.index_of("interp.render"),
        "a child's guard drops first, so it must precede its parent in the block"
    );
    assert!(
        layout.start >= render.start && layout.end <= render.end,
        "layout.taffy's span must be contained by interp.render's"
    );
}

#[test]
fn the_frame_total_is_not_the_naive_sum_of_every_scope() {
    // The failure §I2 exists to prevent: a flat sum over a nested scope set
    // reports a total larger than the frame it measures.
    let (mut interp, tree) = build();
    let samples = profiled_frame(&mut interp, &tree, &[]);

    let naive: u64 = samples.0.samples.iter().map(Sample::duration_ns).sum();
    let total = samples.0.total_ns();
    assert!(
        total <= naive,
        "the depth-0 total ({total}ns) can never exceed the flat sum ({naive}ns)"
    );
    assert!(
        total >= samples.sample("interp.render").duration_ns(),
        "the total must at least cover its largest top-level scope"
    );
    // Every nanosecond is attributed to exactly one scope.
    let self_sum: u64 = (0..samples.0.samples.len())
        .map(|i| samples.0.self_ns(i))
        .sum();
    assert_eq!(
        self_sum, total,
        "self-times must partition the frame exactly"
    );
}

#[test]
fn the_interpreter_tax_does_not_bill_taffy_to_the_interpreter() {
    // RFC-0030 §I2b, measured on the real path rather than on a synthetic
    // block: `layout.taffy` is Native and nested inside an Interpreter parent,
    // so an inclusive sum would count it as a cost an AOT build avoids. It
    // does not.
    let (mut interp, tree) = build();
    let samples = profiled_frame(&mut interp, &tree, &[]);

    let inclusive = samples.0.sum_by_kind(ScopeKind::Interpreter);
    let tax = samples.0.interpreter_tax_ns();
    let layout = samples.sample("layout.taffy").duration_ns();

    assert!(
        tax <= inclusive,
        "self-time can never exceed inclusive time (tax {tax}ns, inclusive {inclusive}ns)"
    );
    assert_eq!(
        inclusive - tax,
        layout + nested_native_ns(&samples),
        "the difference between the inclusive and self readings is exactly the \
         native work nested inside interpreter scopes"
    );
    assert!(
        tax + layout <= samples.0.total_ns() + 1,
        "the tax plus layout cannot exceed the frame (+1ns for clock granularity)"
    );
}

/// Inclusive time of every `Native` sample nested at depth ≥ 1 **other than**
/// the `layout.taffy` sample the caller accounts for separately.
fn nested_native_ns(samples: &Samples) -> u64 {
    let layout_index = samples.index_of("layout.taffy");
    samples
        .0
        .samples
        .iter()
        .enumerate()
        .filter(|(i, s)| {
            *i != layout_index && s.depth() > 0 && scope_kind(s.scope) == Some(ScopeKind::Native)
        })
        .map(|(_, s)| s.duration_ns())
        .sum()
}

#[test]
fn a_second_frame_starts_from_an_empty_ring() {
    // The block is a per-tick snapshot, not an accumulator: draining at
    // publish time must leave nothing behind, or every reading after the first
    // second of a session would be a running total.
    let (mut interp, tree) = build();
    let first = profiled_frame(&mut interp, &tree, &[]);
    let second = profiled_frame(&mut interp, &tree, &[]);

    assert_eq!(
        first.0.samples.len(),
        second.0.samples.len(),
        "two identical frames must produce the same scope set, not a growing one"
    );
    assert_eq!(
        first.0.dropped, 0,
        "the ring must not overflow on one frame"
    );
    assert_eq!(second.0.dropped, 0);
}
