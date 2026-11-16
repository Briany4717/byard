//! The reactive core: read-tracking, Mark-and-Pull, pull-based memos, and
//! structural effects (RFC-0004 in full; RFC-0002 D1/D2).
//!
//! This is the highest-risk Phase 2 module. It implements automatic reactivity
//! as a *read-tracking* layer over a subscription store, exactly as RFC-0004
//! specifies:
//!
//! - A logic-thread-local `CURRENT_SCOPE` names the computation being
//!   evaluated (§2). Every `read_signal`/`read_memo` while a scope is active
//!   records a subscription, so dependency edges are *discovered*, never
//!   declared.
//! - **Mark** (§4) is synchronous and idempotent: a mutation sets dirty bits
//!   and enqueues work, never computing a value. Memos cascade the mark to
//!   their subscribers; bindings/structurals enqueue.
//! - **Pull** (§5) runs once per tick (the consistency boundary): structural
//!   effects first (mounts create new bindings), then value bindings, each at
//!   most once per tick via an epoch guard. Memos are **pull-on-read** (§6),
//!   recomputing lazily against a fully-settled mark set, the glitch-free
//!   diamond solution.
//! - Dynamic dependencies (§3): every (re)evaluation clears the scope's old
//!   subscriptions before re-tracking.
//!
//! ## How computations are plugged in
//!
//! RFC-0004's `walk_expr(scope.expr)` is the eval driver's job (M9). To keep
//! this module testable in isolation against the §"Test fixtures" matrix, a
//! scope's computation is a boxed `FnMut(&mut ReactiveCtx) -> Value` closure
//! (the AST walker, in M9; a plain Rust closure, in these tests). The closure
//! is *taken out* of the context for the duration of its own evaluation, so the
//! borrow checker is satisfied without `RefCell` and a self-read cycle is caught
//! by the `evaluating` trip-wire (§6) rather than aliasing.

use std::cell::Cell;

use smallvec::SmallVec;

use super::env::{SignalId, Value};

thread_local! {
    /// The scope currently being evaluated, or `None` outside any scope (and
    /// under [`untrack`]). Logic-thread-local: RFC-0001 §5.1 confines all of
    /// this to one thread, so no atomics or locks are needed.
    static CURRENT_SCOPE: Cell<Option<ScopeId>> = const { Cell::new(None) };
}

/// Identifies a reactive scope (value binding, memo, or structural effect).
/// A value binding's id doubles as its RFC-0001 §2.2 dirty-flag target.
///
/// A retired scope's slot is handed out again, so the id carries the
/// **generation** of the slot it was minted for. An id that outlived its scope
/// (a closure captured in a reply that arrives after the screen it belonged to
/// was popped) then names a dead scope and is refused, rather than silently
/// naming whichever scope took the slot next: the failure mode of recycling
/// without it is a wrong value in the wrong place, with nothing to say so.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct ScopeId {
    index: u32,
    generation: u32,
}

impl ScopeId {
    /// The slot this id names in the scope arena.
    #[inline]
    const fn index(self) -> usize {
        self.index as usize
    }

    /// An identity of this scope alone, for a node that has no uid of its own
    /// but owns a scope no other does (a `Text`'s content, an `Image`'s source).
    #[inline]
    pub(crate) const fn key(self) -> u64 {
        (1 << 63) | ((self.generation as u64) << 32) | self.index as u64
    }
}

/// Identifies a `RenderFrame` field a value binding projects into. Opaque to
/// the reactive core; the eval driver (M9) maps it to a concrete primitive
/// field.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct FrameTarget(pub u32);

/// Which structural construct a `ScopeKind::Structural` scope drives.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum StructuralKind {
    /// `when cond { … } else { … }`.
    When,
    /// `for item in list { … }`.
    For,
}

/// A computation: re-walked to produce a [`Value`], recording its reads as
/// subscriptions while it runs.
type Compute = Box<dyn FnMut(&mut ReactiveCtx) -> Value>;

/// Mounts a `when` branch, returning the scope ids it created (so they can be
/// dropped on the next toggle).
type BranchMount = Box<dyn FnMut(&mut ReactiveCtx) -> Vec<ScopeId>>;

/// Mounts one `for` item, returning the scope ids it created.
type ItemMount = Box<dyn FnMut(&mut ReactiveCtx, &Value) -> Vec<ScopeId>>;

/// A recorded dependency edge from a scope to a source it read.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Dep {
    Signal(SignalId),
    Memo(ScopeId),
}

/// Per-`when`/`for` mount state, kept outside the scope so it can be taken out
/// during reconciliation without aliasing the scope arena.
enum Mount {
    When {
        then: BranchMount,
        els: Option<BranchMount>,
        active: Option<bool>,
        children: Vec<ScopeId>,
    },
    For {
        item: ItemMount,
        children: Vec<ScopeId>,
    },
}

/// What kind of scope this is, plus its kind-specific state.
enum ScopeKind {
    /// Projects an expression into one `RenderFrame` field.
    ValueBinding {
        target: FrameTarget,
        last_written: Option<Value>,
    },
    /// A `let`/`fn` computed value: pull-based, memoized.
    Memo {
        cache: Value,
        subs: SmallVec<[ScopeId; 4]>,
        evaluating: bool,
    },
    /// A `for`/`when` structural effect.
    Structural { kind: StructuralKind },
}

/// One reactive scope. Liveness is tracked so structural unmount can retire a
/// scope without shifting other scopes' ids.
struct Scope {
    kind: ScopeKind,
    deps: SmallVec<[Dep; 4]>,
    dirty: bool,
    last_epoch: u32,
    eval_count: u32,
    live: bool,
    /// The [`ReactiveCtx::seq`] at which a memo was last evaluated or a value
    /// binding last projected a different value (see [`ReadSet`]).
    changed: u64,
    /// Bumped each time the slot is retired, so a [`ScopeId`] minted for an
    /// earlier occupant no longer matches.
    generation: u32,
}

/// A reactive source: a value plus the scopes subscribed to it.
struct SignalCell {
    value: Value,
    subscribers: SmallVec<[ScopeId; 4]>,
    /// The [`ReactiveCtx::seq`] of the last write (RFC-0068 §5: what a surface
    /// asks to know whether something it read has changed since it drew).
    written: u64,
}

/// What a surface read while it was drawn: one bit per signal and one per scope
/// (a memo or a value binding), by slot. Kept per surface, so that a tick can
/// ask whether anything the surface showed has changed, without walking it
/// (RFC-0068 §5).
#[derive(Clone, Debug, Default)]
pub struct ReadSet {
    signals: Vec<u64>,
    scopes: Vec<u64>,
}

impl ReadSet {
    fn set(words: &mut Vec<u64>, bit: usize) {
        let (w, b) = (bit / 64, bit % 64);
        if words.len() <= w {
            words.resize(w + 1, 0);
        }
        words[w] |= 1 << b;
    }

    fn bits(words: &[u64]) -> impl Iterator<Item = usize> + '_ {
        words.iter().enumerate().flat_map(|(w, &word)| {
            (0..64)
                .filter(move |b| word >> b & 1 == 1)
                .map(move |b| w * 64 + b)
        })
    }

    /// Whether nothing was read.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.signals.iter().chain(&self.scopes).all(|w| *w == 0)
    }
}

/// The reactive context: owns the scope arena, the signal store, the per-tick
/// dirty queues, and the epoch counter (RFC-0004 §12).
#[derive(Default)]
pub struct ReactiveCtx {
    scopes: Vec<Scope>,
    /// Parallel to `scopes`: each scope's computation, taken out while it runs.
    computes: Vec<Option<Compute>>,
    /// Parallel to `scopes`: structural mount state, present only for
    /// `Structural` scopes.
    mounts: Vec<Option<Mount>>,
    /// Retired slots, reused (last in, first out) before the arena grows. A
    /// screen that is pushed and popped over and over then costs the same
    /// memory the first time and the thousandth.
    free_scopes: Vec<u32>,
    /// Open recordings of the scopes allocated since a
    /// [`push_scope_tracker`](Self::push_scope_tracker). Empty (and a single
    /// `last_mut` check per allocation) unless a lazily lowered subtree is being
    /// lowered.
    trackers: Vec<Vec<ScopeId>>,
    signals: Vec<SignalCell>,
    dirty_bindings: Vec<ScopeId>,
    dirty_structural: Vec<ScopeId>,
    epoch: u32,
    /// Observability for the fixtures: how many frame-field writes the last
    /// pull(s) performed (the value-equality cut means an unchanged projection
    /// records nothing).
    frame_writes: Vec<(FrameTarget, Value)>,
    /// Cumulative count of *effective* mark visits (a scope dirtied for the
    /// first time), to prove the cascade is idempotent and never exponential.
    mark_effective: u32,
    /// The signals written since [`start_write_log`](Self::start_write_log),
    /// `None` (and no cost beyond the check) when nobody is listening.
    write_log: Option<Vec<SignalId>>,
    /// What was read since [`start_read_log`](Self::start_read_log), `None`
    /// when nobody is listening.
    read_log: Option<(Vec<SignalId>, Vec<ScopeId>)>,
    /// A counter that moves on every signal write, every memo evaluation and
    /// every value binding that projects something new (RFC-0068 §5). A surface
    /// remembers it when it is drawn, and what it read has changed since if a
    /// read signal or scope carries a larger number.
    seq: u64,
    /// Whether reads are being recorded into `reads`. Off, and one flag test per
    /// read, unless a multi-surface program is drawing a surface.
    watching: Cell<bool>,
    /// What the surface being drawn has read.
    reads: std::cell::RefCell<ReadSet>,
}

impl ReactiveCtx {
    /// Creates an empty reactive context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ── construction ────────────────────────────────────────────────────

    /// Allocates a reactive source with `initial` value.
    pub fn create_signal(&mut self, initial: Value) -> SignalId {
        let id = SignalId(self.signals.len() as u32);
        self.signals.push(SignalCell {
            value: initial,
            subscribers: SmallVec::new(),
            written: 0,
        });
        id
    }

    fn push_scope(
        &mut self,
        kind: ScopeKind,
        compute: Option<Compute>,
        mount: Option<Mount>,
    ) -> ScopeId {
        let id = if let Some(index) = self.free_scopes.pop() {
            let slot = index as usize;
            let scope = &mut self.scopes[slot];
            scope.kind = kind;
            scope.dirty = true;
            scope.last_epoch = 0;
            scope.eval_count = 0;
            scope.live = true;
            scope.changed = self.seq;
            self.computes[slot] = compute;
            self.mounts[slot] = mount;
            ScopeId {
                index,
                generation: scope.generation,
            }
        } else {
            let index = self.scopes.len() as u32;
            self.scopes.push(Scope {
                kind,
                deps: SmallVec::new(),
                dirty: true,
                last_epoch: 0,
                eval_count: 0,
                live: true,
                changed: self.seq,
                generation: 0,
            });
            self.computes.push(compute);
            self.mounts.push(mount);
            ScopeId {
                index,
                generation: 0,
            }
        };
        if let Some(tracker) = self.trackers.last_mut() {
            tracker.push(id);
        }
        id
    }

    /// Whether `s` still names the scope it was minted for: the slot is live
    /// *and* has not been retired and reused since.
    #[inline]
    fn is_live(&self, s: ScopeId) -> bool {
        self.scopes
            .get(s.index())
            .is_some_and(|scope| scope.live && scope.generation == s.generation)
    }

    /// The scope `s` names, or `None` if it has been retired.
    #[inline]
    fn scope(&self, s: ScopeId) -> Option<&Scope> {
        self.scopes
            .get(s.index())
            .filter(|scope| scope.live && scope.generation == s.generation)
    }

    /// Starts recording every scope allocated until the matching
    /// [`pop_scope_tracker`](Self::pop_scope_tracker).
    ///
    /// For a subtree that is lowered lazily and discarded as a unit (a route,
    /// a `when` branch, a `for` row): the owner keeps the list and retires it
    /// with the subtree. Trackers nest, and a scope belongs to the innermost
    /// one open when it was allocated, so no scope is ever listed twice.
    pub fn push_scope_tracker(&mut self) {
        self.trackers.push(Vec::new());
    }

    /// Stops the innermost recording and returns what it saw.
    pub fn pop_scope_tracker(&mut self) -> Vec<ScopeId> {
        self.trackers.pop().unwrap_or_default()
    }

    /// Overwrites a signal's value without marking its subscribers.
    ///
    /// For releasing what a signal pinned once nothing can read it: the element
    /// of a `for` row that left the list. Marking would dirty scopes that are
    /// not on screen, to compute a value nobody will look at.
    pub fn reset_signal_silent(&mut self, sig: SignalId, value: Value) {
        self.signals[sig.0 as usize].value = value;
    }

    /// Opens a computed memo (`let`/`fn`). Starts dirty: its first read
    /// computes it.
    pub fn open_memo(
        &mut self,
        compute: impl FnMut(&mut ReactiveCtx) -> Value + 'static,
    ) -> ScopeId {
        self.push_scope(
            ScopeKind::Memo {
                cache: Value::Unit,
                subs: SmallVec::new(),
                evaluating: false,
            },
            Some(Box::new(compute)),
            None,
        )
    }

    /// Opens a value binding projecting `compute` into `target`. Enqueued dirty
    /// so the next [`ReactiveCtx::pull`] evaluates it.
    pub fn open_value_binding(
        &mut self,
        target: FrameTarget,
        compute: impl FnMut(&mut ReactiveCtx) -> Value + 'static,
    ) -> ScopeId {
        let id = self.push_scope(
            ScopeKind::ValueBinding {
                target,
                last_written: None,
            },
            Some(Box::new(compute)),
            None,
        );
        self.dirty_bindings.push(id);
        id
    }

    /// Opens a `when` structural effect. `cond` evaluates the condition; `then`
    /// / `els` mount the corresponding branch.
    pub fn open_when(
        &mut self,
        cond: impl FnMut(&mut ReactiveCtx) -> Value + 'static,
        then: impl FnMut(&mut ReactiveCtx) -> Vec<ScopeId> + 'static,
        els: Option<BranchMount>,
    ) -> ScopeId {
        let id = self.push_scope(
            ScopeKind::Structural {
                kind: StructuralKind::When,
            },
            Some(Box::new(cond)),
            Some(Mount::When {
                then: Box::new(then),
                els,
                active: None,
                children: Vec::new(),
            }),
        );
        self.dirty_structural.push(id);
        id
    }

    /// Opens a `for` structural effect. `iter` evaluates the list; `item` mounts
    /// one child per element.
    pub fn open_for(
        &mut self,
        iter: impl FnMut(&mut ReactiveCtx) -> Value + 'static,
        item: impl FnMut(&mut ReactiveCtx, &Value) -> Vec<ScopeId> + 'static,
    ) -> ScopeId {
        let id = self.push_scope(
            ScopeKind::Structural {
                kind: StructuralKind::For,
            },
            Some(Box::new(iter)),
            Some(Mount::For {
                item: Box::new(item),
                children: Vec::new(),
            }),
        );
        self.dirty_structural.push(id);
        id
    }

    /// Replaces a scope's computation (used by the cycle fixture and by
    /// hot-reload). Marks the scope dirty.
    pub fn set_compute(
        &mut self,
        s: ScopeId,
        compute: impl FnMut(&mut ReactiveCtx) -> Value + 'static,
    ) {
        self.computes[s.index()] = Some(Box::new(compute));
        self.scopes[s.index()].dirty = true;
    }

    // ── reads (tracking-aware) ──────────────────────────────────────────

    /// Reads a signal, subscribing the current scope (if any) to it (§2).
    pub fn read_signal(&mut self, sig: SignalId) -> Value {
        if let Some((signals, _)) = &mut self.read_log {
            signals.push(sig);
        }
        if self.watching.get() {
            ReadSet::set(&mut self.reads.get_mut().signals, sig.0 as usize);
        }
        if let Some(s) = CURRENT_SCOPE.with(Cell::get) {
            self.subscribe_signal(sig, s);
            self.record_dep(s, Dep::Signal(sig));
        }
        self.signals[sig.0 as usize].value.clone()
    }

    /// Reads a memo, subscribing the current scope to it and pulling it on
    /// demand if dirty (§6).
    pub fn read_memo(&mut self, m: ScopeId) -> Value {
        if let Some((_, memos)) = &mut self.read_log {
            memos.push(m);
        }
        if self.watching.get() {
            ReadSet::set(&mut self.reads.get_mut().scopes, m.index());
        }
        // A memo whose screen has been popped reads as nothing, never as the
        // scope that has since taken its slot (see [`ScopeId`]).
        if !self.is_live(m) {
            return Value::Unit;
        }
        if let Some(s) = CURRENT_SCOPE.with(Cell::get) {
            self.subscribe_memo(m, s);
            self.record_dep(s, Dep::Memo(m));
        }
        let dirty = self.scopes[m.index()].dirty;
        if dirty {
            if let ScopeKind::Memo { evaluating, .. } = &self.scopes[m.index()].kind {
                debug_assert!(!evaluating, "reactive cycle through memo {m:?}");
            }
            self.set_memo_evaluating(m, true);
            let v = self.evaluate_scope(m);
            self.set_memo_cache(m, v);
            self.seq += 1;
            self.scopes[m.index()].changed = self.seq;
            self.scopes[m.index()].dirty = false;
            self.set_memo_evaluating(m, false);
        }
        match &self.scopes[m.index()].kind {
            ScopeKind::Memo { cache, .. } => cache.clone(),
            _ => Value::Unit,
        }
    }

    // ── mutation: the mark cascade (§4) ─────────────────────────────────

    /// Reads a signal's current value **without** tracking, for mutation
    /// l-values (`count++`) and other actions that are not reactive
    /// projections.
    #[must_use]
    pub fn peek_signal(&self, sig: SignalId) -> Value {
        self.signals[sig.0 as usize].value.clone()
    }

    /// Writes a new value to a signal and runs the synchronous mark cascade
    /// (the mutation entry point).
    pub fn write_signal(&mut self, sig: SignalId, value: Value) {
        if let Some(log) = &mut self.write_log {
            log.push(sig);
        }
        self.seq += 1;
        let cell = &mut self.signals[sig.0 as usize];
        cell.value = value;
        cell.written = self.seq;
        self.mark(sig);
    }

    /// Starts recording the signals and memos read, until
    /// [`take_read_log`](Self::take_read_log).
    pub fn start_read_log(&mut self) {
        self.read_log = Some((Vec::new(), Vec::new()));
    }

    /// Stops recording and returns the signals and memos read.
    pub fn take_read_log(&mut self) -> (Vec<SignalId>, Vec<ScopeId>) {
        self.read_log.take().unwrap_or_default()
    }

    /// Starts recording which signals are written, until
    /// [`take_write_log`](Self::take_write_log).
    pub fn start_write_log(&mut self) {
        self.write_log = Some(Vec::new());
    }

    /// Stops recording and returns the signals written since it started.
    pub fn take_write_log(&mut self) -> Vec<SignalId> {
        self.write_log.take().unwrap_or_default()
    }

    /// Whether the computation `scope` reads, however far down its memos go,
    /// any of `sigs`.
    #[must_use]
    pub fn memo_reads_any(&self, scope: ScopeId, sigs: &[SignalId]) -> bool {
        let mut stack = vec![scope];
        let mut seen = 0_u32;
        while let Some(s) = stack.pop() {
            // A chain of memos is short; this bounds a pathological graph.
            seen += 1;
            if seen > 256 || !self.is_live(s) {
                continue;
            }
            for dep in &self.scopes[s.index()].deps {
                match dep {
                    Dep::Signal(sig) if sigs.contains(sig) => return true,
                    Dep::Signal(_) => {}
                    Dep::Memo(m) => stack.push(*m),
                }
            }
        }
        false
    }

    /// Marks every subscriber of `sig` dirty (§4).
    pub fn mark(&mut self, sig: SignalId) {
        let subs = self.signals[sig.0 as usize].subscribers.clone();
        for s in subs {
            self.mark_scope(s);
        }
    }

    fn mark_scope(&mut self, s: ScopeId) {
        if !self.is_live(s) {
            return;
        }
        {
            let scope = &mut self.scopes[s.index()];
            if scope.dirty {
                return; // IDEMPOTENT, stop re-traversal (D1)
            }
            scope.dirty = true;
        }
        self.mark_effective += 1;
        // Extract the cascade target into owned data before recursing, so no
        // borrow of `self.scopes` is held across the recursive `mark_scope`.
        let memo_subs: Option<SmallVec<[ScopeId; 4]>> = match &self.scopes[s.index()].kind {
            ScopeKind::ValueBinding { .. } => {
                self.dirty_bindings.push(s);
                None
            }
            ScopeKind::Structural { .. } => {
                self.dirty_structural.push(s);
                None
            }
            ScopeKind::Memo { subs, .. } => Some(subs.clone()),
        };
        if let Some(subs) = memo_subs {
            for sub in subs {
                self.mark_scope(sub); // cascade; memo not recomputed here
            }
        }
    }

    // ── the tick: begin + pull (§5) ─────────────────────────────────────

    /// Begins a tick, bumping the epoch. Each dirty scope evaluates at most
    /// once per epoch.
    pub fn begin_tick(&mut self) -> u32 {
        self.epoch += 1;
        self.frame_writes.clear();
        self.epoch
    }

    /// Pulls all dirty scopes for `epoch`: structural effects first (their
    /// mounts create new bindings), then value bindings, each guarded to one
    /// evaluation per epoch. The frame write is value-equality–gated (§5/§7).
    pub fn pull(&mut self, epoch: u32) {
        // RFC-0030 §I1, the reactive tick itself: memo re-evaluation,
        // structural reconciliation and frame projection. Declared on `pull`
        // rather than on `Interpreter::tick` so every driver is covered by
        // construction, including the fixpoint re-pulls that
        // `Interpreter::render` performs while structure settles (those nest
        // one level down, and the depth field keeps the frame total honest).
        byard_core::profile_scope!("interp.tick", byard_core::telemetry::ScopeKind::Interpreter);
        // Drained rather than taken: this releases the borrow on `self` so
        // the reconcile below can have it, and it leaves `dirty_structural`
        // holding its capacity for the next tick. `mem::take`, which clippy
        // suggests here, would move that capacity into a local and drop it,
        // making the hot tick reallocate every frame.
        #[allow(clippy::drain_collect)]
        let structural: Vec<ScopeId> = self.dirty_structural.drain(..).collect();
        for s in structural {
            self.reconcile_structural(s, epoch);
        }
        // Reconciliation may have enqueued freshly-mounted bindings; process the
        // queue in FIFO order (so `for` children project in list order) and keep
        // going as it grows, until the whole tick settles.
        let mut i = 0;
        while i < self.dirty_bindings.len() {
            let s = self.dirty_bindings[i];
            i += 1;
            // A scope retired after it was queued is skipped, and so is a stale
            // entry for a slot that has been reused since: the id's generation
            // no longer matches, so the new occupant is not run under it.
            if !self.is_live(s) {
                continue;
            }
            if self.scopes[s.index()].last_epoch == epoch {
                continue; // EPOCH GUARD (D1)
            }
            self.scopes[s.index()].last_epoch = epoch;
            let v = self.evaluate_scope(s);
            self.write_frame_field(s, v);
            self.scopes[s.index()].dirty = false;
        }
        self.dirty_bindings.clear();
    }

    fn write_frame_field(&mut self, s: ScopeId, v: Value) {
        let target = match &self.scopes[s.index()].kind {
            ScopeKind::ValueBinding { target, .. } => *target,
            _ => return,
        };
        let changed = match &self.scopes[s.index()].kind {
            ScopeKind::ValueBinding { last_written, .. } => last_written.as_ref() != Some(&v),
            _ => false,
        };
        if changed {
            // value-equality cut: unchanged projection writes nothing (§7).
            if let ScopeKind::ValueBinding { last_written, .. } = &mut self.scopes[s.index()].kind {
                *last_written = Some(v.clone());
            }
            self.seq += 1;
            self.scopes[s.index()].changed = self.seq;
            self.frame_writes.push((target, v));
        }
    }

    // ── scope evaluation with dynamic-dep clearing (§3) ─────────────────

    fn evaluate_scope(&mut self, s: ScopeId) -> Value {
        self.clear_deps(s);
        let mut compute = self.computes[s.index()]
            .take()
            .expect("scope has a computation (or a reactive cycle re-entered it)");
        let prev = CURRENT_SCOPE.replace(Some(s));
        let value = compute(self);
        CURRENT_SCOPE.set(prev);
        self.computes[s.index()] = Some(compute);
        self.scopes[s.index()].eval_count += 1;
        value
    }

    /// Removes `s` from each of its dependencies' subscriber lists, then empties
    /// its dep set (§3).
    fn clear_deps(&mut self, s: ScopeId) {
        let deps = std::mem::take(&mut self.scopes[s.index()].deps);
        for dep in &deps {
            match dep {
                Dep::Signal(sig) => {
                    self.signals[sig.0 as usize].subscribers.retain(|x| *x != s);
                }
                Dep::Memo(m) => {
                    if let ScopeKind::Memo { subs, .. } = &mut self.scopes[m.index()].kind {
                        subs.retain(|x| *x != s);
                    }
                }
            }
        }
    }

    // ── structural reconciliation (§8) ──────────────────────────────────

    fn reconcile_structural(&mut self, s: ScopeId, epoch: u32) {
        if !self.is_live(s) || self.scopes[s.index()].last_epoch == epoch {
            return;
        }
        self.scopes[s.index()].last_epoch = epoch;
        let kind = match &self.scopes[s.index()].kind {
            ScopeKind::Structural { kind } => *kind,
            _ => return,
        };
        let value = self.evaluate_scope(s);
        self.scopes[s.index()].dirty = false;

        let mut mount = self.mounts[s.index()]
            .take()
            .expect("structural scope has mount state");
        match (&mut mount, kind) {
            (
                Mount::When {
                    then,
                    els,
                    active,
                    children,
                },
                StructuralKind::When,
            ) => {
                let take = value.as_bool().unwrap_or(false);
                if *active != Some(take) {
                    for child in children.drain(..) {
                        self.drop_scope(child);
                    }
                    *active = Some(take);
                    *children = if take {
                        then(self)
                    } else if let Some(els) = els {
                        els(self)
                    } else {
                        Vec::new()
                    };
                }
            }
            (Mount::For { item, children }, StructuralKind::For) => {
                // Phase 2 coarse reconciliation (D7): drop all, rebuild.
                for child in children.drain(..) {
                    self.drop_scope(child);
                }
                let items = value.as_list().map(<[Value]>::to_vec).unwrap_or_default();
                for it in &items {
                    let mut mounted = item(self, it);
                    children.append(&mut mounted);
                }
            }
            _ => {}
        }
        self.mounts[s.index()] = Some(mount);
    }

    /// Retires a scope: clears its subscriptions (so no `Signal` or memo keeps a
    /// stale link), drops any structural children recursively, releases whatever
    /// value it had cached, and hands the slot back for the next allocation to
    /// reuse. The §4.2 grid entry removal is the eval driver's job; here we
    /// guarantee no leaked reactive subscription and no unbounded growth (§8).
    ///
    /// Idempotent, and safe to call with an id that is already retired: the
    /// generation tells a dead id from the live scope that took its slot.
    ///
    /// Everything that can point at a scope is undone here, because a slot that
    /// is reused while something still points at it fires the new occupant under
    /// the old one's name: the scopes it read (their `subs`), the scopes that read
    /// it (their `deps`, if it is a memo), and the signals it subscribed to. The
    /// dirty queues are not purged (a linear scan per retired scope); an entry
    /// left in them carries the old generation and is skipped when they drain.
    pub fn drop_scope(&mut self, s: ScopeId) {
        if !self.is_live(s) {
            return;
        }
        // Recursively drop structural children first.
        if let Some(Mount::When { children, .. } | Mount::For { children, .. }) =
            self.mounts[s.index()].as_mut()
        {
            let children = std::mem::take(children);
            for child in children {
                self.drop_scope(child);
            }
        }
        self.clear_deps(s);
        // If this scope was a memo, the scopes that read it still list it as a
        // dependency; take it off them, or their next `clear_deps` would reach
        // into whichever scope the slot holds by then.
        if let ScopeKind::Memo { subs, .. } = &mut self.scopes[s.index()].kind {
            let subs = std::mem::take(subs);
            for sub in subs {
                if let Some(reader) = self.scopes.get_mut(sub.index()) {
                    if reader.generation == sub.generation {
                        reader.deps.retain(|d| *d != Dep::Memo(s));
                    }
                }
            }
        }
        let scope = &mut self.scopes[s.index()];
        // Replaced, not just flagged dead: a memo's cache or a binding's last
        // value can be a whole list or record.
        scope.kind = ScopeKind::Memo {
            cache: Value::Unit,
            subs: SmallVec::new(),
            evaluating: false,
        };
        scope.deps = SmallVec::new();
        scope.dirty = false;
        scope.live = false;
        scope.generation = scope.generation.wrapping_add(1);
        self.computes[s.index()] = None;
        self.mounts[s.index()] = None;
        self.free_scopes.push(s.index);
    }

    // ── small helpers ───────────────────────────────────────────────────

    fn subscribe_signal(&mut self, sig: SignalId, s: ScopeId) {
        let subs = &mut self.signals[sig.0 as usize].subscribers;
        if !subs.contains(&s) {
            subs.push(s);
        }
    }

    fn subscribe_memo(&mut self, m: ScopeId, s: ScopeId) {
        if let ScopeKind::Memo { subs, .. } = &mut self.scopes[m.index()].kind {
            if !subs.contains(&s) {
                subs.push(s);
            }
        }
    }

    fn record_dep(&mut self, s: ScopeId, dep: Dep) {
        let deps = &mut self.scopes[s.index()].deps;
        if !deps.contains(&dep) {
            deps.push(dep);
        }
    }

    fn set_memo_evaluating(&mut self, m: ScopeId, v: bool) {
        if let ScopeKind::Memo { evaluating, .. } = &mut self.scopes[m.index()].kind {
            *evaluating = v;
        }
    }

    fn set_memo_cache(&mut self, m: ScopeId, v: Value) {
        if let ScopeKind::Memo { cache, .. } = &mut self.scopes[m.index()].kind {
            *cache = v;
        }
    }

    // ── escape hatch + metadata (§9, §10) ───────────────────────────────

    /// Whether a scope read at least one source on its last evaluation, the
    /// D3 "is this `let`/`fn` reactive?" signal (§10), recorded in a side-table,
    /// never on the AST.
    #[must_use]
    pub fn deps_nonempty(&self, s: ScopeId) -> bool {
        self.scope(s).is_some_and(|scope| !scope.deps.is_empty())
    }

    // ── observability (tests) ───────────────────────────────────────────

    /// Whether `s` is waiting to be recomputed: something it read was written
    /// since it last ran.
    #[must_use]
    pub fn is_dirty(&self, s: ScopeId) -> bool {
        self.scope(s).is_some_and(|scope| scope.dirty)
    }

    /// How many times `s` has been evaluated since creation.
    #[must_use]
    pub fn eval_count(&self, s: ScopeId) -> u32 {
        self.scope(s).map_or(0, |scope| scope.eval_count)
    }

    /// Number of frame-field writes performed since the last
    /// [`ReactiveCtx::begin_tick`] (an unchanged projection records none).
    #[must_use]
    pub fn frame_write_count(&self) -> usize {
        self.frame_writes.len()
    }

    /// Number of scopes currently subscribed to `sig` (leak check for §7/§8).
    #[must_use]
    pub fn signal_subscriber_count(&self, sig: SignalId) -> usize {
        self.signals[sig.0 as usize].subscribers.len()
    }

    /// The counter every write, memo evaluation and new projection moves
    /// (see [`ReadSet`]).
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Starts recording what is read into a fresh [`ReadSet`], until
    /// [`take_reads`](Self::take_reads). A multi-surface program does this
    /// around the drawing of each surface (RFC-0068 §5).
    pub fn watch_reads(&mut self) {
        *self.reads.get_mut() = ReadSet::default();
        self.watching.set(true);
    }

    /// Stops recording and returns what was read.
    pub fn take_reads(&mut self) -> ReadSet {
        self.watching.set(false);
        std::mem::take(self.reads.get_mut())
    }

    /// Records that the value binding or memo `s` was read, for a reader that
    /// cannot take `&mut self` (a binding's projected value is read through
    /// `&self`).
    pub fn note_scope_read(&self, s: ScopeId) {
        if self.watching.get() {
            ReadSet::set(&mut self.reads.borrow_mut().scopes, s.index());
        }
    }

    /// Whether anything in `set` has changed since `since` (a [`seq`](Self::seq)
    /// taken when it was read): a signal written, a memo evaluated or waiting to
    /// be, a binding that now projects something else.
    #[must_use]
    pub fn reads_changed_since(&self, set: &ReadSet, since: u64) -> bool {
        ReadSet::bits(&set.signals).any(|i| self.signals.get(i).is_some_and(|c| c.written > since))
            || ReadSet::bits(&set.scopes).any(|i| {
                self.scopes.get(i).is_some_and(|sc| {
                    sc.live
                        && (sc.changed > since
                            || (sc.dirty && matches!(sc.kind, ScopeKind::Memo { .. })))
                })
            })
    }

    /// The most recently projected value of a value binding (its `last_written`).
    #[must_use]
    pub fn binding_value(&self, s: ScopeId) -> Option<Value> {
        self.note_scope_read(s);
        match &self.scope(s)?.kind {
            ScopeKind::ValueBinding { last_written, .. } => last_written.clone(),
            _ => None,
        }
    }

    /// The frame-field writes performed since the last
    /// [`ReactiveCtx::begin_tick`].
    #[must_use]
    pub fn frame_writes(&self) -> &[(FrameTarget, Value)] {
        &self.frame_writes
    }

    /// Cumulative count of *effective* mark visits (each scope dirtied once),
    /// proving the cascade is idempotent (fixture 2).
    #[must_use]
    pub fn mark_effective_visits(&self) -> u32 {
        self.mark_effective
    }

    /// How many scope slots the arena has ever needed: its high-water mark. A
    /// retired slot is reused before this grows, so a screen pushed and popped
    /// repeatedly leaves it where the first push put it.
    #[must_use]
    pub fn scope_slot_count(&self) -> usize {
        self.scopes.len()
    }

    /// How many scopes are alive right now.
    #[must_use]
    pub fn live_scope_count(&self) -> usize {
        self.scopes.len() - self.free_scopes.len()
    }

    /// How many signals have been created. Signals are not recycled, so this
    /// only grows; it is here so a test can say what a feature costs in them.
    #[must_use]
    pub fn signal_count(&self) -> usize {
        self.signals.len()
    }
}

/// Evaluates `thunk` with read-tracking suspended (`CURRENT_SCOPE = None`), so
/// `read_signal`/`read_memo` inside install no subscription, then restores the
/// previous scope, correctly nested, even on unwind (§9 / D2).
pub fn untrack<R>(thunk: impl FnOnce() -> R) -> R {
    struct Restore(Option<ScopeId>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT_SCOPE.set(self.0);
        }
    }
    let _guard = Restore(CURRENT_SCOPE.replace(None));
    thunk()
}

#[cfg(test)]
mod tests;
