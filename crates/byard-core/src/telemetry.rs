//! Zero-allocation telemetry & profiling (RFC-0013).
//!
//! Each engine thread owns a fixed-capacity, thread-local ring of
//! [`Sample`]s. [`profile_scope!`] wraps a block in an RAII [`Guard`] that
//! writes one sample on drop; the ring never grows and never allocates on
//! the hot path (P1). At end-of-tick, [`crate::relay::Relay::publish`] calls
//! [`crate::frame::RenderFrame::drain_telemetry`], which pulls the calling
//! thread's ring into the frame's own [`SampleBlock`], piggybacking on the
//! existing atomic frame swap instead of opening a new channel.
//!
//! With the `telemetry` Cargo feature off, [`profile_scope!`] expands to a
//! no-op statement, zero cost in a build that disables it (e.g. release).

use std::cell::{Cell, RefCell};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Fixed capacity of each thread's sample ring (RFC-0013 **P1**).
pub const RING_CAPACITY: usize = 4096;

/// How many direct children of one scope
/// [`SampleBlock::for_each_direct_child`] will report.
///
/// Bounded so the traversal stays allocation-free on what is a display path.
/// The engine's whole scope set is six (RFC-0030 §I1); a scope with more than
/// 32 *direct* children is a loop that should have been one scope, not a
/// reading anybody is going to sit and read.
pub const MAX_DIRECT_CHILDREN: usize = 32;

/// A compile-time-interned scope identifier.
///
/// Looked up once per call site (cached in a call-site-local `OnceLock` by
/// [`profile_scope!`]), never per sample.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ScopeId(pub u16);

/// Which **logical producer** a sample's time belongs to (RFC-0030 erratum
/// "self-accounting and owner attribution").
///
/// A scope name says *what kind of work* ran. It does not say *whose*. When two
/// producers run the same scope in one frame, the app's interpreter and the
/// dev HUD's, both entering `interp.render`, both entering `layout.taffy`, a
/// consumer that aggregates by name alone merges them and bills all of it to
/// the app. The merge is not hidden (the block prints `×2`), but the
/// attribution is silently wrong, which is worse: the reader is told two
/// samples were folded and not told that one of them was the profiler's own
/// overlay.
///
/// So the owner travels **with the sample**, stamped at entry from a
/// thread-local set by [`attribute_to`], and every consumer that attributes
/// cost filters on it. This is the same class of correction as RFC-0030 §I2b's
/// self-time, there, a parent and child were summed and exceeded the frame;
/// here, two peers are summed and attributed to one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Owner {
    /// The application under development, and the engine work it causes. The
    /// default: nothing has to opt in to being the app.
    #[default]
    App = 0,
    /// The dev runner's own diagnostic surfaces, the in-window HUD and
    /// everything the engine does *because* the HUD is open, including its
    /// interpreter, its layout and the shaping of its text on the render
    /// thread.
    ///
    /// Every figure `byard dev` shows about the app is net of this bucket, and
    /// the bucket itself is displayed rather than merely subtracted (RFC-0030
    /// §V4).
    DevTools = 1,
}

impl Owner {
    /// The raw byte stored in a [`Sample`].
    const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The owner a stored byte denotes. An unrecognised byte reads as
    /// [`Owner::App`]: a `Sample` is `Pod`, so every bit pattern has to mean
    /// something, and "the app" is the reading that cannot invent dev-tool
    /// overhead out of a corrupt byte.
    const fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::DevTools,
            _ => Self::App,
        }
    }
}

/// Which cost bucket a scope belongs to (RFC-0013 §"The interpreter tax
/// segmentation"): `Interpreter` scopes evaporate in an AOT release build,
/// `Native` scopes don't, and `Gpu` scopes are async pass timings rather than
/// CPU wall-clock at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopeKind {
    /// Tree-walking eval, dynamic dispatch, env lookups, the cost an AOT
    /// (transpiled) build does not pay.
    Interpreter,
    /// Ordinary CPU work that costs the same in dev and in an AOT release.
    #[default]
    Native,
    /// A `wgpu` render-pass timing, resolved asynchronously (RFC-0013
    /// "GPU timing").
    Gpu,
}

struct ScopeEntry {
    name: &'static str,
    kind: ScopeKind,
}

fn registry() -> &'static Mutex<Vec<ScopeEntry>> {
    static REGISTRY: OnceLock<Mutex<Vec<ScopeEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Looks up (or registers, on first touch) the [`ScopeId`] for a scope name,
/// tagged `Native` (see [`scope_id_tagged`] for `Interpreter`/`Gpu` scopes).
///
/// Backed by a small `Mutex<Vec<_>>` registry, touched once per unique call
/// site, never on the per-sample hot path.
#[must_use]
pub fn scope_id(name: &'static str) -> ScopeId {
    scope_id_tagged(name, ScopeKind::Native)
}

/// Looks up (or registers, on first touch) the [`ScopeId`] for a scope name
/// tagged with `kind`. Re-registering an existing name with a different
/// `kind` is a programming error and panics, a scope's cost bucket is
/// determined once, at its call site, and must not drift.
///
/// # Panics
///
/// Panics if more than `u16::MAX` distinct scope names are ever registered
/// (a build-time authoring error, not something user input can trigger),
/// silently wrapping the index would alias two unrelated scopes under the
/// same `ScopeId` and corrupt profiling data. Also panics if `name` was
/// already registered under a different `kind`.
pub fn scope_id_tagged(name: &'static str, kind: ScopeKind) -> ScopeId {
    let mut names = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(pos) = names.iter().position(|e| e.name == name) {
        assert!(
            names[pos].kind == kind,
            "telemetry scope {name:?} re-registered with a different ScopeKind"
        );
        return ScopeId(index_to_u16(pos));
    }
    names.push(ScopeEntry { name, kind });
    ScopeId(index_to_u16(names.len() - 1))
}

/// Returns the [`ScopeKind`] a [`ScopeId`] was registered with.
#[must_use]
pub fn scope_kind(id: ScopeId) -> Option<ScopeKind> {
    let names = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    names.get(usize::from(id.0)).map(|e| e.kind)
}

/// Returns the name a [`ScopeId`] was registered with, the overlay/CLI's
/// only way to turn a `Sample` back into a human-readable scope label.
#[must_use]
pub fn scope_name(id: ScopeId) -> Option<&'static str> {
    let names = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    names.get(usize::from(id.0)).map(|e| e.name)
}

fn index_to_u16(index: usize) -> u16 {
    u16::try_from(index).expect("telemetry scope registry exceeded u16::MAX distinct scope names")
}

/// Returns the engine's telemetry epoch, established on first use.
fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Nanoseconds elapsed since the engine's telemetry epoch.
///
/// Backed by `std::time::Instant` (portable, correct everywhere) rather than
/// an always-on `rdtsc` fast path, see RFC-0013 "Rationale and alternatives".
#[must_use]
#[allow(clippy::cast_possible_truncation)] // u64 ns covers ~584 years; never truncates in practice
pub fn now_ns() -> u64 {
    epoch().elapsed().as_nanos() as u64
}

/// One CPU scope timing: a scope identifier, its nesting depth, and its
/// start/end timestamps in nanoseconds since the telemetry [`epoch`].
///
/// `#[repr(C)]` with explicit padding fields (no implicit tail/interior
/// padding) so the type is a clean `bytemuck::Pod`, required to pack a flat
/// byte block that can cross the frame swap as `Send` data (RFC-0013
/// "Hand-off").
///
/// `depth` (RFC-0030 §I2) and `owner` (the self-accounting erratum) occupy
/// the two low bytes of what used to be a `u16` of explicit padding, so
/// [`size_of::<Sample>()`](std::mem::size_of) is unchanged and the block still
/// crosses the frame boundary as plain data.
///
/// `depth` is what makes a *correct* frame total possible: without it, summing
/// a nested scope set double-counts every child inside its parent and reports
/// an 8 ms total for a 4 ms frame. `owner` is what makes a correct
/// *attribution* possible: without it, two producers entering the same scope
/// in one frame are summed into one row and billed to whichever of them the
/// reader assumes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Sample {
    /// Which scope this sample belongs to.
    pub scope: ScopeId,
    /// How deeply this scope was nested inside other scopes on the same
    /// thread when it was entered: `0` for a top-level scope, `1` for a
    /// scope opened inside one, and so on. Read it via [`Sample::depth`].
    depth: u8,
    /// Which logical producer's cost this is, as an [`Owner`] discriminant.
    /// Read it via [`Sample::owner`]; stored raw so the type stays `Pod`.
    owner: u8,
    /// Explicit padding to align `start`/`end` on an 8-byte boundary.
    _reserved_b: u32,
    /// Scope entry time, nanoseconds since the telemetry epoch.
    pub start: u64,
    /// Scope exit time, nanoseconds since the telemetry epoch.
    pub end: u64,
}

/// `depth` must cost nothing: it lives in padding that was already reserved,
/// so the frame-boundary contract (RFC-0001 §5: only `Send` PODs cross) and
/// every existing byte-level consumer are unaffected.
const _: () = assert!(
    std::mem::size_of::<Sample>() == 24,
    "Sample must stay 24 bytes, `depth` and `owner` live in existing padding, not in a new word"
);

impl Sample {
    /// Builds a CPU sample at an explicit nesting `depth`, attributed to
    /// [`Owner::App`].
    ///
    /// [`Guard`] is the normal producer (it maintains `depth` and reads the
    /// thread's current owner itself); this exists for consumers that
    /// reconstruct a block from an external source, a trace importer, a test
    ///, without going through the ring. See [`Sample::owned`] to build one
    /// for another owner.
    #[must_use]
    pub const fn cpu(scope: ScopeId, depth: u8, start: u64, end: u64) -> Self {
        Self::owned(scope, Owner::App, depth, start, end)
    }

    /// Builds a CPU sample attributed to an explicit [`Owner`].
    #[must_use]
    pub const fn owned(scope: ScopeId, owner: Owner, depth: u8, start: u64, end: u64) -> Self {
        Self {
            scope,
            depth,
            owner: owner.as_u8(),
            _reserved_b: 0,
            start,
            end,
        }
    }

    /// Builds a `Gpu`-tagged sample from an already-resolved pass duration
    /// (RFC-0013 "GPU timing") rather than two wall-clock timestamps: GPU
    /// passes are timed by the device's own timestamp queries, resolved
    /// asynchronously, so there is no meaningful CPU-epoch `start` for them.
    /// By convention `start` is `0` and `end` is the duration itself, so
    /// `end - start` (the quantity every consumer actually wants) is still
    /// the pass duration in nanoseconds.
    ///
    /// `depth` is set to `0` explicitly rather than left to the default
    /// (RFC-0030 §Q6): a GPU pass resolves two frames later on a different
    /// timeline and does not nest inside any CPU scope, so the value is
    /// intentional at the construction site rather than incidental.
    #[must_use]
    pub const fn gpu_duration(scope: ScopeId, duration_ns: u64) -> Self {
        Self::cpu(scope, 0, 0, duration_ns)
    }

    /// This sample's *inclusive* duration (`end - start`) in nanoseconds,
    /// its own work plus everything nested inside it. See
    /// [`SampleBlock::self_ns`] for the exclusive counterpart.
    #[must_use]
    pub const fn duration_ns(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// This sample's nesting depth: `0` for a top-level scope, `1` for a
    /// scope opened inside one, and so on (RFC-0030 §I2).
    #[must_use]
    pub const fn depth(&self) -> u8 {
        self.depth
    }

    /// Which logical producer this sample's time belongs to. See [`Owner`].
    #[must_use]
    pub const fn owner(&self) -> Owner {
        Owner::from_u8(self.owner)
    }
}

/// A flat, `Send` snapshot of one thread's ring for a single tick.
///
/// Attached to the [`crate::frame::RenderFrame`] on the existing atomic
/// frame swap (RFC-0013 "Hand-off"), no new channel, no new lock. `samples`
/// holds only [`Sample`], a `Pod` type, so the block is plain data end to
/// end even though the `Vec` wrapper itself isn't `Pod`.
#[derive(Debug, Clone, Default)]
pub struct SampleBlock {
    /// The samples captured this tick, in push order.
    pub samples: Vec<Sample>,
    /// How many samples were dropped this tick because the ring was full.
    pub dropped: u64,
}

impl SampleBlock {
    /// Sums the *inclusive* duration of every sample whose scope is tagged
    /// `kind` (RFC-0013 "the interpreter tax segmentation").
    ///
    /// Inclusive means nested scopes are counted inside their parent, so this
    /// double-counts across a nesting boundary. It is the right measure for a
    /// disjoint bucket (`Gpu` passes never nest, RFC-0030 §Q6) and the wrong
    /// one for `Interpreter`, which contains `Native` layout work; use
    /// [`Self::sum_self_by_kind`] there. [`Self::interpreter_tax_ns`] does.
    #[must_use]
    pub fn sum_by_kind(&self, kind: ScopeKind) -> u64 {
        // Locks the registry once for the whole sum rather than once per
        // sample (`scope_kind` per element would re-lock on every call,
        // needless contention for a consumer aggregating many samples).
        let names = registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.samples
            .iter()
            .filter(|s| {
                names
                    .get(usize::from(s.scope.0))
                    .is_some_and(|e| e.kind == kind)
            })
            .map(Sample::duration_ns)
            .sum()
    }

    /// The *self*-time of the sample at `index`: its inclusive duration minus
    /// the inclusive duration of its direct children (RFC-0030 §I2b).
    ///
    /// Direct children are recoverable from the flat block without building a
    /// tree, because samples are pushed in `Drop` order: a child always
    /// precedes its parent, and the run immediately before a sample at depth
    /// `d`, up to the first sample at depth `≤ d`, is exactly that sample's
    /// subtree. Scanning it backwards and taking only the `d + 1` entries
    /// yields the direct children; deeper entries are grandchildren, already
    /// counted inside a child. One reverse linear pass, no allocation.
    ///
    /// Returns `0` for an out-of-range `index`.
    #[must_use]
    pub fn self_ns(&self, index: usize) -> u64 {
        let Some(sample) = self.samples.get(index) else {
            return 0;
        };
        let mut children_ns: u64 = 0;
        self.for_each_direct_child(index, |_, child| {
            children_ns = children_ns.saturating_add(child.duration_ns());
        });
        sample.duration_ns().saturating_sub(children_ns)
    }

    /// Calls `f(child_index, child)` for each direct child of the sample at
    /// `index`, in **chronological** (entry) order.
    ///
    /// See [`Self::self_ns`] for why one reverse scan is enough to recover
    /// them; this collects the indices into a small stack buffer and replays
    /// them forwards, so a consumer that renders a tree gets parents before
    /// children without the block ever being reordered or copied. A scope with
    /// more direct children than the buffer holds reports the first
    /// [`MAX_DIRECT_CHILDREN`] of them, enough for any scope set a human
    /// reads, and a bounded, allocation-free failure mode rather than a
    /// growing `Vec` on a display path.
    pub fn for_each_direct_child(&self, index: usize, mut f: impl FnMut(usize, &Sample)) {
        let Some(sample) = self.samples.get(index) else {
            return;
        };
        let depth = sample.depth();
        // A saturated depth cannot have distinguishable children: `Guard`
        // clamps at `u8::MAX`, so everything below it was recorded at the same
        // value and is indistinguishable from a sibling. Bail rather than
        // guess, over-attributing here would make `self_ns` negative-ish
        // (clamped to zero) and silently hide a runaway recursion.
        if depth == u8::MAX {
            return;
        }
        let mut found = [0usize; MAX_DIRECT_CHILDREN];
        let mut count = 0;
        for (i, prev) in self.samples[..index].iter().enumerate().rev() {
            if prev.depth() <= depth {
                break;
            }
            if prev.depth() == depth + 1 {
                if count == MAX_DIRECT_CHILDREN {
                    break;
                }
                found[count] = i;
                count += 1;
            }
        }
        // `found` is newest-first; replay it oldest-first.
        for &i in found[..count].iter().rev() {
            f(i, &self.samples[i]);
        }
    }

    /// Calls `f(index, sample)` for each **top-level** (depth-0) sample, in
    /// entry order, the roots of the block's scope forest.
    pub fn for_each_root(&self, mut f: impl FnMut(usize, &Sample)) {
        for (i, sample) in self.samples.iter().enumerate() {
            if sample.depth() == 0 {
                f(i, sample);
            }
        }
    }

    /// Sums the [self-time](Self::self_ns) of every sample tagged `kind`.
    ///
    /// Unlike [`Self::sum_by_kind`] this is safe across nesting boundaries:
    /// each nanosecond of the tick is attributed to exactly one scope, so
    /// summing every kind reproduces [`Self::total_ns`] rather than exceeding
    /// it.
    #[must_use]
    pub fn sum_self_by_kind(&self, kind: ScopeKind) -> u64 {
        let names = registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (0..self.samples.len())
            .filter(|&i| {
                names
                    .get(usize::from(self.samples[i].scope.0))
                    .is_some_and(|e| e.kind == kind)
            })
            .map(|i| self.self_ns(i))
            .sum()
    }

    /// The block's total wall-clock cost: the sum of the *inclusive*
    /// durations of its **depth-0** samples only.
    ///
    /// Summing every sample instead would count a nested scope twice, once
    /// on its own row and once inside its parent, and report a total larger
    /// than the frame it measures. A profiler that overstates the frame it
    /// measures destroys the credibility of every other number the project
    /// publishes (RFC-0030 §I2), so this is the accessor consumers should
    /// reach for.
    #[must_use]
    pub fn total_ns(&self) -> u64 {
        self.samples
            .iter()
            .filter(|s| s.depth() == 0)
            .map(Sample::duration_ns)
            .sum()
    }

    /// The self-time of every sample that is both tagged `kind` and stamped
    /// `owner`, [`sum_self_by_kind`](Self::sum_self_by_kind) narrowed to one
    /// producer.
    ///
    /// The interpreter tax is the motivating case. It answers "what would an
    /// AOT build of *this app* stop paying", and the dev HUD is written in
    /// `byld` too, so its interpreter cost is real interpreter cost belonging
    /// to somebody else. Without the narrowing, opening the HUD raises the
    /// developer's tax figure by the HUD's own tree, an observer effect
    /// reported as an app regression (RFC-0030 INV-24).
    #[must_use]
    pub fn owner_kind_self_ns(&self, owner: Owner, kind: ScopeKind) -> u64 {
        let names = registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (0..self.samples.len())
            .filter(|&i| {
                self.samples[i].owner() == owner
                    && names
                        .get(usize::from(self.samples[i].scope.0))
                        .is_some_and(|e| e.kind == kind)
            })
            .map(|i| self.self_ns(i))
            .sum()
    }

    /// The total cost attributable to `owner` this tick: the sum of the
    /// [self-time](Self::self_ns) of every sample it stamped.
    ///
    /// **Self**-time, so this is safe across nesting in both directions. A
    /// dev-owned scope nested inside an app-owned parent (`encode.glyphs.dev`
    /// inside `encode.glyphs`) is subtracted from the parent's self-time and
    /// added here exactly once; an app-owned scope nested inside a dev-owned
    /// parent would behave symmetrically. Consequently
    /// `owner_total_ns(App) + owner_total_ns(DevTools) == total_ns()` for any
    /// well-formed block, every nanosecond of the tick is attributed to
    /// exactly one owner, and the two buckets reconstruct the frame rather than
    /// exceeding it.
    ///
    /// This is the accessor RFC-0030 §V4's subtraction is built on: it is the
    /// *whole* cost of a producer, wherever in the scope forest that producer's
    /// work happened to land, rather than the inclusive time of one scope that
    /// happens to carry its name.
    #[must_use]
    pub fn owner_total_ns(&self, owner: Owner) -> u64 {
        (0..self.samples.len())
            .filter(|&i| self.samples[i].owner() == owner)
            .map(|i| self.self_ns(i))
            .sum()
    }

    /// The total `Interpreter`-tagged time this tick, the tax an AOT release
    /// build does not pay. Overlay/CLI consumers sum this bucket separately
    /// from the rest of `frame.total` (RFC-0013 "the honest number").
    ///
    /// This is **self**-time, not inclusive time (RFC-0030 §I2b). The
    /// distinction is load-bearing rather than pedantic: `layout.taffy` is
    /// `Native` and nests strictly inside `interp.render`, which is
    /// `Interpreter`, so an inclusive sum bills Taffy to the interpreter,
    /// and an AOT build still pays for layout in full. [`project_aot`] would
    /// then return an estimate optimistic by the entire cost of layout, and
    /// would push the RFC-0014 JIT decision towards "the interpreter is the
    /// problem", which is the expensive direction to be wrong in.
    #[must_use]
    pub fn interpreter_tax_ns(&self) -> u64 {
        self.sum_self_by_kind(ScopeKind::Interpreter)
    }
}

/// A fixed-capacity, non-circular sample buffer: once full, new samples are
/// dropped (not the oldest) so an in-flight capture is never overwritten
/// mid-frame (RFC-0013 **P1**).
struct Ring {
    buf: Box<[Sample]>,
    len: usize,
    dropped: u64,
}

impl Ring {
    fn new() -> Self {
        Self {
            // Built directly on the heap (never as a stack array first),
            // `RING_CAPACITY * size_of::<Sample>()` is too large to build on
            // the stack before boxing.
            buf: vec![Sample::default(); RING_CAPACITY].into_boxed_slice(),
            len: 0,
            dropped: 0,
        }
    }

    /// Pushes a sample. Never allocates: writes into the preallocated
    /// buffer, or increments the dropped counter once full.
    fn push(&mut self, sample: Sample) {
        if self.len < RING_CAPACITY {
            self.buf[self.len] = sample;
            self.len += 1;
        } else {
            self.dropped += 1;
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Copies out the current contents into `out` and resets the ring for
    /// the next tick.
    ///
    /// Reuses `out.samples`' existing heap allocation (`Vec::clear` +
    /// `extend_from_slice`) instead of allocating a fresh `Vec` every tick,
    /// so a caller that keeps its `SampleBlock` around (e.g. a recycled
    /// [`crate::frame::RenderFrame`]) drains at steady-state zero
    /// allocation once its capacity has grown to fit a typical tick.
    fn drain_into(&mut self, out: &mut SampleBlock) {
        out.samples.clear();
        out.samples.extend_from_slice(&self.buf[..self.len]);
        out.dropped = self.dropped;
        self.len = 0;
        self.dropped = 0;
    }
}

thread_local! {
    static RING: RefCell<Ring> = RefCell::new(Ring::new());
}

/// Writes one [`Sample`] into the calling thread's ring. Never allocates.
pub fn push_sample(sample: Sample) {
    RING.with(|r| r.borrow_mut().push(sample));
}

/// Returns the number of samples currently held in the calling thread's ring.
#[must_use]
pub fn ring_len() -> usize {
    RING.with(|r| r.borrow().len())
}

/// Returns the number of samples dropped so far this tick on the calling
/// thread because its ring was full.
#[must_use]
pub fn ring_dropped() -> u64 {
    RING.with(|r| r.borrow().dropped())
}

/// Drains the calling thread's ring into `out`, resetting the ring for the
/// next tick and reusing `out`'s existing `Vec` allocation.
///
/// This is the hot path, [`crate::frame::RenderFrame::drain_telemetry`]
/// calls this on the logic thread right before [`crate::relay::Relay::publish`]
/// swaps the frame in, so a recycled frame's `SampleBlock` never reallocates
/// once it has grown to fit a typical tick.
pub fn drain_samples_into(out: &mut SampleBlock) {
    RING.with(|r| r.borrow_mut().drain_into(out));
}

/// Drains the calling thread's ring into a freshly allocated [`SampleBlock`].
///
/// Convenience for tests and one-off call sites; steady-state hot paths
/// should prefer [`drain_samples_into`] with a reused buffer.
#[must_use]
pub fn drain_samples() -> SampleBlock {
    let mut block = SampleBlock::default();
    drain_samples_into(&mut block);
    block
}

/// A calibrated interpreter-vs-native cost ratio (RFC-0013 **P4**): a fixed
/// set of microbenchmarks (e.g. `byard-core/benches/telemetry_calibration.rs`,
/// signal read / element construct / memo eval), refreshed per release,
/// never measured live, which would re-add observer overhead.
#[derive(Debug, Clone, Copy)]
pub struct Calibration {
    /// Where this ratio came from, always shown alongside a projection so
    /// the number is legible as an estimate, never a hard promise (P3).
    pub basis: &'static str,
    /// `native_ns ≈ interpreter_ns * ratio` for representative interpreter
    /// operations, as measured by the calibration benchmarks.
    pub interpreter_to_native_ratio: f64,
}

/// A projected "what would this cost in an AOT release build" estimate
/// (RFC-0013 **P3**): opt-in, and always carries its [`Calibration::basis`]
/// so the overlay/CLI can show the number is an estimate, not a measurement.
#[derive(Debug, Clone, Copy)]
pub struct Projection {
    /// The projected total frame cost in nanoseconds.
    pub projected_ns: u64,
    /// The calibration basis this projection was computed from.
    pub basis: &'static str,
}

/// Projects an AOT estimate from a tick's measured total and its
/// [`SampleBlock::interpreter_tax_ns`] (RFC-0013 "The interpreter tax
/// segmentation"): `native ≈ total − interp_measured + interp_native_equiv`,
/// where `interp_native_equiv` comes from `calibration`.
///
/// Never called implicitly, a caller opts in by calling this and choosing to
/// display the result (P3: "opt-in, always shown with its basis").
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)] // frame times never approach f64's precision or u64's range
pub fn project_aot(total_ns: u64, interpreter_ns: u64, calibration: &Calibration) -> Projection {
    let interp_native_equiv =
        (interpreter_ns as f64 * calibration.interpreter_to_native_ratio) as u64;
    Projection {
        projected_ns: total_ns
            .saturating_sub(interpreter_ns)
            .saturating_add(interp_native_equiv),
        basis: calibration.basis,
    }
}

thread_local! {
    /// How many [`Guard`]s are currently live on this thread, the depth the
    /// *next* one records. Maintained by `Guard::new`/`Guard::drop` only.
    static DEPTH: Cell<u8> = const { Cell::new(0) };
}

/// The nesting depth the next scope entered on this thread would record.
///
/// Exposed for tests and for consumers that construct samples by hand; the
/// profiler maintains it internally and no hot path needs to read it.
#[must_use]
pub fn current_depth() -> u8 {
    DEPTH.with(Cell::get)
}

thread_local! {
    /// Which [`Owner`] the next scope entered on this thread will be stamped
    /// with. Maintained by [`attribute_to`] and `OwnerGuard::drop` only.
    static OWNER: Cell<Owner> = const { Cell::new(Owner::App) };
}

/// The owner the next scope entered on this thread would be stamped with.
#[must_use]
pub fn current_owner() -> Owner {
    OWNER.with(Cell::get)
}

/// RAII guard returned by [`attribute_to`]: restores the previous owner on
/// drop.
///
/// Like [`Guard`] it *restores* rather than resets to a default, so nesting
/// composes and an unwind through an attributed region cannot leave the thread
/// permanently mis-attributed.
pub struct OwnerGuard {
    previous: Owner,
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        OWNER.with(|o| o.set(self.previous));
    }
}

/// Attributes every scope entered on this thread, for as long as the returned
/// guard lives, to `owner`, including scopes entered by code that has never
/// heard of [`Owner`].
///
/// That last property is the whole point. The dev HUD's cost is not a call it
/// makes; it is the interpreter, the layout atlas and the shaper doing their
/// ordinary jobs on the HUD's behalf, in code shared with the app. Threading an
/// owner parameter through all of it would be invasive, easy to get wrong in
/// one branch, and would have to be repeated for the next producer. A
/// thread-local set at the boundary attributes the whole subtree by
/// construction.
///
/// ```
/// # use byard_core::telemetry::{attribute_to, Owner, current_owner};
/// assert_eq!(current_owner(), Owner::App);
/// {
///     let _dev = attribute_to(Owner::DevTools);
///     assert_eq!(current_owner(), Owner::DevTools);
/// }
/// assert_eq!(current_owner(), Owner::App);
/// ```
#[must_use = "the attribution ends when the guard is dropped; binding it to `_` ends it immediately"]
pub fn attribute_to(owner: Owner) -> OwnerGuard {
    OwnerGuard {
        previous: OWNER.with(|o| o.replace(owner)),
    }
}

/// RAII guard produced by [`profile_scope!`]; writes one [`Sample`] to the
/// calling thread's ring when dropped.
///
/// The guard also maintains the thread's nesting depth (RFC-0030 §I2). It
/// *restores* the entry value on drop rather than decrementing a counter, so
/// an unbalanced sequence, a guard leaked with `mem::forget`, an unwind
/// across a scope, cannot leave the depth permanently skewed: the next outer
/// guard to drop resets it to a known-correct value.
pub struct Guard {
    scope: ScopeId,
    depth: u8,
    owner: Owner,
    start: u64,
}

impl Guard {
    /// Starts timing `scope` now, at the calling thread's current nesting
    /// depth and under its current [`Owner`].
    ///
    /// The owner is read at **entry**, not at exit, so a guard that is still
    /// open when an [`OwnerGuard`] around it is dropped still records the owner
    /// its work actually ran under.
    #[must_use]
    pub fn new(scope: ScopeId) -> Self {
        let depth = DEPTH.with(|d| {
            let entry = d.get();
            // Saturating rather than wrapping: a runaway recursion must not
            // make a depth-256 scope look like a top-level one and get summed
            // into the frame total a second time.
            d.set(entry.saturating_add(1));
            entry
        });
        Self {
            scope,
            depth,
            owner: current_owner(),
            start: now_ns(),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(self.depth));
        push_sample(Sample::owned(
            self.scope,
            self.owner,
            self.depth,
            self.start,
            now_ns(),
        ));
    }
}

/// A [`Guard`] and an [`OwnerGuard`] taken together, for a region that is only
/// *sometimes* somebody else's.
///
/// [`profile_scope!`] is a statement, so it cannot be opened conditionally,
/// and the encoder needs exactly that: one iteration of a loop is the dev
/// runner's and the next is the app's, with the loop body identical. Holding
/// both guards in one value makes `cond.then(|| attributed_scope(..))` express
/// it without duplicating the body.
///
/// Empty when the `telemetry` feature is off, so the conditional costs a
/// branch and nothing else.
pub struct AttributedScope {
    #[cfg(feature = "telemetry")]
    // Field order is drop order, and neither guard depends on the other:
    // `Guard` captured its owner when it was constructed.
    _scope: Guard,
    #[cfg(feature = "telemetry")]
    _owner: OwnerGuard,
}

/// Opens `scope` attributed to `owner`, until the returned value is dropped.
///
/// The scope id is taken rather than a name so the caller can cache it in a
/// call-site `OnceLock`, exactly as [`profile_scope!`] does, the registry is a
/// mutex, and a loop is not the place to lock it.
#[must_use = "the scope closes when the returned value is dropped"]
pub fn attributed_scope(scope: ScopeId, owner: Owner) -> AttributedScope {
    #[cfg(feature = "telemetry")]
    {
        // The owner is set *before* the guard is constructed, because `Guard`
        // reads it at entry, the order here is the whole mechanism.
        let owner_guard = attribute_to(owner);
        AttributedScope {
            _scope: Guard::new(scope),
            _owner: owner_guard,
        }
    }
    #[cfg(not(feature = "telemetry"))]
    {
        let _ = (scope, owner);
        AttributedScope {}
    }
}

/// Times the rest of the enclosing block as a named scope.
///
/// ```
/// # use byard_core::profile_scope;
/// fn build_frame() {
///     profile_scope!("frame.total");
///     // ... work ...
/// }
/// ```
///
/// Expands to a no-op when `byard-core`'s `telemetry` feature is off, zero
/// cost in a build that disables it. The feature is resolved **here**, at the
/// definition site, by selecting between two macro definitions: a
/// `#[cfg(feature = "telemetry")]` inside the expansion would be evaluated
/// against the *calling* crate's feature set instead, so the macro would
/// silently compile to nothing in every crate but this one.
#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! profile_scope {
    ($name:expr) => {
        $crate::profile_scope!($name, $crate::telemetry::ScopeKind::Native);
    };
    ($name:expr, $kind:expr) => {
        let _guard = {
            static SCOPE: ::std::sync::OnceLock<$crate::telemetry::ScopeId> =
                ::std::sync::OnceLock::new();
            let id = *SCOPE.get_or_init(|| $crate::telemetry::scope_id_tagged($name, $kind));
            $crate::telemetry::Guard::new(id)
        };
    };
}

/// The `telemetry`-off definition: the scope name and kind are still
/// type-checked (so a stale scope cannot rot behind the feature gate), but
/// nothing is evaluated and no guard is constructed.
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! profile_scope {
    ($name:expr) => {
        $crate::profile_scope!($name, $crate::telemetry::ScopeKind::Native);
    };
    ($name:expr, $kind:expr) => {
        if false {
            let _: (&'static str, $crate::telemetry::ScopeKind) = ($name, $kind);
        }
    };
}

#[cfg(test)]
mod tests {
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
}
