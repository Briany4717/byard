//! # Relay
//!
//! Thread management and the double-buffered frame swap (RFC-0001 §5.1, §5.2).
//!
//! [`Relay`] is the single point of contact between the **logic thread**
//! (Evaluator + Atlas, produces frames) and the **render thread** (Encoder,
//! consumes frames). It owns three things:
//!
//! - a lock-free publish/subscribe slot for the latest [`RenderFrame`],
//! - a bounded recycle pool so steady-state operation reuses `RenderFrame`
//!   heap allocations instead of reallocating every frame, and
//! - the Tokio runtime backing the async I/O pool (file loads, network,
//!   timers, anything that must not block either the logic or render
//!   thread), plus the two `tokio::sync::mpsc` channels that pool uses to
//!   hand completed results back (RFC-0001 §5.1, row 3).
//!
//! ## Why there are two result channels, not one
//!
//! The pool serves two different consumers, and they run on different
//! threads. A controller reply has to reach the **logic** thread, which is
//! the only place a `Signal` may be written (INV-2); a decoded image has to
//! reach the **render** thread, which is the only place a texture may be
//! uploaded. A single channel would mean each thread receiving the other's
//! traffic and having to put it back, which RFC-0028 §7 rejects on the
//! grounds that re-queueing invites livelock and ordering surprises between
//! two consumers. So the channel is split by destination: `io_result_*` is
//! drained by the logic thread, `decode_result_*` by the render thread, and
//! neither ever sees a message addressed to the other.
//!
//! ## Why each channel still carries `Box<dyn Any + Send>`
//!
//! Splitting by *thread* is not the same as naming a concrete payload type.
//! `byard-core` cannot name `ControllerReply`'s continuation semantics, or a
//! future capability's reply shape, without pinning `Relay` to one consumer's
//! vocabulary. Type erasure at the channel keeps `Relay` concrete for every
//! caller while still giving each consumer a working, tested delivery
//! mechanism: a sender sends `Box::new(value) as Box<dyn Any + Send>` and the
//! receiving thread downcasts on receipt.
//!
//! ## Why `arc-swap`, not hand-rolled `unsafe`
//!
//! An earlier draft of this module used a raw `AtomicPtr<RenderFrame>` with
//! manual `Box::into_raw`/`Box::from_raw`. `CONTRIBUTING.md`'s bar for new
//! `unsafe` is: *"could this be done in safe code without significant cost
//! or correctness loss?"* Here the answer is yes, [`arc_swap::ArcSwapOption`]
//! is a published, audited, lock-free swap primitive with the same
//! single-instruction-exchange performance characteristics, so introducing
//! a new `#![allow(unsafe_code)]` module would have bought nothing. The
//! issue's own task list asks for `Arc<RenderFrame>` specifically, which is
//! exactly what `ArcSwapOption<RenderFrame>` stores.
//!
//! ## Why `Relay` does not own its logic thread's `JoinHandle`
//!
//! The logic thread closure must hold a strong `Arc<Relay>` to call back
//! into `acquire_recycled`/`publish`/`is_shutdown`. If `Relay` also stored
//! its own `JoinHandle` for that same thread, dropping the *last* external
//! `Arc<Relay>` would never actually run `Relay`'s drop glue, the thread's
//! own clone keeps the refcount above zero, so a join-on-drop inside
//! `Relay` itself would either never fire or, worse, fire from inside the
//! thread it's trying to join (a deadlock). [`Relay::spawn_logic_thread`]
//! therefore returns the [`JoinHandle`] to the caller, exactly as
//! `std::thread::spawn` does. The owner of that handle (today: a test;
//! eventually: [`crate::engine::Engine`]) is responsible for calling
//! [`Relay::request_shutdown`] and then joining before dropping its own
//! `Arc<Relay>`. This mirrors the issue's literal acceptance criterion
//! ("dropping the engine joins all threads cleanly") at the layer that can
//! actually guarantee it.
//!
//! ## Engine integration
//!
//! [`crate::engine::Engine`] owns an `Arc<Relay>`, spawns the logic thread
//! in [`Engine::start_logic`], and reads the latest frame via
//! [`Relay::current`] in [`Engine::render_latest`]. `Engine`'s `Drop` impl
//! calls [`Relay::request_shutdown`] and joins the logic thread before
//! releasing the last `Arc<Relay>`.
//!
//! [`Engine::start_logic`] does not use [`Relay::spawn_logic_thread`]
//! directly. The tick state it needs to capture, a `ReactiveLabel`, holds
//! a [`crate::evaluator::Signal`], which is intentionally `!Send` per
//! RFC-0001 §5.1 (signals are never accessed outside the logic thread; the
//! compiler enforces this). A `FnMut + Send` closure therefore cannot
//! capture `ReactiveLabel`. Instead, `start_logic` spawns via
//! `std::thread::Builder` and constructs the `ReactiveLabel` inside the
//! thread body, following the same `acquire_recycled → tick → publish →
//! yield_now → is_shutdown` loop that [`Relay::spawn_logic_thread`] would
//! have supplied.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use arc_swap::ArcSwapOption;
use crossbeam_channel::{Receiver, Sender, bounded};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::ByardError;
use crate::InputEvent;
use crate::LogicRuntime;
use crate::evaluator::ViewArena;
use crate::frame::RenderFrame;

/// A type-erased result delivered from the async I/O pool back to the
/// **logic** thread, a controller reply or a timer tick (RFC-0028 §7). See
/// the module-level docs for why this is `Box<dyn Any + Send>` rather than a
/// generic parameter on [`Relay`].
pub type IoResult = Box<dyn Any + Send>;

/// A type-erased result delivered from the async I/O pool back to the
/// **render** thread, today an image decode (RFC-0028 §7). Same erasure, a
/// different destination, see [`IoResult`].
pub type DecodeResult = Box<dyn Any + Send>;

/// Capacity of the frame recycle pool.
///
/// Two is the minimum that allows one frame to be "in flight" with the
/// render thread while the logic thread recycles another, the literal
/// "double" in double-buffered. Raising this trades a little memory for
/// more slack when the render thread holds a frame longer than usual.
const RECYCLE_POOL_SIZE: usize = 2;

// Compile-time invariant, not a runtime check: `bounded(0)` would make the
// recycle channel permanently full from construction, so every
// `acquire_recycled` would allocate and `publish` could never recycle.
// Asserting this on the constant itself (rather than as a `#[test]`) is
// clippy's own suggested form (`clippy::assertions_on_constants`) and fails
// the build immediately if anyone ever sets this to 0.
const _: () = assert!(RECYCLE_POOL_SIZE > 0);

/// How long an *idle* logic tick parks before re-checking for work.
///
/// This is now only a **fallback** bound, not the delivery latency: an I/O
/// result or timer tick opens the [`IdleGate`] and the parked thread wakes
/// immediately (RFC-0029 §2). The timeout remains so a signal lost to a race
/// (a result queued in the instant between the emptiness check and the park)
/// can never hang the loop, and so a caller that hands the logic thread work
/// by some other means keeps its old ≤6 ms responsiveness.
const IDLE_PARK: std::time::Duration = std::time::Duration::from_millis(6);

/// The rendezvous between a sleeping logic thread and the async pool
/// (RFC-0029 §2, resolved question "idle parking vs latency").
///
/// Before this, an idle tick parked for a flat [`IDLE_PARK`], so a network
/// reply or a timer tick that landed just after the park was invisible for up
/// to 6 ms, an eternity to add to a result the pool already has in hand, and
/// worse, a latency the developer cannot see or attribute. A condvar makes
/// the wake edge-triggered instead: whoever queues work for the logic thread
/// opens the gate.
///
/// The flag is what makes it a *gate* rather than a bare condvar: a result
/// that arrives while the logic thread is still working (not yet parked)
/// leaves the flag set, so the following park returns at once instead of
/// sleeping through work that is already queued. Missing that is the classic
/// lost-wakeup bug, and it is exactly the case a busy tick produces.
#[derive(Default)]
struct IdleGate {
    /// Set when work has been queued and not yet observed by the logic thread.
    pending: Mutex<bool>,
    /// Signalled on every [`IdleGate::signal`].
    woken: std::sync::Condvar,
}

impl IdleGate {
    /// Records that work is queued and wakes a parked logic thread.
    fn signal(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        *pending = true;
        drop(pending);
        self.woken.notify_all();
    }

    /// Parks until [`signal`](Self::signal) is called or `timeout` elapses,
    /// clearing the pending flag. Returns immediately when work was queued
    /// while the caller was busy.
    fn park(&self, timeout: std::time::Duration) {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let mut pending = if *pending {
            pending
        } else {
            self.woken
                .wait_timeout(pending, timeout)
                .unwrap_or_else(PoisonError::into_inner)
                .0
        };
        *pending = false;
    }
}

/// The sender an async task uses to hand a completed result to the **logic**
/// thread (RFC-0028 §5 step 2, RFC-0029 §2).
///
/// A thin wrapper over the channel sender rather than the sender itself,
/// because delivering a result has two halves that must not be separable:
/// queueing the payload, and waking the thread that will apply it. A caller
/// that had the raw sender could do the first and forget the second, and the
/// symptom, a result that appears up to one park late, only under idle, is
/// the kind of latency bug that gets attributed to the network instead.
#[derive(Clone)]
pub struct IoSender {
    tx: UnboundedSender<IoResult>,
    idle: Arc<IdleGate>,
}

impl IoSender {
    /// Queues `result` for the logic thread and wakes it if it is parked.
    ///
    /// # Errors
    ///
    /// Returns the undelivered payload if the receiving `Relay` has been
    /// dropped (the app is shutting down), never panics.
    pub fn send(
        &self,
        result: IoResult,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<IoResult>> {
        self.tx.send(result)?;
        self.idle.signal();
        Ok(())
    }
}

/// Owns the atomic frame swap, the frame recycle pool, and the async I/O
/// runtime described in RFC-0001 §5.
///
/// `Relay` is cheap to share: wrap it in `Arc<Relay>` and clone the `Arc`
/// into both the logic-thread closure and the render-thread owner (e.g.
/// `Engine`). All methods take `&self` and never block the caller for
/// longer than an atomic load/store or a non-blocking channel operation.
pub struct Relay {
    latest: ArcSwapOption<RenderFrame>,
    recycle_tx: Sender<RenderFrame>,
    recycle_rx: Receiver<RenderFrame>,
    shutdown: AtomicBool,
    io_runtime: tokio::runtime::Runtime,
    io_result_tx: UnboundedSender<IoResult>,
    // `tokio::sync::mpsc::UnboundedReceiver` only allows a single consumer
    // and needs `&mut self` to poll, so it is not `Sync` on its own. The
    // `Mutex` exists purely to grant `&self` access for the very short
    // `try_recv` in `Relay::try_recv_io_result`, it is never held across
    // an `.await` and never contended in practice (one logic thread polls
    // it once per tick), so it does not reintroduce blocking in any
    // meaningful sense.
    io_result_rx: Mutex<UnboundedReceiver<IoResult>>,
    decode_result_tx: UnboundedSender<DecodeResult>,
    // The render thread's half of the split (RFC-0028 §7). Same single-consumer
    // `Mutex` rationale as `io_result_rx` above; the two are never locked
    // together, and each is polled by exactly one thread.
    decode_result_rx: Mutex<UnboundedReceiver<DecodeResult>>,
    input_tx: crossbeam_channel::Sender<InputEvent>,
    input_rx: crossbeam_channel::Receiver<InputEvent>,
    // Invoked by the logic thread after it publishes a frame that changed in
    // response to input, so a `Wait`-mode (event-driven) render loop knows to
    // wake and present it. `None` until a host installs one via
    // [`Relay::set_frame_waker`]; a continuously-redrawing (`Poll`) host can
    // leave it unset. Set rarely (once at startup), read once per published
    // frame, the mutex is never contended.
    frame_waker: Mutex<Option<FrameWaker>>,
    /// Monotonic count of frames published through [`Relay::publish`], stamped
    /// onto each frame as its [`RenderFrame::version`]. The encoder uses the
    /// gap between consecutive versions it *encodes* to detect that the render
    /// thread skipped a frame, and therefore possibly a dirty bit
    /// (RFC-0001 §5.2).
    /// The gate an idle logic tick parks on, and that a completed I/O result
    /// or timer tick opens (RFC-0029 §2). See [`IdleGate`].
    idle: Arc<IdleGate>,
    publish_seq: AtomicU64,
    /// The [`RenderFrame::version`] of the last frame the render thread
    /// actually encoded, or `0` before the first one.
    ///
    /// [`Relay::publish`] compares it against the version of the frame it is
    /// about to replace, to decide whether that frame still carries dirty bits
    /// nobody has acted on (RFC-0032 §R3 step 6).
    ///
    /// **A version and not a flag.** A flag answers "has *something* been
    /// rendered", and the two threads run free of each other: the render
    /// thread reads a frame, spends milliseconds encoding it, and marks
    /// afterwards, by which time the logic thread may have published one or
    /// more newer frames into the slot. A flag set at that moment claims those
    /// frames were drawn, and the next publish drops their dirty bits. The
    /// version names the frame the mark is *about*, so a mark that lands late
    /// can only ever clear the frame it belongs to.
    rendered_version: AtomicU64,
}

/// A host-installed callback the logic thread fires after publishing a changed
/// frame (see [`Relay::set_frame_waker`]). The platform layer points it at its
/// event loop's wake primitive (e.g. a winit `EventLoopProxy`) so an
/// event-driven render thread redraws exactly when there is something new to
/// show, no busy polling, no stale frame after an input.
pub type FrameWaker = Arc<dyn Fn() + Send + Sync>;

impl Relay {
    /// Creates a new `Relay` with an empty frame slot, a seeded recycle
    /// pool, and a freshly started multi-threaded Tokio runtime for async
    /// I/O.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::RuntimeCreation`] if the OS refuses to start
    /// the Tokio runtime's worker threads (e.g. thread-creation resource
    /// limits).
    #[must_use = "ignoring the returned Relay drops it immediately, shutting down its I/O runtime"]
    pub fn new() -> Result<Self, ByardError> {
        let (recycle_tx, recycle_rx) = bounded(RECYCLE_POOL_SIZE);
        for _ in 0..RECYCLE_POOL_SIZE {
            // Channel was just created with this exact capacity, cannot be full.
            let _ = recycle_tx.try_send(RenderFrame::new());
        }

        // RFC-0029 O1: the `runtime-io` feature turns on Tokio's `net` and
        // `time` drivers, which is what a socket or a `tokio::time::sleep`
        // needs to exist at all (without them either panics with "no reactor
        // running"). It is default-on because every capability RFC-0029 adds
        // depends on it; a headless or embedded build that wants none of them
        // drops the feature and gets the original compute-only runtime back,
        // with no `net`/`time` code compiled in.
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.thread_name("byard-io-worker");
        #[cfg(feature = "runtime-io")]
        builder.enable_all();
        let io_runtime = builder
            .build()
            .map_err(|e| ByardError::RuntimeCreation(e.to_string()))?;

        let (io_result_tx, io_result_rx) = mpsc::unbounded_channel();
        let (decode_result_tx, decode_result_rx) = mpsc::unbounded_channel();
        let (input_tx, input_rx) = crossbeam_channel::unbounded();

        Ok(Self {
            latest: ArcSwapOption::from(None),
            recycle_tx,
            recycle_rx,
            shutdown: AtomicBool::new(false),
            io_runtime,
            io_result_tx,
            io_result_rx: Mutex::new(io_result_rx),
            decode_result_tx,
            decode_result_rx: Mutex::new(decode_result_rx),
            input_tx,
            input_rx,
            frame_waker: Mutex::new(None),
            idle: Arc::new(IdleGate::default()),
            publish_seq: AtomicU64::new(0),
            rendered_version: AtomicU64::new(0),
        })
    }

    /// Pushes an input event into the logic queue.
    pub fn push_input(&self, event: InputEvent) {
        let _ = self.input_tx.send(event);
    }

    /// Installs the callback fired after the logic thread publishes a frame
    /// that changed in response to input. See [`FrameWaker`].
    pub fn set_frame_waker(&self, waker: FrameWaker) {
        *self
            .frame_waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(waker);
    }

    /// Fires the installed [`FrameWaker`], if any. The `Arc` is cloned out of
    /// the lock first so the (host-supplied) callback never runs while the
    /// mutex is held.
    fn wake_renderer(&self) {
        let waker = self
            .frame_waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    /// Returns a frame ready to be populated, preferring a recycled buffer
    /// over a fresh allocation.
    ///
    /// Never blocks: if the recycle pool is momentarily empty (the render
    /// thread is holding on to frames longer than usual), this allocates a
    /// new [`RenderFrame`] instead of waiting.
    #[must_use]
    pub fn acquire_recycled(&self) -> RenderFrame {
        let mut frame = self.recycle_rx.try_recv().unwrap_or_default();
        frame.clear();
        frame
    }

    /// Publishes `frame` as the new latest frame, atomically replacing
    /// whatever was previously visible to readers.
    ///
    /// This is the "single atomic pointer exchange" from RFC-0001 §5.2: the
    /// swap is one lock-free operation, so a concurrent [`Relay::current`]
    /// call always observes either the entire old frame or the entire new
    /// one, never a partial mix of both.
    ///
    /// If the previous frame is not referenced anywhere else (the render
    /// thread already dropped its clone, or never took one), its `Vec`
    /// allocation is returned to the recycle pool for reuse. If the pool is
    /// momentarily full, the frame is dropped normally, a missed recycle
    /// opportunity, not a correctness issue. This call never blocks.
    ///
    /// Also drains the calling thread's telemetry ring into `frame`
    /// (RFC-0013 "Hand-off") before the swap, so every publish path, the
    /// Phase-1 demo loop, [`Relay::spawn_logic_thread`], and
    /// [`Relay::spawn_logic_from_view`] alike, piggybacks CPU samples on
    /// this same atomic exchange with no per-call-site wiring.
    pub fn publish(&self, mut frame: RenderFrame) {
        // RFC-0030 §I1. This scope's own sample is written when the guard
        // drops, after `drain_telemetry` below, so it rides along with the
        // *next* tick's block rather than the one it timed. That one-tick lag
        // is inherent to measuring the hand-off from inside the hand-off, and
        // is preferable to the alternatives: hoisting the drain out of
        // `publish` would move telemetry wiring back to every call site
        // (RFC-0013 "Hand-off"), and timing only the swap would exclude the
        // drain, which is the part that can actually cost something.
        crate::profile_scope!("relay.publish");
        frame.drain_telemetry();
        // Stamp the publish sequence (RFC-0001 §5.2, generalised by RFC-0032).
        //
        // The relay is latest-wins: a slow render thread simply never sees
        // some published frames. That was harmless while every primitive was
        // emitted `dirty: true`, the encoder re-shaped everything on every
        // frame it *did* see. Now that the interpreter reports what actually
        // changed, a skipped frame is a lost dirty bit: the frame that carried
        // "this line's text changed" was dropped, and the next one truthfully
        // reports the line clean while the encoder's cached glyph buffer still
        // holds the string from two frames ago.
        //
        // Counting publishes here rather than asking each host to remember is
        // the point: this is the only place every published frame passes
        // through, so the counter cannot be forgotten by a new runtime.
        frame.set_version(
            self.publish_seq
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1),
        );
        // If the frame we are about to replace was never rendered, its dirty
        // bits describe changes nobody has drawn yet. Carry them forward
        // rather than dropping them, see `RenderFrame::merge_dirty_from` for
        // why the union is the correct operation and why the alternative
        // (detect the skip, redraw everything) gives the whole win back.
        if let Some(previous) = self.latest.load_full() {
            if previous.version() != self.rendered_version.load(Ordering::Relaxed) {
                frame.merge_dirty_from(&previous);
            }
        }
        let previous = self.latest.swap(Some(Arc::new(frame)));
        if let Some(arc) = previous {
            if let Ok(reclaimed) = Arc::try_unwrap(arc) {
                let _ = self.recycle_tx.try_send(reclaimed);
            }
            // else: a reader still holds a clone of the old Arc. It will be
            // deallocated normally once that reader drops it, we simply
            // don't get to recycle its buffer this time.
        }
    }

    /// Records that the frame with this [`version`](RenderFrame::version) has
    /// been rendered, so a later [`publish`](Self::publish) that replaces *that
    /// frame* may drop its dirty bits instead of carrying them forward.
    ///
    /// Called by the render thread **after** the encode succeeds, not when the
    /// frame is fetched: a frame read and then abandoned (a lost surface, an
    /// occluded window) has not been drawn, and its dirty bits still have to
    /// reach the screen eventually.
    ///
    /// The version is what makes the late arrival harmless. Encoding takes
    /// milliseconds and the logic thread does not wait, so by the time this is
    /// called the slot usually holds a newer frame; naming the frame that was
    /// drawn means this can never be read as a claim about that newer one.
    pub fn mark_rendered(&self, version: u64) {
        // Monotonic: an out-of-order mark (two render threads, a retried
        // encode) must not walk the watermark backwards and make a frame that
        // was drawn look undrawn.
        self.rendered_version.fetch_max(version, Ordering::Relaxed);
    }

    /// Returns a clone of the current latest frame, or `None` if nothing
    /// has been published yet.
    ///
    /// Non-blocking and may be called concurrently from any number of
    /// threads, including while the logic thread is mid-[`Relay::publish`]
    ///, this is exactly the "render thread never blocks" guarantee.
    #[must_use]
    pub fn current(&self) -> Option<Arc<RenderFrame>> {
        self.latest.load_full()
    }

    /// Returns a sender that lets a consumer (e.g. the render thread, after
    /// it finishes drawing a frame) voluntarily return a `RenderFrame` to
    /// the recycle pool.
    ///
    /// Using this is optional, frames returned only via [`Relay::publish`]
    /// already keep the pool healthy in the common case where the render
    /// thread doesn't hold on to old frames.
    #[must_use]
    pub fn recycler(&self) -> Sender<RenderFrame> {
        self.recycle_tx.clone()
    }

    /// Returns a handle to the async I/O Tokio runtime.
    ///
    /// The handle is cheap to clone and can be used to spawn tasks from any
    /// thread, including from inside the logic thread's tick closure.
    #[must_use]
    pub fn io_handle(&self) -> tokio::runtime::Handle {
        self.io_runtime.handle().clone()
    }

    /// Returns a cloneable sender that tasks spawned on [`Relay::io_handle`]
    /// use to deliver a completed result back to the **logic** thread.
    ///
    /// Per RFC-0001 §5.1: "\[the Tokio pool\] sends results back to the
    /// logic thread via `tokio::sync::mpsc`." The payload is boxed and
    /// type-erased (see the module-level docs); the receiving side downcasts
    /// via [`Relay::try_recv_io_result`]. Sending also wakes a parked logic
    /// thread (RFC-0029 §2), which is why this hands out an [`IoSender`]
    /// rather than the bare channel sender.
    #[must_use]
    pub fn io_result_sender(&self) -> IoSender {
        IoSender {
            tx: self.io_result_tx.clone(),
            idle: Arc::clone(&self.idle),
        }
    }

    /// Non-blocking poll for the next completed I/O result, if any.
    ///
    /// Intended to be called once per logic-thread tick. Never blocks:
    /// returns `None` immediately if no result has arrived yet, mirroring
    /// every other `Relay` accessor's "never blocks" guarantee.
    #[must_use]
    pub fn try_recv_io_result(&self) -> Option<IoResult> {
        self.io_result_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_recv()
            .ok()
    }

    /// Returns a cloneable sender for results addressed to the **render**
    /// thread, today an image decode (RFC-0028 §7).
    #[must_use]
    pub fn decode_result_sender(&self) -> UnboundedSender<DecodeResult> {
        self.decode_result_tx.clone()
    }

    /// Non-blocking poll for the next completed decode, if any. The render
    /// thread's counterpart to [`Relay::try_recv_io_result`].
    #[must_use]
    pub fn try_recv_decode_result(&self) -> Option<DecodeResult> {
        self.decode_result_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_recv()
            .ok()
    }

    /// Opens the idle gate, waking a parked logic thread now rather than at
    /// the end of its [`IDLE_PARK`] fallback (RFC-0029 §2).
    ///
    /// [`IoSender::send`] already does this for every delivered result; this
    /// is for the sources that hand the logic thread work without going
    /// through that channel, a timer driver arming its first tick, a host
    /// pushing a reload.
    pub fn wake_logic(&self) {
        self.idle.signal();
    }

    /// Returns `true` once [`Relay::request_shutdown`] has been called.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    /// Signals the logic thread (and any other cooperating loop) to stop.
    ///
    /// Idempotent, calling this more than once has no additional effect.
    /// Does not itself join any thread; pair with the [`JoinHandle`]
    /// returned by [`Relay::spawn_logic_thread`].
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    /// Spawns the logic thread: a loop that acquires a recycled frame, lets
    /// `tick` populate it, publishes it, and repeats until
    /// [`Relay::request_shutdown`] is called.
    ///
    /// `tick` is intentionally unpaced, it runs back-to-back with no
    /// sleep, because RFC-0001 does not yet specify a fixed tick rate.
    /// Callers that need pacing (vsync-driven redraw, a fixed-hz simulation
    /// step, etc.) should implement it inside `tick` itself, or wait on an
    /// external signal before returning. A future sub-issue may add a
    /// `Relay::run_at(hz, tick)` helper once that policy is decided.
    ///
    /// The caller owns the returned [`JoinHandle`], see the module-level
    /// docs for why `Relay` cannot safely join its own logic thread.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::ThreadSpawn`] if the OS refuses to create the
    /// thread.
    pub fn spawn_logic_thread<F>(
        relay: &Arc<Relay>,
        mut tick: F,
    ) -> Result<JoinHandle<()>, ByardError>
    where
        F: FnMut(&mut RenderFrame) + Send + 'static,
    {
        let relay = Arc::clone(relay);
        thread::Builder::new()
            .name("byard-logic-thread".to_string())
            .spawn(move || {
                while !relay.is_shutdown() {
                    let mut frame = relay.acquire_recycled();
                    tick(&mut frame);
                    relay.publish(frame);
                    thread::yield_now();
                }
            })
            .map_err(|e| ByardError::ThreadSpawn(e.to_string()))
    }

    /// Spawns the logic thread from a `build` factory that constructs a
    /// [`LogicRuntime`] **inside** the thread, then drives it
    /// `acquire_recycled → evaluate_tick → publish` until shutdown
    /// (RFC-0002 §"Integration with Engine", RFC-0003 §8).
    ///
    /// This is the generalization of [`Relay::spawn_logic_thread`] for a
    /// stateful interpreter: the running runtime holds `!Send` data
    /// (`Signal`s, a `ViewArena`, a logic-thread-local reactive scope), so it
    /// can never cross a thread boundary. Only the **factory** is bounded
    /// `Send + 'static` (INV-6), it closes over plain owned data (a
    /// `CompiledView`) and is moved into the thread, where it builds the arena
    /// and the borrowing runtime in place. The `for<'a>` HRTB ties the
    /// runtime's borrow to the thread-local arena's lifetime.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::ThreadSpawn`] if the OS refuses to create the
    /// thread.
    pub fn spawn_logic_from_view<F>(
        relay: &Arc<Relay>,
        build: F,
    ) -> Result<JoinHandle<()>, ByardError>
    where
        F: for<'a> FnOnce(&'a ViewArena) -> Box<dyn LogicRuntime + 'a> + Send + 'static,
    {
        let relay = Arc::clone(relay);
        thread::Builder::new()
            .name("byard-logic-thread".to_string())
            .spawn(move || {
                // The arena and the runtime that borrows it both live for the
                // thread body only; neither is ever observed off this thread.
                let arena = ViewArena::new();
                let mut runtime = build(&arena);
                while !relay.is_shutdown() {
                    let mut frame = relay.acquire_recycled();
                    // Tick step 0 (RFC-0028 §6): apply everything the async
                    // pool completed *before* input is processed and before
                    // the pull, so a reply and a same-tick input mark together
                    // and the frame reflects both coherently. A result that
                    // lands mid-frame simply waits for the next tick; it never
                    // arrives mid-pull, which is what makes the tick the
                    // consistency boundary D1 promises.
                    let mut results = Vec::new();
                    while let Some(result) = relay.try_recv_io_result() {
                        results.push(result);
                    }
                    let applied = !results.is_empty() && runtime.apply_io_results(results);
                    let mut inputs = Vec::new();
                    while let Ok(ev) = relay.input_rx.try_recv() {
                        inputs.push(ev);
                    }
                    // The reactive interpreter computes its own dirty set; the
                    // engine-level dirty-target plumbing is wired in a later
                    // phase, so pass an empty slice for now.
                    let had_input = !inputs.is_empty();
                    runtime.evaluate_tick(&mut frame, &inputs, &[]);
                    relay.publish(frame);

                    // Idle throttle: a UI with no pending input re-publishes an
                    // identical frame every iteration, so a tight `yield_now`
                    // spin would peg a core at 100% (heat → thermal throttling →
                    // sluggish input). When input *is* waiting we loop at full
                    // speed so bursts drain immediately; only an idle tick parks
                    // briefly, capping idle CPU while keeping first-input latency
                    // under one short park. (RFC-0001 leaves pacing to the caller.)
                    if had_input || applied {
                        // The frame just published reflects this input, or an
                        // async result that landed with no input at all. Either
                        // way, wake an event-driven (`Wait`-mode) render thread
                        // so it presents the update now rather than showing the
                        // stale frame until the next unrelated OS event.
                        //
                        // The `applied` half is RFC-0029 §2: without it a
                        // network reply or a timer tick would update state and
                        // then sit unseen, which reads as "the app ignored the
                        // response" and is indistinguishable from a bug in the
                        // request.
                        relay.wake_renderer();
                        thread::yield_now();
                    } else {
                        // Edge-triggered, with a timeout only as a lost-signal
                        // backstop (RFC-0029 §2): a result queued by the pool
                        // opens this gate immediately instead of waiting out
                        // the park.
                        relay.idle.park(IDLE_PARK);
                    }
                }
            })
            .map_err(|e| ByardError::ThreadSpawn(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Rect;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    /// Static assertion: `Relay` must be safely shareable across threads
    /// behind an `Arc`, mirroring the `assert_send_sync`-style checks
    /// already used elsewhere in this crate (see `frame.rs`'s `TargetId`
    /// tests).
    #[test]
    fn relay_is_send_and_sync() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Relay>();
    }

    #[test]
    fn new_relay_succeeds() {
        assert!(Relay::new().is_ok());
    }

    #[test]
    fn new_relay_has_no_latest_frame_initially() {
        let relay = Relay::new().unwrap();
        assert!(relay.current().is_none());
    }

    #[test]
    fn new_relay_is_not_shutdown_initially() {
        let relay = Relay::new().unwrap();
        assert!(!relay.is_shutdown());
    }

    #[test]
    fn acquire_recycled_returns_empty_frame() {
        let relay = Relay::new().unwrap();
        let frame = relay.acquire_recycled();
        assert!(frame.rects().is_empty());
    }

    #[test]
    fn current_returns_none_before_any_publish() {
        let relay = Relay::new().unwrap();
        assert!(relay.current().is_none());
    }

    #[test]
    fn publish_then_current_returns_published_content() {
        let relay = Relay::new().unwrap();
        let mut frame = relay.acquire_recycled();
        frame.push_rect(Rect::new(1.0, 2.0, 3.0, 4.0), false);
        relay.publish(frame);

        let observed = relay.current().expect("frame was published");
        assert_eq!(observed.rects().len(), 1);
        assert_eq!(observed.rects()[0], Rect::new(1.0, 2.0, 3.0, 4.0));
    }

    #[test]
    fn publish_drains_the_calling_threads_telemetry_ring_into_the_frame() {
        // RFC-0013 "Hand-off": Relay::publish is the single choke point every
        // logic-thread loop passes through, so profiling a scope right before
        // publishing must show up on the published frame with no extra
        // wiring at the call site.
        let _ = crate::telemetry::drain_samples(); // isolate from other tests
        let relay = Relay::new().unwrap();
        let frame = relay.acquire_recycled();
        {
            crate::profile_scope!("relay.test.publish_drains_telemetry");
        }
        relay.publish(frame);

        let observed = relay.current().expect("frame was published");
        #[cfg(feature = "telemetry")]
        assert_eq!(
            observed.telemetry().samples.len(),
            1,
            "the scope profiled before publish must ride the same frame"
        );
        #[cfg(not(feature = "telemetry"))]
        assert!(observed.telemetry().samples.is_empty());
    }

    /// A text line the producer reported as changed.
    fn dirty_line(text: &str) -> crate::frame::TextLine {
        crate::frame::TextLine {
            x: 0.0,
            y: 0.0,
            text: text.to_string(),
            font_size: 12.0,
            weight: 400,
            color: [1.0; 4],
            dirty: true,
        }
    }

    /// The same line, reported unchanged.
    fn clean_line(text: &str) -> crate::frame::TextLine {
        crate::frame::TextLine {
            dirty: false,
            ..dirty_line(text)
        }
    }

    #[test]
    fn multiple_publishes_overwrite_rather_than_merge() {
        let relay = Relay::new().unwrap();

        let mut a = relay.acquire_recycled();
        a.push_rect(Rect::new(0.0, 0.0, 1.0, 1.0), false);
        relay.publish(a);

        let mut b = relay.acquire_recycled();
        b.push_rect(Rect::new(9.0, 9.0, 9.0, 9.0), false);
        b.push_rect(Rect::new(8.0, 8.0, 8.0, 8.0), false);
        relay.publish(b);

        let observed = relay.current().unwrap();
        assert_eq!(observed.rects().len(), 2);
        assert_eq!(observed.rects()[0], Rect::new(9.0, 9.0, 9.0, 9.0));
    }

    /// The dirty bits of a frame nobody drew are carried into its replacement,
    /// so a logic thread that outruns the display loses nothing (RFC-0032 §R3
    /// step 6).
    #[test]
    fn an_unrendered_frames_dirty_bits_survive_into_its_replacement() {
        let relay = Relay::new().unwrap();
        let mut a = relay.acquire_recycled();
        a.push_text(dirty_line("changed"));
        relay.publish(a);

        // Nobody rendered it. The next frame reports the line clean, because
        // the producer compares against what it last published, not against
        // what was last drawn.
        let mut b = relay.acquire_recycled();
        b.push_text(clean_line("changed"));
        relay.publish(b);

        assert!(
            relay.current().unwrap().texts()[0].dirty,
            "the change nobody drew is still owed"
        );
    }

    /// The mark names the frame it is about, so a mark that lands after newer
    /// frames have been published cannot clear *their* dirty bits.
    ///
    /// This is the real interleaving, not a contrived one: encoding takes
    /// milliseconds, the logic thread does not wait for it, and the render
    /// thread marks when it finishes. A flag set at that moment says "the frame
    /// in the slot has been drawn" about a frame it never saw.
    #[test]
    fn a_late_mark_cannot_claim_a_frame_it_never_saw() {
        let relay = Relay::new().unwrap();

        let mut drawn = relay.acquire_recycled();
        drawn.push_text(clean_line("steady"));
        relay.publish(drawn);
        let encoding = relay.current().unwrap();
        let encoding_version = encoding.version();

        // Mid-encode, the logic thread publishes a frame carrying a change.
        let mut changed = relay.acquire_recycled();
        changed.push_text(dirty_line("edited"));
        relay.publish(changed);

        // The encode finishes now, and marks the frame it actually drew.
        relay.mark_rendered(encoding_version);

        // The next frame is clean at the producer, as before.
        let mut after = relay.acquire_recycled();
        after.push_text(clean_line("edited"));
        relay.publish(after);

        assert!(
            relay.current().unwrap().texts()[0].dirty,
            "the edit was never drawn, so it is still owed"
        );
    }

    /// And the other direction: a frame that *was* drawn does not keep
    /// re-reporting itself, which would hand back the whole incremental path.
    #[test]
    fn a_rendered_frames_dirty_bits_are_not_carried_forward() {
        let relay = Relay::new().unwrap();
        let mut a = relay.acquire_recycled();
        a.push_text(dirty_line("changed"));
        relay.publish(a);
        relay.mark_rendered(relay.current().unwrap().version());

        let mut b = relay.acquire_recycled();
        b.push_text(clean_line("changed"));
        relay.publish(b);

        assert!(
            !relay.current().unwrap().texts()[0].dirty,
            "a drawn change is settled"
        );
    }

    #[test]
    fn current_can_be_called_repeatedly_without_consuming() {
        let relay = Relay::new().unwrap();
        let mut frame = relay.acquire_recycled();
        frame.push_rect(Rect::new(1.0, 1.0, 1.0, 1.0), false);
        relay.publish(frame);

        let first = relay.current().unwrap();
        let second = relay.current().unwrap();
        assert_eq!(first.rects(), second.rects());
    }

    #[test]
    fn holding_an_old_arc_keeps_its_content_unchanged_across_later_publishes() {
        let relay = Relay::new().unwrap();

        let mut a = relay.acquire_recycled();
        a.push_rect(Rect::new(1.0, 1.0, 1.0, 1.0), false);
        relay.publish(a);
        let held = relay.current().unwrap(); // render thread "holds" this Arc

        let mut b = relay.acquire_recycled();
        b.push_rect(Rect::new(2.0, 2.0, 2.0, 2.0), false);
        relay.publish(b);

        // The Arc the test is still holding must be unaffected by the swap.
        assert_eq!(held.rects()[0], Rect::new(1.0, 1.0, 1.0, 1.0));
        // But a fresh read sees the new frame.
        assert_eq!(
            relay.current().unwrap().rects()[0],
            Rect::new(2.0, 2.0, 2.0, 2.0)
        );
    }

    #[test]
    fn acquired_recycled_frame_is_always_empty_even_if_reused_buffer_had_content() {
        let relay = Relay::new().unwrap();

        // Publish a frame with content, then publish a second one so the
        // first (uncloned) Arc is reclaimed into the recycle pool.
        let mut a = relay.acquire_recycled();
        a.push_rect(Rect::new(5.0, 5.0, 5.0, 5.0), false);
        relay.publish(a);
        let b = relay.acquire_recycled();
        relay.publish(b);

        // Drain the pool looking for a previously-used buffer; every frame
        // handed back by acquire_recycled must be empty regardless of what
        // it held before.
        for _ in 0..RECYCLE_POOL_SIZE {
            let frame = relay.acquire_recycled();
            assert!(frame.rects().is_empty());
        }
    }

    #[test]
    fn acquire_recycled_falls_back_to_allocation_when_pool_is_empty() {
        let relay = Relay::new().unwrap();

        // Drain the pool completely without returning anything.
        for _ in 0..RECYCLE_POOL_SIZE {
            let _ = relay.acquire_recycled();
        }

        // One more acquire must still succeed (falls back to a fresh
        // allocation) rather than panicking or blocking.
        let frame = relay.acquire_recycled();
        assert!(frame.rects().is_empty());
    }

    #[test]
    fn publish_does_not_block_when_recycle_pool_is_already_full() {
        let relay = Relay::new().unwrap();

        // Saturate the pool directly (test is in the same module, so it can
        // see the private fields).
        for _ in 0..8 {
            let _ = relay.recycle_tx.try_send(RenderFrame::new());
        }

        let frame = relay.acquire_recycled();
        let start = Instant::now();
        relay.publish(frame); // must use try_send internally, never block
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn recycler_handle_can_manually_return_a_frame() {
        let relay = Relay::new().unwrap();
        // `Relay::new()` seeds the bounded(RECYCLE_POOL_SIZE) channel to full
        // capacity, so a slot must be drained before a manual return has
        // room to land.
        let _ = relay.acquire_recycled();
        let recycler = relay.recycler();

        let mut frame = RenderFrame::new();
        frame.push_rect(Rect::new(1.0, 1.0, 1.0, 1.0), false);
        frame.clear();
        assert!(recycler.try_send(frame).is_ok());
    }

    #[test]
    fn request_shutdown_is_idempotent() {
        let relay = Relay::new().unwrap();
        relay.request_shutdown();
        relay.request_shutdown();
        assert!(relay.is_shutdown());
    }

    #[test]
    fn io_handle_can_run_a_future_to_completion() {
        let relay = Relay::new().unwrap();
        let result = relay.io_handle().block_on(async { 21 + 21 });
        assert_eq!(result, 42);
    }

    #[test]
    fn try_recv_io_result_returns_none_when_empty() {
        let relay = Relay::new().unwrap();
        assert!(relay.try_recv_io_result().is_none());
    }

    #[test]
    fn io_result_sent_then_received_round_trips_through_downcast() {
        let relay = Relay::new().unwrap();
        let tx = relay.io_result_sender();

        tx.send(Box::new(42_i32)).unwrap();

        let result = relay
            .try_recv_io_result()
            .expect("a result was sent and should be receivable");
        let value = result
            .downcast::<i32>()
            .expect("payload was sent as i32, should downcast back to i32");
        assert_eq!(*value, 42);
    }

    #[test]
    fn io_result_downcast_to_wrong_type_fails_without_panicking() {
        let relay = Relay::new().unwrap();
        let tx = relay.io_result_sender();

        tx.send(Box::new(42_i32)).unwrap();

        let result = relay.try_recv_io_result().unwrap();
        let failed = result.downcast::<String>();
        assert!(
            failed.is_err(),
            "downcasting to the wrong type must fail, not panic"
        );
    }

    #[test]
    fn multiple_io_results_are_received_in_fifo_order() {
        let relay = Relay::new().unwrap();
        let tx = relay.io_result_sender();

        tx.send(Box::new(1_i32)).unwrap();
        tx.send(Box::new(2_i32)).unwrap();
        tx.send(Box::new(3_i32)).unwrap();

        let mut observed = Vec::new();
        while let Some(result) = relay.try_recv_io_result() {
            observed.push(*result.downcast::<i32>().unwrap());
        }
        assert_eq!(observed, vec![1, 2, 3]);
    }

    #[test]
    fn io_result_sender_is_cloneable_and_both_clones_deliver_to_the_same_receiver() {
        let relay = Relay::new().unwrap();
        let tx_a = relay.io_result_sender();
        let tx_b = tx_a.clone();

        tx_a.send(Box::new("from-a".to_string())).unwrap();
        tx_b.send(Box::new("from-b".to_string())).unwrap();

        let first = *relay
            .try_recv_io_result()
            .unwrap()
            .downcast::<String>()
            .unwrap();
        let second = *relay
            .try_recv_io_result()
            .unwrap()
            .downcast::<String>()
            .unwrap();
        assert_eq!(first, "from-a");
        assert_eq!(second, "from-b");
    }

    #[test]
    fn io_result_sent_from_a_spawned_async_task_is_received_after_it_completes() {
        let relay = Relay::new().unwrap();
        let tx = relay.io_result_sender();

        let task = relay.io_handle().spawn(async move {
            tx.send(Box::new(99_i32)).unwrap();
        });
        relay.io_handle().block_on(task).unwrap();

        let result = relay
            .try_recv_io_result()
            .expect("spawned task should have sent a result");
        assert_eq!(*result.downcast::<i32>().unwrap(), 99);
    }

    #[test]
    fn real_image_decode_on_the_io_pool_is_received_after_it_completes() {
        // The M29 shape end-to-end at the relay level: a deliberately slow
        // (sleep + real decode) task on the I/O pool reports its result back
        // through the type-erased channel, exactly as `TextureCache::ensure`
        // does, proving a blocking `image` decode never touches the caller.
        use std::io::Cursor;

        let relay = Relay::new().unwrap();
        let tx = relay.io_result_sender();

        // A tiny PNG, encoded in-memory so the test needs no fixture file.
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        let task = relay.io_handle().spawn(async move {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
            let dims = (decoded.width(), decoded.height());
            tx.send(Box::new(dims)).unwrap();
        });
        relay.io_handle().block_on(task).unwrap();

        let result = relay
            .try_recv_io_result()
            .expect("decode task should have sent its result");
        assert_eq!(*result.downcast::<(u32, u32)>().unwrap(), (4, 4));
    }

    #[test]
    fn dropping_relay_with_unconsumed_io_results_does_not_panic() {
        let relay = Relay::new().unwrap();
        let tx = relay.io_result_sender();
        tx.send(Box::new(1_i32)).unwrap();
        drop(relay); // must not panic even with an undrained result queued
    }

    #[test]
    fn io_result_sender_outliving_the_relay_does_not_panic_on_send() {
        let relay = Relay::new().unwrap();
        let tx = relay.io_result_sender();
        drop(relay);
        // The receiver is gone now; sending into a closed channel must
        // return an error, not panic.
        assert!(tx.send(Box::new(1_i32)).is_err());
    }

    #[test]
    fn spawn_logic_thread_runs_tick_at_least_once() {
        let relay = Arc::new(Relay::new().unwrap());
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let handle = Relay::spawn_logic_thread(&relay, move |_frame| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        })
        .expect("thread spawn should succeed in tests");

        // Give the thread a moment to run, then ask it to stop.
        thread::sleep(Duration::from_millis(20));
        relay.request_shutdown();
        handle.join().expect("logic thread must not panic");

        assert!(counter.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn spawn_logic_thread_uses_the_documented_thread_name() {
        let relay = Arc::new(Relay::new().unwrap());
        let (name_tx, name_rx) = bounded(1);

        let handle = Relay::spawn_logic_thread(&relay, move |_frame| {
            let _ = name_tx.try_send(thread::current().name().unwrap_or_default().to_string());
        })
        .unwrap();

        let name = name_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("tick should run and report a thread name");
        relay.request_shutdown();
        handle.join().unwrap();

        assert_eq!(name, "byard-logic-thread");
    }

    #[test]
    fn shutdown_then_join_pattern_completes_without_hanging() {
        let relay = Arc::new(Relay::new().unwrap());
        let handle = Relay::spawn_logic_thread(&relay, |_frame| {}).unwrap();

        relay.request_shutdown();
        // If this hangs, the test runner's own timeout will catch it, that
        // is the acceptable failure signal for "does not join cleanly".
        handle
            .join()
            .expect("logic thread must join after shutdown");
    }

    #[test]
    fn render_thread_never_blocks_while_logic_thread_publishes_continuously() {
        let relay = Arc::new(Relay::new().unwrap());
        let handle = Relay::spawn_logic_thread(&relay, |frame| {
            frame.push_rect(Rect::new(0.0, 0.0, 1.0, 1.0), false);
        })
        .unwrap();

        // Hammer `current()` from the "render thread" (this thread) while
        // the logic thread is publishing as fast as it can. The assertion
        // is on the *total* wall time for many calls, not on any single
        // call, a per-iteration millisecond bound is flaky under
        // scheduler jitter; a generous aggregate bound is not.
        let start = Instant::now();
        for _ in 0..2000 {
            let _ = relay.current();
        }
        let elapsed = start.elapsed();

        relay.request_shutdown();
        handle.join().unwrap();

        assert!(
            elapsed < Duration::from_secs(2),
            "2000 non-blocking reads took {elapsed:?}, render thread appears to be blocking"
        );
    }

    #[test]
    fn frame_content_is_never_torn_under_concurrent_publish_and_read() {
        let relay = Arc::new(Relay::new().unwrap());
        let generation = Arc::new(AtomicUsize::new(0));
        let generation_clone = Arc::clone(&generation);

        // Each published frame encodes a single monotonic "generation"
        // value into every rect it contains. If the swap were ever
        // non-atomic, a reader could observe a frame built from two
        // different generations, this test asserts that never happens.
        let handle = Relay::spawn_logic_thread(&relay, move |frame| {
            let generation_value = generation_clone.fetch_add(1, Ordering::SeqCst);
            #[allow(clippy::cast_precision_loss)]
            let generation_f = generation_value as f32;
            for _ in 0..4 {
                frame.push_rect(Rect::new(generation_f, generation_f, 1.0, 1.0), false);
            }
        })
        .unwrap();

        for _ in 0..500 {
            if let Some(observed) = relay.current() {
                let rects = observed.rects();
                if let Some(first) = rects.first() {
                    #[allow(clippy::float_cmp)]
                    let consistent = rects.iter().all(|r| r.x == first.x);
                    assert!(consistent, "observed a torn frame: {rects:?}");
                }
            }
        }

        relay.request_shutdown();
        handle.join().unwrap();
    }

    #[test]
    fn current_never_returns_none_once_something_has_been_published() {
        let relay = Arc::new(Relay::new().unwrap());
        let mut seed = relay.acquire_recycled();
        seed.push_rect(Rect::new(0.0, 0.0, 1.0, 1.0), false);
        relay.publish(seed);

        let mut readers = Vec::new();
        for _ in 0..8 {
            let relay = Arc::clone(&relay);
            readers.push(thread::spawn(move || {
                for _ in 0..100 {
                    assert!(relay.current().is_some());
                }
            }));
        }
        for reader in readers {
            reader.join().unwrap();
        }
    }

    #[test]
    fn stress_many_publish_acquire_cycles_without_panicking() {
        let relay = Relay::new().unwrap();
        for i in 0..10_000 {
            let mut frame = relay.acquire_recycled();
            #[allow(clippy::cast_precision_loss)]
            frame.push_rect(Rect::new(i as f32, 0.0, 1.0, 1.0), false);
            relay.publish(frame);
        }
        assert_eq!(relay.current().unwrap().rects().len(), 1);
    }

    #[test]
    fn dropping_relay_with_unconsumed_latest_frame_does_not_panic() {
        let relay = Relay::new().unwrap();
        let mut frame = relay.acquire_recycled();
        frame.push_rect(Rect::new(1.0, 1.0, 1.0, 1.0), false);
        relay.publish(frame);
        drop(relay); // must not panic
    }

    #[test]
    fn dropping_relay_after_clean_shutdown_and_join_does_not_panic() {
        let relay = Arc::new(Relay::new().unwrap());
        let handle = Relay::spawn_logic_thread(&relay, |_frame| {}).unwrap();
        relay.request_shutdown();
        handle.join().unwrap();
        drop(relay); // last Arc, runs Relay's drop glue, must not panic
    }

    #[test]
    fn two_relays_are_fully_independent() {
        let a = Relay::new().unwrap();
        let b = Relay::new().unwrap();

        let mut fa = a.acquire_recycled();
        fa.push_rect(Rect::new(1.0, 1.0, 1.0, 1.0), false);
        a.publish(fa);

        assert!(a.current().is_some());
        assert!(b.current().is_none());
    }

    // ── spawn_logic_from_view: the !Send-runtime / Send-factory contract ──

    /// A minimal `!Send` `LogicRuntime`: it holds a [`Signal`] (which is `!Send`
    /// by construction, RFC-0001 §5.1), proving a stateful interpreter can run
    /// on the logic thread without ever crossing a thread boundary.
    struct CounterRuntime<'a> {
        signal: crate::evaluator::Signal<'a, i64>,
        hits: Arc<AtomicUsize>,
    }

    impl LogicRuntime for CounterRuntime<'_> {
        fn evaluate_tick(
            &mut self,
            frame: &mut RenderFrame,
            _input_events: &[InputEvent],
            _dirty: &[crate::frame::TargetId],
        ) {
            self.signal.write(|v| *v += 1);
            #[allow(clippy::cast_sign_loss)]
            frame.set_version(self.signal.read(|v| *v as u64));
            self.hits.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn spawn_logic_from_view_builds_a_non_send_runtime_and_ticks() {
        let relay = Arc::new(Relay::new().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_factory = Arc::clone(&hits);

        // The factory is `Send` (it captures only an `Arc<AtomicUsize>`); the
        // runtime it builds borrows the thread-local arena and is `!Send`.
        let handle = Relay::spawn_logic_from_view(&relay, move |arena| {
            Box::new(CounterRuntime {
                signal: crate::evaluator::Signal::new_in(arena, 0_i64),
                hits: hits_factory,
            })
        })
        .expect("thread spawn should succeed in tests");

        // Wait until at least one tick has run.
        let start = Instant::now();
        while hits.load(Ordering::SeqCst) == 0 && start.elapsed() < Duration::from_secs(2) {
            thread::yield_now();
        }
        relay.request_shutdown();
        handle.join().expect("logic thread must not panic");

        assert!(
            hits.load(Ordering::SeqCst) >= 1,
            "the !Send runtime must have ticked at least once"
        );
        assert!(
            relay.current().is_some(),
            "ticking must have published at least one frame"
        );
    }

    // ── RFC-0028 §7: two channels, one per destination thread ────────────

    #[test]
    fn a_decode_result_never_reaches_the_logic_threads_drain() {
        // The whole point of the split: the render thread's traffic must be
        // invisible to the logic thread, so neither has to recognise and
        // re-queue the other's messages.
        let relay = Relay::new().unwrap();
        relay
            .decode_result_sender()
            .send(Box::new(7_i32))
            .expect("decode channel accepts a result");

        assert!(
            relay.try_recv_io_result().is_none(),
            "the logic drain must not see a decode result"
        );
        assert_eq!(
            *relay
                .try_recv_decode_result()
                .expect("the render drain sees it")
                .downcast::<i32>()
                .unwrap(),
            7
        );
    }

    #[test]
    fn a_controller_reply_never_reaches_the_render_threads_drain() {
        let relay = Relay::new().unwrap();
        relay.io_result_sender().send(Box::new(9_i32)).unwrap();

        assert!(
            relay.try_recv_decode_result().is_none(),
            "the render drain must not see a controller reply"
        );
        assert!(relay.try_recv_io_result().is_some());
    }

    // ── RFC-0029 O1/§2: the runtime can do I/O, and idle wakes on it ─────

    #[test]
    #[cfg(feature = "runtime-io")]
    fn the_io_pool_can_sleep_on_the_time_driver() {
        // Without `.enable_all()` this panics with "there is no reactor
        // running", which is exactly the state RFC-0029 O1 found the runtime
        // in. It is the single precondition for every capability that follows.
        let relay = Relay::new().unwrap();
        let elapsed = relay.io_handle().block_on(async {
            let start = Instant::now();
            tokio::time::sleep(Duration::from_millis(5)).await;
            start.elapsed()
        });
        assert!(
            elapsed >= Duration::from_millis(4),
            "the timer actually slept"
        );
    }

    #[test]
    fn a_result_queued_while_the_logic_thread_was_busy_does_not_sleep_through_the_park() {
        // The lost-wakeup case: work arrives before the thread parks. A bare
        // condvar would sleep for the full fallback; the gate's pending flag
        // means the park returns at once.
        let gate = IdleGate::default();
        gate.signal();
        let start = Instant::now();
        gate.park(Duration::from_secs(30));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a pre-signalled gate must not park"
        );
    }

    #[test]
    fn parking_ends_the_moment_a_result_is_sent() {
        let relay = Arc::new(Relay::new().unwrap());
        let sender = relay.io_result_sender();
        let park_relay = Arc::clone(&relay);

        let parked = thread::spawn(move || {
            let start = Instant::now();
            // A 30 s fallback: if the wake did not work this test fails on
            // duration rather than hanging the suite.
            park_relay.idle.park(Duration::from_secs(30));
            start.elapsed()
        });

        thread::sleep(Duration::from_millis(20));
        sender.send(Box::new(1_i32)).unwrap();

        let waited = parked.join().expect("the parked thread must not panic");
        assert!(
            waited < Duration::from_secs(5),
            "sending a result must wake the parked logic thread, waited {waited:?}"
        );
    }

    /// A runtime that records what it was handed at tick step 0 and reports
    /// whether it changed anything, the two halves of the `LogicRuntime`
    /// contract this phase adds.
    struct DrainingRuntime {
        applied: Arc<AtomicUsize>,
        changed: bool,
    }

    impl LogicRuntime for DrainingRuntime {
        fn evaluate_tick(
            &mut self,
            _frame: &mut RenderFrame,
            _input_events: &[InputEvent],
            _dirty: &[crate::frame::TargetId],
        ) {
        }

        fn apply_io_results(&mut self, results: Vec<crate::relay::IoResult>) -> bool {
            self.applied.fetch_add(results.len(), Ordering::SeqCst);
            self.changed
        }
    }

    #[test]
    fn the_logic_loop_drains_io_results_into_the_runtime() {
        // The gap RFC-0028 §5 step 3 names: the channel had a sender and a
        // receiver and nothing on the logic side ever called it.
        let relay = Arc::new(Relay::new().unwrap());
        let applied = Arc::new(AtomicUsize::new(0));
        let applied_rt = Arc::clone(&applied);

        let handle = Relay::spawn_logic_from_view(&relay, move |_arena| {
            Box::new(DrainingRuntime {
                applied: applied_rt,
                changed: true,
            })
        })
        .unwrap();

        relay.io_result_sender().send(Box::new(1_i32)).unwrap();

        let start = Instant::now();
        while applied.load(Ordering::SeqCst) == 0 && start.elapsed() < Duration::from_secs(2) {
            thread::yield_now();
        }
        relay.request_shutdown();
        relay.wake_logic();
        handle.join().unwrap();

        assert_eq!(applied.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_applied_result_wakes_a_wait_mode_render_loop_with_no_input_at_all() {
        // RFC-0029 §2: without this, a network reply or a timer tick updates
        // state and then sits unseen until some unrelated OS event repaints.
        let relay = Arc::new(Relay::new().unwrap());
        let woke = Arc::new(AtomicUsize::new(0));
        let woke_cb = Arc::clone(&woke);
        relay.set_frame_waker(Arc::new(move || {
            woke_cb.fetch_add(1, Ordering::SeqCst);
        }));

        let handle = Relay::spawn_logic_from_view(&relay, |_arena| {
            Box::new(DrainingRuntime {
                applied: Arc::new(AtomicUsize::new(0)),
                changed: true,
            })
        })
        .unwrap();

        relay.io_result_sender().send(Box::new(1_i32)).unwrap();

        let start = Instant::now();
        while woke.load(Ordering::SeqCst) == 0 && start.elapsed() < Duration::from_secs(2) {
            thread::yield_now();
        }
        relay.request_shutdown();
        relay.wake_logic();
        handle.join().unwrap();

        assert!(
            woke.load(Ordering::SeqCst) >= 1,
            "an I/O-applied tick must wake the renderer"
        );
    }

    #[test]
    fn a_result_that_changed_nothing_does_not_cost_a_repaint() {
        // The other direction of the same rule: `apply_io_results` reporting
        // `false` means no `var` moved, so there is nothing new to present.
        let relay = Arc::new(Relay::new().unwrap());
        let woke = Arc::new(AtomicUsize::new(0));
        let woke_cb = Arc::clone(&woke);
        relay.set_frame_waker(Arc::new(move || {
            woke_cb.fetch_add(1, Ordering::SeqCst);
        }));
        let applied = Arc::new(AtomicUsize::new(0));
        let applied_rt = Arc::clone(&applied);

        let handle = Relay::spawn_logic_from_view(&relay, move |_arena| {
            Box::new(DrainingRuntime {
                applied: applied_rt,
                changed: false,
            })
        })
        .unwrap();

        relay.io_result_sender().send(Box::new(1_i32)).unwrap();
        let start = Instant::now();
        while applied.load(Ordering::SeqCst) == 0 && start.elapsed() < Duration::from_secs(2) {
            thread::yield_now();
        }
        relay.request_shutdown();
        relay.wake_logic();
        handle.join().unwrap();

        assert_eq!(
            woke.load(Ordering::SeqCst),
            0,
            "a reply that wrote nothing must not wake the renderer"
        );
    }

    #[test]
    fn frame_waker_fires_after_an_input_bearing_tick() {
        // The wake-on-publish contract (Wait-mode redraw): after the logic
        // thread processes input and publishes, it must fire the installed
        // waker so an event-driven render loop knows to present the update.
        let relay = Arc::new(Relay::new().unwrap());
        let woke = Arc::new(AtomicUsize::new(0));
        let woke_cb = Arc::clone(&woke);
        relay.set_frame_waker(Arc::new(move || {
            woke_cb.fetch_add(1, Ordering::SeqCst);
        }));

        let hits = Arc::new(AtomicUsize::new(0));
        let hits_factory = Arc::clone(&hits);
        let handle = Relay::spawn_logic_from_view(&relay, move |arena| {
            Box::new(CounterRuntime {
                signal: crate::evaluator::Signal::new_in(arena, 0_i64),
                hits: hits_factory,
            })
        })
        .expect("thread spawn should succeed in tests");

        // Feed an input event; the next (input-bearing) tick must wake.
        relay.push_input(InputEvent {
            kind: crate::platform::EventKind::PointerDown,
            pos: (1.0, 1.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
        });

        let start = Instant::now();
        while woke.load(Ordering::SeqCst) == 0 && start.elapsed() < Duration::from_secs(2) {
            thread::yield_now();
        }
        relay.request_shutdown();
        handle.join().expect("logic thread must not panic");

        assert!(
            woke.load(Ordering::SeqCst) >= 1,
            "an input-bearing tick must fire the frame waker"
        );
    }
}
