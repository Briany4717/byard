//! The in-window dev HUD (RFC-0030 §V3–V4).
//!
//! A `View __ByardDevHud()` written in `byld`, embedded with `include_str!`,
//! parsed once at startup into its **own** `Interpreter`, and rendered into an
//! RFC-0017 overlay layer above the app.
//!
//! # Why it is written in `byld`
//!
//! A Rust HUD would be easier to write and would prove nothing. This one is a
//! permanent, self-executing test that the framework can render a non-trivial,
//! continuously-updating, blurred, animated overlay within its own frame
//! budget — run on every developer's machine, every session they press the
//! chord. If it cannot, the project needs to know immediately, and this is the
//! cheapest possible way to find out.
//!
//! It uses no privileged syntax. `for` inside a `Canvas` did not exist when the
//! sparkline was written; the answer was to add it to the language rather than
//! special-case the HUD (RFC-0020 erratum).
//!
//! # Its own interpreter, and why no name is reserved (§Q4)
//!
//! The HUD is parsed into a separate `Interpreter` from the user's view
//! registry. A user view named `__ByardDevHud` therefore resolves to the user's
//! view in the user's tree and to the HUD in the HUD's tree, with no
//! interaction between them. There is nothing to diagnose because there is no
//! collision — which *removes* work rather than adding it, and avoids a
//! permanent tax on the language's namespace paid for a dev-only feature.
//!
//! # The observer effect, handled rather than hidden (§V4)
//!
//! The HUD draws into the frame it reports on. So:
//!
//! 1. lowering and rendering run inside `profile_scope!("hud.render")`, and
//!    the whole of it under `telemetry::attribute_to(Owner::DevTools)`;
//! 2. every figure it displays is computed net of that **owner**, not of that
//!    scope;
//! 3. the subtracted amount is displayed, so the subtraction is auditable
//!    rather than a claim.
//!
//! The distinction between (1)'s scope and (2)'s owner is the whole of the
//! self-accounting erratum, and it is not pedantry. `hud.render` bounds the
//! HUD's cost on the *logic* thread and nothing else. Two things escape it:
//!
//! - Inside it, the HUD's own `interp.render`, `interp.tick` and
//!   `layout.taffy` carry the same scope *names* as the app's. A block that
//!   aggregates by name merges them into the app's rows — honestly marked
//!   `×2`, and silently wrong about whose time it is.
//! - Outside it entirely, on the render thread, the HUD's twenty-five text
//!   leaves are shaped inside `encode.glyphs`, which the HUD's own scope
//!   cannot possibly enclose because it has already been dropped.
//!
//! Both are answered by the owner travelling with the sample. `hud.render`
//! still exists — it is what makes the logic half separable at all — but the
//! figure §V4 requires is `owner_total_ns(DevTools)` summed over both threads.
//!
//! # INV-24: it must not defeat what it measures
//!
//! The HUD's text changes every frame, so its text leaves would be
//! layout-dirty every frame, so they would be re-shaped every frame — exactly
//! the cost the retained layout path removed, reintroduced by the tool that
//! reports on it. Three mitigations:
//!
//! 1. **10 Hz updates.** The eye cannot read a 60 Hz counter, and this leaves
//!    five of six frames fully clean.
//! 2. **The sparkline is geometry, not text.** A `CanvasShape` parameter is a
//!    paint-class change and never touches layout.
//! 3. **Fixed-width numeric formatting**, done here on the host rather than by
//!    interpolation in `byld`: `" 3.4"` and `"12.7"` occupy the same columns,
//!    so a text leaf's *layout* fingerprint is stable even when its content
//!    changes and only the paint half dirties.
//!
//! Mitigation 3 only became load-bearing once the glyph cache was made
//! content-addressed. Before that the shaper re-shaped whatever the upstream
//! `dirty` flag pointed at, and the interpreter sets that flag on every leaf
//! of every frame — so the HUD's twenty-five fixed-width fields were re-shaped
//! sixty times a second no matter how stable their strings were, and
//! `encode.glyphs` roughly quadrupled the moment the HUD opened. The
//! formatting was correct and inert. It is now the thing that makes five of
//! six frames cost nothing (see `byard-core::encoder::text_glyph`).
//!
//! And one that is not a mitigation but a correctness requirement: the HUD runs
//! its own `LayoutAtlas`, on the same thread, so its layout activity lands in
//! the same thread-local `path_counters` the statusline reads. The app's
//! snapshot is captured before the HUD renders and restored onto the frame
//! afterwards — otherwise the HUD would report the app as rebuilding every
//! frame, which is the observer effect in its most embarrassing form.

use byard_compiler::Symbol;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::ast::ViewDecl;
use byard_core::frame::RenderFrame;

/// The HUD's source, embedded at build time.
const HUD_SOURCE: &str = include_str!("hud.byd");

/// The view the HUD source declares.
const HUD_VIEW: &str = "__ByardDevHud";

/// How many sparkline bars the HUD draws.
const SPARK_BARS: usize = 24;
/// Bar geometry, in the canvas's local coordinates.
const BAR_W: f32 = 6.0;
const BAR_GAP: f32 = 3.0;
const SPARK_H: f32 = 28.0;

/// How often the HUD's fields are rebuilt (INV-24 mitigation 1).
const UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// The HUD's margin from the top-right corner of the window.
const MARGIN: f32 = 12.0;
/// The HUD's own width, mirroring `hud.byd`'s root `width:`.
const HUD_WIDTH: f32 = 252.0;

/// One frame's worth of numbers for the HUD, as they cross from the render
/// thread to the logic thread.
///
/// Plain `Copy` data — no `String`, no `Arc`, nothing that allocates on either
/// side of the boundary. The formatting happens on the logic thread, once per
/// update rather than once per frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct HudTelemetry {
    /// Redraws in the last measured second.
    pub fps: u32,
    /// The frame period.
    pub frame_ns: u64,
    /// The budget the bars are drawn against.
    pub budget_ns: u64,
    /// The pipeline's critical path, with the HUD's own cost already removed.
    pub work_ns: u64,
    /// `interp.render` inclusive, minus the HUD.
    pub interp_ns: u64,
    /// `layout.taffy` self-time, minus the HUD's own layout.
    pub layout_ns: u64,
    /// `encode.frame` inclusive.
    pub encode_ns: u64,
    /// Resolved GPU pass time (async, −2f).
    pub gpu_ns: u64,
    /// Box-class draw instances in the last published frame.
    pub boxes: usize,
    /// Text lines.
    pub texts: usize,
    /// Vector glyphs.
    pub vectors: usize,
    /// Frames on the retained layout path, out of `window`.
    pub retained: u32,
    /// How many frames that window holds.
    pub window: u32,
    /// Per-frame work in microseconds, oldest first.
    pub spark: [u16; SPARK_BARS],
    /// `hud.render`, measured last frame and subtracted from everything above.
    pub hud_cost_ns: u64,
}

/// The HUD's interpreter, tree and update clock.
pub struct DevHud {
    interp: Interpreter,
    decl: ViewDecl,
    tree: Vec<RenderNode>,
    /// The most recent telemetry received from the render thread.
    latest: HudTelemetry,
    /// When the fields were last rebuilt.
    last_update: std::time::Instant,
    /// Viewport width the tree was last lowered against, so a resize re-anchors
    /// the HUD to the corner rather than leaving it mid-screen.
    lowered_width: f32,
}

impl DevHud {
    /// Parses the embedded source into its own interpreter.
    ///
    /// # Errors
    ///
    /// Returns a message if the embedded HUD fails to parse — which is a build
    /// error in this repository, not a user-facing one, and is reported rather
    /// than panicked so a broken HUD never takes a developer's session down
    /// with it.
    pub fn new() -> Result<Self, String> {
        let parsed = byard_compiler::parser::parse(HUD_SOURCE);
        if !parsed.errors.is_empty() {
            return Err(format!(
                "the embedded dev HUD does not parse: {:?}",
                parsed.errors
            ));
        }
        let decl = parsed
            .views
            .into_iter()
            .find(|v| v.name.as_str() == HUD_VIEW)
            .ok_or_else(|| format!("the embedded dev HUD declares no `{HUD_VIEW}`"))?;

        Ok(Self {
            interp: Interpreter::new(),
            decl,
            tree: Vec::new(),
            latest: HudTelemetry::default(),
            // Already due, so the first frame the HUD is shown draws real
            // numbers rather than a tenth of a second of zeroes.
            last_update: std::time::Instant::now()
                .checked_sub(UPDATE_INTERVAL)
                .unwrap_or_else(std::time::Instant::now),
            lowered_width: f32::NAN,
        })
    }

    /// Accepts a fresh reading from the render thread.
    pub fn accept(&mut self, telemetry: HudTelemetry) {
        self.latest = telemetry;
    }

    /// Whether the fields are due to be rebuilt (INV-24 mitigation 1).
    fn due(&self, width: f32) -> bool {
        // A resize re-anchors immediately: a HUD stranded in the middle of the
        // window for a tenth of a second reads as a bug in the HUD.
        self.tree.is_empty()
            || (self.lowered_width - width).abs() > 0.5
            || self.last_update.elapsed() >= UPDATE_INTERVAL
    }

    /// Renders the HUD into `frame`'s overlay layer.
    ///
    /// Everything here — the record build, the lowering, the tick and the
    /// render — is inside `hud.render` **and** attributed to
    /// [`Owner::DevTools`](byard_core::telemetry::Owner::DevTools), because all
    /// of it is cost the app would not otherwise pay. Measuring only the render
    /// half would understate the figure the HUD publishes about itself, which
    /// is the one number it must not get wrong.
    ///
    /// The attribution is entered **before** the scope, so `hud.render` is
    /// itself dev-owned, and it covers every scope the HUD's interpreter and
    /// layout atlas open underneath — none of which have heard of owners.
    pub fn render(&mut self, frame: &mut RenderFrame, width: f32, height: f32) {
        let _dev = byard_core::telemetry::attribute_to(byard_core::telemetry::Owner::DevTools);
        byard_core::profile_scope!("hud.render");

        if self.due(width) {
            self.last_update = std::time::Instant::now();
            self.lowered_width = width;
            let record = self.build_record(width);
            self.interp.inject_provider("DevTelemetry", record);
            self.tree = self.interp.lower_view(&self.decl, &[HUD_VIEW]);
        }
        self.interp.tick();

        // Where the app's primitives end. Everything the HUD writes from here
        // on is marked dirty, unconditionally — see below.
        let base = frame.cursor();

        // RFC-0017: the HUD composites after the app and cannot be occluded by
        // it. This is an ordinary layer — nothing about the HUD is
        // special-cased in the renderer.
        frame.begin_layer();
        self.interp.render(&self.tree, frame, width, height);

        // # Why this is unconditional
        //
        // The first cut marked only on frames where the app's cursor had moved
        // or the HUD had re-lowered, on the reasoning that nothing else could
        // change what sits at a given index. That reasoning was wrong in
        // practice and, more importantly, **unprovable from here**: the HUD's
        // text is re-evaluated against the injected record on every render, and
        // the two producers' indices interleave in ways neither can see. The
        // failure it allowed was a debug assertion on a frame two seconds in —
        // and in release, silently stale text on screen.
        //
        // An overlay that cannot prove its indices are stable does not get to
        // assume they are. The cost is bounded and known: ~25 fixed-width text
        // leaves re-shaped per frame the HUD is open, measured and reported in
        // the HUD's own `hud cost` field, where a developer can see what their
        // diagnostic is charging them.
        frame.mark_dirty_since(base);
    }

    /// Builds the `DevTelemetry` record the HUD injects.
    ///
    /// Every displayed number is formatted to a fixed width here rather than
    /// interpolated in `byld` (INV-24 mitigation 3): a `"{t.work}ms"` would
    /// change string length as the number crossed 10, and the HUD would
    /// re-measure itself at exactly the moment something interesting was
    /// happening.
    fn build_record(&self, width: f32) -> Value {
        let t = &self.latest;
        let over = t.work_ns > t.budget_ns;
        let field = |name: &str, v: Value| (Symbol::intern(name), v);

        Value::Record(vec![
            field("ox", Value::Float(f64::from(width - HUD_WIDTH - MARGIN))),
            field("fps", Value::Str(fixed_u32(t.fps))),
            field("frame", Value::Str(fixed_ms(t.frame_ns))),
            field("budget", Value::Str(fixed_ms(t.budget_ns))),
            field("interp", Value::Str(fixed_ms(t.interp_ns))),
            field("layout", Value::Str(fixed_ms(t.layout_ns))),
            field("encode", Value::Str(fixed_ms(t.encode_ns))),
            field("gpu", Value::Str(fixed_ms(t.gpu_ns))),
            field("boxes", Value::Str(fixed_usize(t.boxes))),
            field("texts", Value::Str(fixed_usize(t.texts))),
            field("vectors", Value::Str(fixed_usize(t.vectors))),
            field(
                "retained",
                Value::Str(format!("retained {}/{}", t.retained, t.window)),
            ),
            field(
                "retainedOk",
                Value::Bool(t.window == 0 || t.retained == t.window),
            ),
            field("over", Value::Bool(over)),
            field("hudCost", Value::Str(fixed_ms(t.hud_cost_ns))),
            // §V4's own acceptance condition, rendered rather than asserted in
            // a comment: above 5 % of the budget the HUD has failed its test,
            // and it says so on itself rather than in a document nobody reads
            // while the frame is dropping.
            field(
                "hudOk",
                Value::Bool(t.hud_cost_ns * 20 <= t.budget_ns.max(1)),
            ),
            field("spark", spark_bars(&t.spark, t.budget_ns)),
        ])
    }
}

/// The sparkline as records the `Canvas`'s `for` iterates.
///
/// Geometry rather than text (INV-24 mitigation 2): a shape parameter is a
/// paint-class change, so a moving sparkline never touches layout.
///
/// Scaled against the **budget**, and plotting **work** rather than the frame
/// period — under vsync the period *is* the budget, so a period sparkline draws
/// a permanently full bar on a perfectly healthy app.
fn spark_bars(spark: &[u16; SPARK_BARS], budget_ns: u64) -> Value {
    #[allow(clippy::cast_precision_loss)]
    let ceiling_us = (budget_ns / 1_000).max(1) as f32;
    let bars = spark
        .iter()
        .enumerate()
        .map(|(i, &us)| {
            let frac = (f32::from(us) / ceiling_us).clamp(0.0, 1.0);
            // At least one pixel: a bar of height zero and a frame that was
            // never measured look identical, and only one of them is news.
            let h = (frac * SPARK_H).max(1.0);
            #[allow(clippy::cast_precision_loss)]
            let x = i as f32 * (BAR_W + BAR_GAP);
            Value::Record(vec![
                (Symbol::intern("x"), Value::Float(f64::from(x))),
                (Symbol::intern("y"), Value::Float(f64::from(SPARK_H - h))),
                (Symbol::intern("h"), Value::Float(f64::from(h))),
                (Symbol::intern("over"), Value::Bool(frac >= 1.0)),
            ])
        })
        .collect();
    Value::List(bars)
}

/// `"  3.4"` — one decimal, right-aligned in five columns.
///
/// Fixed width is the whole point: the string's *length* must not depend on its
/// value, or every crossing of a power of ten re-measures the text.
#[allow(clippy::cast_precision_loss)]
fn fixed_ms(ns: u64) -> String {
    format!("{:>5.1}", ns as f64 / 1_000_000.0)
}

fn fixed_u32(n: u32) -> String {
    format!("{n:>3}")
}

fn fixed_usize(n: usize) -> String {
    format!("{n:>4}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_hud_parses_and_lowers_in_its_own_interpreter() {
        // The HUD ships inside the binary, so "does it compile" is a question
        // about this repository rather than about a user's project — which is
        // exactly why it needs a test rather than a runtime discovery.
        let mut hud = DevHud::new().expect("the embedded HUD must parse");
        let mut frame = RenderFrame::new();
        hud.accept(HudTelemetry {
            budget_ns: 16_667_000,
            ..HudTelemetry::default()
        });
        hud.render(&mut frame, 1280.0, 720.0);
        assert!(
            hud.interp.errors().is_empty(),
            "the HUD must lower clean: {:?}",
            hud.interp.errors()
        );
        assert!(
            !frame.texts().is_empty(),
            "the HUD must actually draw something"
        );
        assert!(
            !frame.canvas_shapes().is_empty(),
            "the sparkline must be geometry, not text — a paint-class change \
             never touches layout (INV-24)"
        );
    }

    #[test]
    fn the_hud_opens_its_own_overlay_layer() {
        // RFC-0017: it composites after the app and cannot be occluded by it.
        let mut hud = DevHud::new().unwrap();
        let mut frame = RenderFrame::new();
        frame.push_instance(byard_core::frame::BoxInstance {
            rect: [0.0, 0.0, 10.0, 10.0],
            color: [1.0; 4],
            radii: [0.0; 4],
            transform: byard_core::frame::Transform::IDENTITY,
        });
        hud.render(&mut frame, 1280.0, 720.0);
        assert!(
            !frame.layer_marks().is_empty(),
            "the HUD must open a layer, not draw into the app's"
        );
        assert_eq!(
            frame.layer_marks()[0].solid,
            1,
            "the layer must open *after* the app's geometry"
        );
    }

    #[test]
    fn a_user_view_named_like_the_hud_cannot_collide_with_it() {
        // §Q4: the collision is unexpressible rather than diagnosed. The HUD
        // has its own registry, so a user view of the same name resolves to
        // the user's in the user's tree and to the HUD's in the HUD's.
        let user =
            byard_compiler::parser::parse("View __ByardDevHud() { Text(\"the user's own view\") }");
        assert!(user.errors.is_empty(), "{:?}", user.errors);
        let mut user_interp = Interpreter::new();
        user_interp.load_views(&user.views);
        let user_tree = user_interp.lower_view(&user.views[0], &[HUD_VIEW]);
        let mut user_frame = RenderFrame::new();
        user_interp.tick();
        user_interp.render(&user_tree, &mut user_frame, 400.0, 300.0);
        assert_eq!(user_frame.texts().len(), 1);
        assert_eq!(user_frame.texts()[0].text, "the user's own view");

        // And the HUD, in its own interpreter, is entirely unaffected.
        let mut hud = DevHud::new().unwrap();
        let mut hud_frame = RenderFrame::new();
        hud.render(&mut hud_frame, 1280.0, 720.0);
        assert!(hud_frame.texts().len() > 1, "the HUD is still the HUD");
    }

    #[test]
    fn hud_render_is_a_real_scope_and_its_value_is_what_gets_subtracted() {
        // §V4. A HUD that hides its own overhead is worse than no HUD, so the
        // scope has to exist, be entered, and be the thing the displayed
        // figures are net of.
        let _ = byard_core::telemetry::drain_samples();
        let mut hud = DevHud::new().unwrap();
        let mut frame = RenderFrame::new();
        hud.render(&mut frame, 1280.0, 720.0);
        let block = byard_core::telemetry::drain_samples();
        let entered = block.samples.iter().any(|s| {
            byard_core::telemetry::scope_name(s.scope) == Some("hud.render") && s.duration_ns() > 0
        });
        assert!(
            entered,
            "`hud.render` must be entered and non-zero, or the subtraction is \
             a claim rather than a measurement"
        );
    }

    #[test]
    fn every_scope_the_hud_causes_is_stamped_as_the_dev_runners() {
        // The correction the self-accounting erratum exists for. The HUD's
        // cost is not a call it makes — it is the interpreter, the layout
        // atlas and the shaper doing their ordinary jobs on its behalf, under
        // scope names the app also uses. Nothing below `render` opts in, so
        // this asserts that the boundary attribution really does reach all of
        // it: one un-stamped sample here is one that would be added to the
        // app's row.
        use byard_core::telemetry::{Owner, scope_name};
        let _ = byard_core::telemetry::drain_samples();
        let mut hud = DevHud::new().unwrap();
        let mut frame = RenderFrame::new();
        hud.render(&mut frame, 1280.0, 720.0);
        let block = byard_core::telemetry::drain_samples();

        assert!(block.samples.len() > 1, "the HUD opens more than one scope");
        let strays: Vec<_> = block
            .samples
            .iter()
            .filter(|s| s.owner() != Owner::DevTools)
            .filter_map(|s| scope_name(s.scope))
            .collect();
        assert!(
            strays.is_empty(),
            "these ran for the HUD and would be billed to the app: {strays:?}"
        );
        assert_eq!(block.owner_total_ns(Owner::App), 0);
        assert_eq!(
            block.owner_total_ns(Owner::DevTools),
            block.total_ns(),
            "the whole of this block is the HUD's"
        );
    }

    #[test]
    fn the_attribution_ends_with_the_render_it_covers() {
        // A thread-local that leaked would silently re-label the *app's* next
        // frame as the profiler's, which is the same defect in the opposite
        // direction and much harder to notice.
        use byard_core::telemetry::{Owner, current_owner};
        assert_eq!(current_owner(), Owner::App);
        let mut hud = DevHud::new().unwrap();
        hud.render(&mut RenderFrame::new(), 1280.0, 720.0);
        assert_eq!(current_owner(), Owner::App);
    }

    #[test]
    fn the_hud_declares_where_its_primitives_start_so_the_encoder_can_charge_it() {
        // The logic thread cannot enclose the render thread's shaping — its
        // scope is long gone by then — so the *frame* carries the partition.
        // Without it the HUD's twenty-five lines are shaped on the app's
        // ticket and §V4 under-reports by the largest term in the frame.
        let mut frame = RenderFrame::new();
        frame.push_text(byard_core::frame::TextLine {
            x: 0.0,
            y: 0.0,
            text: "the app".to_string(),
            font_size: 14.0,
            color: [1.0; 4],
            dirty: true,
        });
        let base = frame.cursor();
        let mut hud = DevHud::new().unwrap();
        hud.render(&mut frame, 1280.0, 720.0);
        frame.set_dev_base(base);

        assert_eq!(frame.dev_text_start(), 1);
        assert!(
            frame.texts().len() > 10,
            "the HUD really does emit the text this is partitioning"
        );
    }

    #[test]
    fn every_displayed_number_has_a_value_independent_width() {
        // INV-24 mitigation 3. If a field's *length* changes with its value,
        // the text re-measures the moment a number crosses a power of ten —
        // which is exactly when something interesting is happening.
        assert_eq!(fixed_ms(3_400_000).len(), fixed_ms(123_400_000).len());
        assert_eq!(fixed_ms(0).len(), fixed_ms(999_900_000).len());
        assert_eq!(fixed_u32(7).len(), fixed_u32(144).len());
        assert_eq!(fixed_usize(0).len(), fixed_usize(9999).len());
        // And they are right-aligned, so the digits line up rather than the
        // padding.
        assert!(fixed_ms(3_400_000).starts_with(' '));
        assert!(fixed_ms(3_400_000).ends_with("3.4"));
    }

    #[test]
    fn the_sparkline_is_scaled_against_the_budget_and_never_zero_height() {
        let budget = 16_667_000u64;
        let mut spark = [0u16; SPARK_BARS];
        spark[0] = 1_000; // 1ms — well inside budget
        spark[1] = 16_667; // exactly at budget
        spark[2] = 40_000; // way over
        let Value::List(bars) = spark_bars(&spark, budget) else {
            panic!("the sparkline is a list of records");
        };
        assert_eq!(bars.len(), SPARK_BARS);

        let float = |i: usize, key: &str| match &bars[i] {
            Value::Record(f) => match f.iter().find(|(k, _)| k.as_str() == key) {
                Some((_, Value::Float(v))) => *v,
                other => panic!("expected a float {key}, got {other:?}"),
            },
            _ => panic!("record"),
        };
        let height = |i: usize| float(i, "h");
        let over = |i: usize| match &bars[i] {
            Value::Record(f) => f
                .iter()
                .find(|(k, _)| k.as_str() == "over")
                .and_then(|(_, v)| v.as_bool())
                .unwrap(),
            _ => panic!("record"),
        };

        assert!(
            height(0) < f64::from(SPARK_H) * 0.2,
            "1ms of 16.7ms is short"
        );
        assert!(!over(0));
        assert!(over(1), "a frame at the budget is over it");
        assert!(over(2));
        assert!(
            (height(2) - f64::from(SPARK_H)).abs() < 0.01,
            "an over-budget bar clamps to full height rather than overflowing"
        );
        // A never-measured frame and a zero-cost frame must not render as
        // nothing at all — only one of them is news.
        assert!(height(3) >= 1.0);
    }

    #[test]
    fn the_hud_does_not_claim_the_apps_layout_path_as_its_own() {
        // INV-24's second acceptance condition, at the unit level. The HUD runs
        // its own `LayoutAtlas` on the same thread, so its layout activity
        // lands in the same thread-local counters the statusline reads — and
        // `Interpreter::render` writes `set_atlas_paths` at the end of *its*
        // render. Without the caller restoring the app's snapshot afterwards,
        // the HUD reports the developer's app as rebuilding every frame, which
        // looks exactly like an app bug.
        let mut frame = RenderFrame::new();
        let app_paths = byard_core::atlas::layout::path_counters::Counts {
            clears: 0,
            populate_calls: 1,
            retained_recomputes: 1,
            ..Default::default()
        };
        frame.set_atlas_paths(app_paths);

        let mut hud = DevHud::new().unwrap();
        hud.render(&mut frame, 1280.0, 720.0);
        assert_ne!(
            frame.atlas_paths(),
            app_paths,
            "this test is worthless unless the HUD really does overwrite it —              if this ever fails, the restore in `dev.rs` has become unnecessary              and should be deleted rather than kept as a charm"
        );

        // Which is why the caller restores it, and why that restore is the
        // thing under test.
        frame.set_atlas_paths(app_paths);
        assert_eq!(frame.atlas_paths(), app_paths);
    }

    #[test]
    fn the_hud_marks_its_own_primitives_dirty_on_every_frame() {
        // The encoder's glyph cache is index-addressed: it compares `texts[i]`
        // against what it shaped for `texts[i]` last frame. The HUD is a second
        // interpreter emitting after the app into the same pools, so index `i`
        // can hold the HUD's line where it held the app's. Both producers
        // truthfully report their own primitives unchanged; the cache would
        // draw last frame's shaped buffer, in release, silently.
        //
        // The first cut marked only when the app's cursor had moved or the HUD
        // had re-lowered. That was unprovable from here and wrong in practice —
        // a real session tripped the debug assertion two seconds in. An overlay
        // that cannot prove its indices are stable does not get to assume they
        // are, so this is unconditional and the test says so.
        let mut hud = DevHud::new().unwrap();

        // A frame where nothing beneath the HUD moved at all: same app content,
        // no re-lower due. The old conditional passed this and should not have.
        let mut first = RenderFrame::new();
        hud.render(&mut first, 1280.0, 720.0);
        let base_first = 0;
        assert!(
            first.texts()[base_first..].iter().all(|t| t.dirty),
            "the HUD must mark its own text even when nothing beneath it moved"
        );

        let mut second = RenderFrame::new();
        second.push_text(byard_core::frame::TextLine {
            x: 0.0,
            y: 0.0,
            text: "the app".to_string(),
            font_size: 14.0,
            color: [1.0; 4],
            dirty: false,
        });
        let base = second.text_count();
        hud.render(&mut second, 1280.0, 720.0);
        assert!(
            second.texts().len() > base,
            "the HUD must have emitted text at all"
        );
        assert!(
            second.texts()[base..].iter().all(|t| t.dirty),
            "every HUD line must be dirty, or the glyph cache draws the \
             previous occupant's shaped buffer at that index"
        );
        // That `mark_dirty_since` leaves everything *before* the mark alone is
        // asserted where the mechanism lives, in `byard-core`'s frame tests —
        // here the question is only whether the HUD invokes it.
    }

    #[test]
    fn the_hud_reports_itself_as_failing_its_own_budget_test() {
        // §V4's acceptance condition is 5 % of the budget. Rendering that
        // verdict on the HUD, rather than leaving it in a document, is what
        // makes it visible at the moment it matters.
        let hud = |cost_ns| {
            let mut h = DevHud::new().unwrap();
            h.accept(HudTelemetry {
                budget_ns: 16_667_000,
                hud_cost_ns: cost_ns,
                ..HudTelemetry::default()
            });
            let Value::Record(fields) = h.build_record(1280.0) else {
                panic!("record");
            };
            fields
                .iter()
                .find(|(k, _)| k.as_str() == "hudOk")
                .and_then(|(_, v)| v.as_bool())
                .unwrap()
        };
        assert!(hud(400_000), "0.4ms of a 16.7ms budget is 2.4% — fine");
        assert!(!hud(1_500_000), "1.5ms is 9% — the HUD has failed its test");
    }
}

/// RFC-0030 §V4's acceptance condition, measured end to end.
///
/// > with the HUD open, the reported HUD cost is within ~10 % of
/// > `(frame total with HUD) − (frame total without HUD)` on the same scene
/// > and build.
///
/// Every other test in this file checks a mechanism. This one checks the
/// *claim*, against a real interpreter, a real layout atlas and a real GPU
/// encoder, by doing the only thing that can check it: rendering the same
/// scene with the HUD and without it and looking at the difference.
///
/// # It is a paired measurement, and that is not a detail
///
/// The two configurations are rendered **alternately**, one frame each, on two
/// rigs that both exist for the whole run — never as one batch followed by
/// another. A batched version of this test passed here and failed on CI,
/// because everything that varies with *time* rather than with configuration
/// (a cold driver, a cold font cache, thermal state, a noisy runner) lands
/// entirely in the difference between two batches. It inflated the first batch
/// until the two totals nearly matched and the "measured delta" came out at a
/// fifth of the cost the profiler was correctly reporting.
///
/// A measurement whose baseline is taken under different conditions from the
/// thing it is a baseline for is the mistake this whole erratum is about. It
/// is worth noticing that it was made twice more while fixing it — once with a
/// static-scene baseline that skipped the draw path, once with batching — and
/// that both times the *reported* number was right and the comparison was
/// wrong.
///
/// # Why the old rule is measured alongside the new one
///
/// The erratum's acceptance also asks for the failure to be demonstrable. So
/// each measurement reports **two** figures — the `hud.render` scope's
/// inclusive time, which is what §V4 used to publish, and the dev-owner total,
/// which is what it publishes now — and the test asserts that the first misses
/// the delta and the second does not. The red case is therefore permanent and
/// runs on every commit, rather than being a procedure in a document that
/// someone has to remember to follow.
#[cfg(test)]
mod self_accounting {
    use super::*;
    use byard_core::telemetry::{Owner, drain_samples};
    use std::sync::Arc;

    /// Paired frames measured. Enough that a median is not one scheduling
    /// hiccup, few enough that the test stays quick.
    const SAMPLES: usize = 21;
    /// Frames rendered on both rigs before either is measured, so neither
    /// configuration is the one paying for pipeline compilation, glyph-atlas
    /// residency and every line's first shape.
    const WARMUP: usize = 8;
    /// RFC-0030 §V4's own acceptance condition, and the budget it is against.
    ///
    /// Checked against the **measured** delta, never against what the profiler
    /// reports about itself — a self-accounting gate read off its own output
    /// is not a gate.
    const GATE_FRACTION: f64 = 0.05;
    /// 60 Hz, the budget every figure in this test is quoted against.
    const BUDGET_NS: f64 = 16_667_000.0;

    /// How far `dev-owner total + the measured `encode.finish` delta` may sit
    /// from the measured frame delta — in **both** profiles.
    ///
    /// This is the real invariant, and it is tighter than a wide band on the
    /// direct comparison rather than looser: it says every nanosecond the HUD
    /// adds is either *attributed to the dev runner* or *inside the one scope
    /// this PR documented as unsplittable* — with that scope's contribution
    /// measured on the same frames rather than assumed. Attribution silently
    /// degrading fails it; `wgpu`'s validator being slow in debug does not,
    /// because the term it inflates is named on both sides of the equation.
    ///
    /// The remaining slack covers what genuinely does not decompose: a few
    /// microseconds of `encode.frame` self-time, and `encode.scissor` /
    /// `encode.bookkeeping` scanning ~80 more primitives when the HUD is open.
    const ACCOUNTED_TOLERANCE: f64 = 0.25;

    struct Gpu {
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        enc: byard_core::encoder::EncoderSubsystem,
        target: wgpu::Texture,
    }

    impl Gpu {
        fn new() -> Option<Self> {
            let instance = wgpu::Instance::new(
                wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
            );
            let adapter = pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
            )
            .ok()?;
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("byard dev - HUD self-accounting test device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: adapter.limits(),
                    memory_hints: wgpu::MemoryHints::Performance,
                    ..Default::default()
                }))
                .ok()?;
            let (device, queue) = (Arc::new(device), Arc::new(queue));
            let mut enc = pollster::block_on(byard_core::encoder::EncoderSubsystem::init(
                Arc::clone(&device),
                Arc::clone(&queue),
                wgpu::TextureFormat::Rgba8UnormSrgb,
                1.0,
                1280,
                720,
            ))
            .ok()?;
            enc.update_viewport(
                byard_core::frame::Viewport::new(1280.0, 720.0),
                1280,
                720,
                1.0,
            );
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("hud self-accounting target"),
                size: wgpu::Extent3d {
                    width: 1280,
                    height: 720,
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
            });
            Some(Self {
                device,
                queue,
                enc,
                target,
            })
        }

        fn encode(&mut self, frame: &RenderFrame) -> byard_core::telemetry::SampleBlock {
            let cmd = self
                .enc
                .encode_frame_from_relay(&self.target, frame)
                .expect("encode");
            self.queue.submit(std::iter::once(cmd));
            self.device.poll(wgpu::PollType::wait_indefinitely()).ok();
            drain_samples()
        }
    }

    /// A small but non-trivial app: a column of labelled rows, so the
    /// interpreter, the layout atlas and the shaper all do real work.
    const APP_SOURCE: &str = r#"
View Main() {
    Column #[bg: 0x101014, p: 24, gap: 10, width: 720] {
        Text("RFC-0030 self-accounting") #[color: 0xFFFFFF, size: 22]
        Row #[gap: 8] {
            Text("alpha") #[color: 0x9AA0AC, size: 14]
            Text("beta") #[color: 0x9AA0AC, size: 14]
            Text("gamma") #[color: 0x9AA0AC, size: 14]
        }
        Row #[gap: 8] {
            Text("delta") #[color: 0x9AA0AC, size: 14]
            Text("epsilon") #[color: 0x9AA0AC, size: 14]
            Text("zeta") #[color: 0x9AA0AC, size: 14]
        }
    }
}
"#;

    /// One frame's readings.
    struct Reading {
        /// Both rings' depth-0 totals — the whole cost of producing this frame.
        total: u64,
        /// What §V4 reports now: every sample stamped `DevTools`, on both
        /// rings.
        owner: u64,
        /// What §V4 used to report: the inclusive time of the one scope named
        /// `hud.render`.
        scope: u64,
        /// `encode.finish` — `wgpu` assembling the command buffer. The one
        /// term the attribution cannot split, measured so the accounting
        /// identity can name it instead of absorbing it into slack.
        finish: u64,
    }

    /// Everything one configuration needs to render frames indefinitely: its
    /// own GPU device and encoder, its own interpreter and tree, and — for the
    /// HUD-open configuration — its own `DevHud`.
    ///
    /// Two of these exist at once and are driven **alternately**; see the test
    /// below for why that matters more than anything else here.
    struct Rig {
        gpu: Gpu,
        app: Interpreter,
        tree: Vec<RenderNode>,
        hud: Option<DevHud>,
        frame: RenderFrame,
        tick: u32,
    }

    impl Rig {
        fn new(with_hud: bool) -> Option<Self> {
            let gpu = Gpu::new()?;
            let parsed = byard_compiler::parser::parse(APP_SOURCE);
            assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
            let mut app = Interpreter::new();
            app.load_views(&parsed.views);
            let tree = app.lower_view(&parsed.views[0], &["Main"]);
            Some(Self {
                gpu,
                app,
                tree,
                hud: with_hud.then(|| DevHud::new().expect("the embedded HUD parses")),
                frame: RenderFrame::new(),
                tick: 0,
            })
        }

        /// Renders one frame and drains both rings.
        ///
        /// The two "threads" are the same thread here, which is what makes the
        /// totals addable: they ran in sequence, so their sum really is the
        /// frame's wall-clock cost rather than a double count of two concurrent
        /// lanes.
        fn one_frame(&mut self) -> Reading {
            self.tick += 1;
            self.frame.clear();
            let _ = drain_samples();
            self.app.tick();
            self.app.render(&self.tree, &mut self.frame, 1280.0, 720.0);
            // One line of the app's own that changes every frame.
            //
            // Without it the baseline measures a frame the encoder *skips*: a
            // static scene has no dirty region, so `should_draw` is false and
            // the whole draw path — glyphs, staging, passes, the swapchain copy
            // — is never entered. Comparing that against a frame the HUD forced
            // into a full redraw would attribute the app's own drawing to the
            // HUD, which is the same misattribution as the one under repair,
            // inverted.
            //
            // (The effect is real and worth knowing — most of what a HUD costs
            // a *static* app is that it defeats the skip — but it is a property
            // of the skip, not of the HUD's own work, and §V4's figure is the
            // latter. It is recorded in `support/PERF_hud_baseline.md`.)
            let tick = self.tick;
            self.frame.push_text(byard_core::frame::TextLine {
                x: 24.0,
                y: 320.0,
                text: format!("frame {tick:>6}"),
                font_size: 14.0,
                color: [0.6, 0.6, 0.65, 1.0],
                dirty: true,
            });
            let base = self.frame.cursor();
            if let Some(hud) = self.hud.as_mut() {
                hud.render(&mut self.frame, 1280.0, 720.0);
            }
            self.frame.set_dev_base(base);
            let logic = drain_samples();
            let render = self.gpu.encode(&self.frame);

            let sum_scope = |name: &str| -> u64 {
                [&logic, &render]
                    .iter()
                    .flat_map(|b| b.samples.iter())
                    .filter(|s| byard_core::telemetry::scope_name(s.scope) == Some(name))
                    .map(byard_core::telemetry::Sample::duration_ns)
                    .sum()
            };
            Reading {
                total: logic.total_ns() + render.total_ns(),
                owner: logic.owner_total_ns(Owner::DevTools)
                    + render.owner_total_ns(Owner::DevTools),
                scope: sum_scope("hud.render"),
                finish: sum_scope("encode.finish"),
            }
        }
    }

    fn median<T: Ord + Copy>(mut v: Vec<T>) -> T {
        v.sort_unstable();
        v[v.len() / 2]
    }

    #[test]
    fn the_reported_hud_cost_is_the_cost_the_hud_actually_adds() {
        let (Some(mut closed), Some(mut open)) = (Rig::new(false), Rig::new(true)) else {
            eprintln!("no GPU adapter available — skipping");
            return;
        };

        // Warm-up, on **both** rigs, before either is measured: the first
        // frames pay pipeline compilation, glyph-atlas residency and every
        // line's first shape.
        for _ in 0..WARMUP {
            closed.one_frame();
            open.one_frame();
        }

        // Paired, interleaved, and this is the part that has to stay that way.
        //
        // The first version of this test measured one whole batch without the
        // HUD and then one whole batch with it. Every cost that is *sequential*
        // rather than configurational — a cold driver, a cold font cache, the
        // machine's thermal state, another job on the runner — then landed
        // entirely in the difference between the batches. On this developer's
        // machine that bias was small enough to hide; on a CI runner it
        // inflated the first batch until the two totals nearly matched, and the
        // measured "delta" collapsed to a fifth of the cost the profiler was
        // correctly reporting. The number under test was right and the baseline
        // was wrong, which is the same mistake this whole PR is about, made
        // one more time by its own test.
        //
        // Alternating one frame each shares that drift between the two
        // configurations instead of charging it to one, and taking the median
        // of the per-iteration *differences* — rather than the difference of
        // two medians — keeps a single noisy frame from moving the answer.
        // Each rig keeps its own encoder, so alternating never disturbs either
        // one's text cache or its steady-state pool length.
        let mut deltas = Vec::with_capacity(SAMPLES);
        let mut finishes = Vec::with_capacity(SAMPLES);
        let mut owners = Vec::with_capacity(SAMPLES);
        let mut scopes = Vec::with_capacity(SAMPLES);
        let ns = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
        for _ in 0..SAMPLES {
            let a = closed.one_frame();
            let b = open.one_frame();
            assert_eq!(a.owner, 0, "a closed HUD costs nothing, and says so");
            assert_eq!(a.scope, 0);
            deltas.push(ns(b.total) - ns(a.total));
            // The unsplittable term's own contribution, measured on the same
            // pair of frames rather than assumed from a previous run.
            finishes.push(ns(b.finish) - ns(a.finish));
            owners.push(b.owner);
            scopes.push(b.scope);
        }

        let (delta_ns, finish_ns) = (median(deltas), median(finishes));
        let (owner, scope) = (median(owners), median(scopes));
        assert!(
            delta_ns > 0,
            "the HUD must cost *something*, or this measurement is meaningless \
             (median delta {delta_ns}ns)"
        );
        #[allow(clippy::cast_precision_loss)]
        let delta = delta_ns as f64;
        #[allow(clippy::cast_precision_loss)]
        let accounted = (ns(owner) + finish_ns.max(0)) as f64;
        #[allow(clippy::cast_precision_loss)]
        let owner_err = ((owner as f64) - delta).abs() / delta;
        #[allow(clippy::cast_precision_loss)]
        let scope_err = ((scope as f64) - delta).abs() / delta;
        let accounted_err = (accounted - delta).abs() / delta;

        // The reading, printed before the assertions so a CI failure carries
        // its own diagnosis rather than one number and no context.
        eprintln!(
            "hud cost: delta {delta:.0}ns · owner {owner}ns ({:.0}% off) · \
             hud.render alone {scope}ns ({:.0}% off) · \
             owner + encode.finish {accounted:.0}ns ({:.0}% off)",
            owner_err * 100.0,
            scope_err * 100.0,
            accounted_err * 100.0,
        );

        // Green, in both profiles: every nanosecond the HUD adds is either
        // attributed to the dev runner or inside the one scope this cannot
        // split — and that scope's share is measured here, not assumed.
        assert!(
            accounted_err <= ACCOUNTED_TOLERANCE,
            "the HUD's cost must be fully accounted for: {owner}ns attributed \
             plus {finish_ns}ns of `encode.finish` against a measured delta of \
             {delta}ns ({:.0}% off, tolerance {:.0}%)",
            accounted_err * 100.0,
            ACCOUNTED_TOLERANCE * 100.0
        );

        // §V4's own condition — the HUD inside 5 % of the frame budget —
        // against the **measured** delta rather than against what the profiler
        // reports about itself, which is the only way a self-accounting gate
        // can be checked without begging the question.
        //
        // Release only, and not because debug is inconvenient: a 5 % *budget*
        // claim is a claim about a shipped build. Asserting it against a debug
        // frame would be asserting something nobody has ever cared about. The
        // accounting identity above — the correctness claim — runs in both.
        if !cfg!(debug_assertions) {
            let share = delta / BUDGET_NS;
            assert!(
                share <= GATE_FRACTION,
                "RFC-0030 §V4: the HUD must fit in {:.0}% of the frame budget \
                 — it costs {delta:.0}ns of {BUDGET_NS:.0}ns ({:.2}%)",
                GATE_FRACTION * 100.0,
                share * 100.0
            );
        }

        // Red: the rule this erratum replaced does not, and the failure is
        // permanent rather than a procedure someone has to re-run.
        assert!(
            scope > 0 && scope < owner,
            "`hud.render` alone cannot enclose the render thread's shaping — \
             if it now does, the attribution moved and this test is guarding \
             nothing (scope {scope}ns, owner {owner}ns)"
        );
    }
}
