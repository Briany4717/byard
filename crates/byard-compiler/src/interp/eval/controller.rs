//! The `byld` half of the controller boundary (RFC-0028 §4–§7) and the
//! structural effects that drive it (RFC-0028 §4b).
//!
//! Three things live here, and they exist together because each one is only
//! useful with the other two:
//!
//! - **The call.** `api.forecast("Tokyo") ok r => { … } err e => { … }`
//!   converts its arguments to [`HostValue`], hands them to the
//!   [`Dispatcher`], and returns. Nothing awaits; the view keeps rendering the
//!   frame it was already on.
//! - **The continuation.** The arms are lowered once, at the call site, and
//!   stored under a fresh id. When the reply lands on the logic thread, the
//!   matching arm runs as an ordinary action, writes a `var`, and the normal
//!   Mark-and-Pull path takes it from there. Nothing about the reply path is
//!   special after that point, which is the whole design.
//! - **The effect.** An `on mount => …` is what asks for the data in the first
//!   place. It is a structural effect, so a screen that comes back under a
//!   `when` asks again rather than showing what the previous mount loaded.
//!
//! ## Why an action cannot place the call itself
//!
//! A lowered action is a `FnMut(&mut ReactiveCtx, Option<&Value>)`: it can
//! read and write signals and nothing else. It cannot reach the registry, the
//! runtime handle, or the continuation table, all of which live on the
//! `Interpreter`, and widening it to `&mut Interpreter` would put the whole
//! render walk behind a mutable borrow of the thing it is walking.
//!
//! So an action *raises* a call rather than placing it: it pushes a
//! [`PendingCall`] onto a queue it shares with the interpreter (an `Rc`, never
//! `Send`, never off the logic thread), and [`Interpreter::drain_calls`]
//! schedules it at the next `&mut self` point. The delay is bounded by one
//! drain, both of which happen in the frame the action ran in.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use byard_core::bridge::{
    ControllerId, ControllerReply, Dispatcher, HostValue, TimerHandle, TimerTick,
};
use byard_core::relay::IoResult;

use crate::diagnostics::{CompileError, Span};
use crate::interp::bridge::{host_to_value, value_to_host};
use crate::interp::env::Value;
use crate::interp::events::Action;
use crate::parser::ast::{Arg, Expr, ResultArm};
use crate::symbol::Symbol;

use super::{Interpreter, Lowered};

/// The `ok`/`err` arms of one call site, shared by every invocation of it.
///
/// Shared rather than cloned per call because an [`Action`] is a boxed
/// `FnMut` and cannot be cloned, and because it is the honest model anyway:
/// two in-flight calls from the same button really do resume into the same
/// written arm. They cannot interleave, the logic thread runs one reply at a
/// time, and each carries its own payload.
pub(super) struct CallArms {
    /// The success arm, if written.
    pub ok: Option<Action>,
    /// The failure arm, if written.
    pub err: Option<Action>,
}

/// A controller call raised by a running action and not yet scheduled.
pub(super) struct PendingCall {
    /// Which registered controller to dispatch on.
    controller: ControllerId,
    /// The method name.
    method: String,
    /// Arguments, already converted to the `Send` boundary type.
    args: Vec<HostValue>,
    /// The arms to resume into, or `None` for fire-and-forget.
    arms: Option<Rc<RefCell<CallArms>>>,
    /// The effect that owns this call, if it was raised from one. A call an
    /// effect placed dies with that effect (INV-14).
    owner: Option<usize>,
    /// The call site, for diagnostics.
    span: Span,
}

/// The logic-thread queue an action raises calls onto. `Rc`, so it is
/// structurally incapable of leaving the logic thread (INV-2).
pub(super) type CallQueue = Rc<RefCell<Vec<PendingCall>>>;

/// A sink for diagnostics raised from inside a lowered closure, which has no
/// path back to `Interpreter::errors`. Drained once per tick.
pub(super) type DiagSink = Rc<RefCell<Vec<CompileError>>>;

/// One outstanding call's resume point (RFC-0028 §5 step 1).
struct Continuation {
    /// The call site's arms.
    arms: Rc<RefCell<CallArms>>,
    /// The effect that owns it, if any.
    owner: Option<usize>,
    /// The call site, so a discarded reply can point at it.
    span: Span,
    /// Whether this continuation survives being resumed. A controller reply is
    /// one-shot; an `every` timer's is not, because it resumes into the same
    /// written action on every tick.
    repeating: bool,
}

/// What kind of structural effect a slot holds.
pub(crate) enum EffectKind {
    /// `on mount => …`, run once each time the enclosing scope mounts.
    Mount,
    /// `on unmount => …`, run once each time it unmounts.
    Unmount,
    /// `every <dur> => …` / `after <dur> => …` (RFC-0029 §5): armed when the
    /// scope mounts, cancelled when it unmounts.
    Timer {
        /// `true` for `every` (repeating), `false` for `after` (one-shot).
        every: bool,
        /// The interval or delay, in milliseconds.
        dur_ms: u64,
    },
}

/// One structural effect and the mount state it is tracking (RFC-0028 §4b).
///
/// Liveness is decided by **being visited**: `reconcile_structure` walks
/// exactly the mounted tree every frame, so an effect that is reached this
/// frame is mounted and one that is not has unmounted. That is the same
/// mark-and-sweep the animation state already uses ("an entry whose element no
/// longer renders ages out"), and it is why `when`, `for`, navigation, a
/// user-view instance and a hot reload all get correct mount edges here
/// without any of them growing a hook.
pub(super) struct EffectSlot {
    /// Which edge this effect fires on.
    pub kind: EffectKind,
    /// The lowered action.
    ///
    /// Held as [`CallArms`] rather than a bare [`Action`] so a timer can
    /// register it as a continuation without a second representation: a tick
    /// resumes into the `ok` arm exactly as a reply does, which is what lets
    /// both travel one delivery path (RFC-0029 §5). Shared so a fire does not
    /// need `&mut self` on the vector holding it.
    pub action: Rc<RefCell<CallArms>>,
    /// The `frame_seq` this effect was last visited on.
    pub seen: u64,
    /// Whether its scope is currently mounted.
    pub mounted: bool,
    /// The armed timer, for a [`EffectKind::Timer`] whose scope is mounted.
    /// Dropping it cancels the Tokio task, which is the entire leak story:
    /// unmounting drops this, and a cancelled timer cannot fire into a view
    /// that is gone (INV-10).
    pub timer: Option<TimerHandle>,
    /// The continuation a timer's ticks resume into, so an unmount can drop it
    /// alongside the task.
    pub continuation: Option<u64>,
}

/// Everything the interpreter needs to reach the async world, kept in one
/// struct so `Interpreter` grows one field rather than six.
#[derive(Default)]
pub(super) struct Bridge {
    /// The registry + runtime handle + reply channel, once a host has wired
    /// one in. `None` in a headless test, where a call degrades to a
    /// diagnostic instead of a panic (INV-4).
    pub dispatcher: Option<Dispatcher>,
    /// Calls raised by actions, drained at the next `&mut self` point.
    pub queue: CallQueue,
    /// Diagnostics raised from inside lowered closures.
    pub diagnostics: DiagSink,
    /// Outstanding calls, keyed by continuation id (one-shot, INV-14).
    continuations: HashMap<u64, Continuation>,
    /// The call sites of continuations that were dropped with a reply still in
    /// flight, so the discard can be reported **at the call** instead of at
    /// nothing.
    ///
    /// Bounded by construction rather than by a cap: every controller call
    /// produces exactly one reply, and the reply removes its entry. A timer's
    /// continuation is never recorded, because its task is aborted with it.
    discarded: HashMap<u64, Span>,
    /// Monotonic continuation id source.
    next_id: u64,
    /// The structural effects lowered so far.
    pub effects: Vec<EffectSlot>,
    /// The effect currently running, so a call it raises is owned by it.
    ///
    /// Shared with every lowered call closure, because ownership is a
    /// **run-time** fact: an effect's action is lowered long before it fires,
    /// and often outside any effect at all (an event handler inside the same
    /// branch). Reading it at lower time would attribute every call to
    /// whatever happened to be running when the tree was built, which is
    /// nothing.
    pub running_effect: Rc<std::cell::Cell<Option<usize>>>,
    /// How deep the current lowering is inside an action. Zero means a pure
    /// context, where a call is [`CompileError::EffectInPureContext`].
    pub action_depth: u32,
}

impl Bridge {
    /// Registers `arms` under a fresh continuation id. `repeating` is `true`
    /// only for an `every` timer, whose ticks resume into the same action
    /// forever; everything else is one-shot.
    fn open(
        &mut self,
        arms: Rc<RefCell<CallArms>>,
        owner: Option<usize>,
        span: Span,
        repeating: bool,
    ) -> u64 {
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        self.continuations.insert(
            id,
            Continuation {
                arms,
                owner,
                span,
                repeating,
            },
        );
        id
    }
}

/// The handle an `inject` binds when nothing provides the named type and
/// nothing could have (`byard check`, a headless test).
///
/// A real id would name some other controller; a `None` would make every call
/// on it lower to nothing and skip the checks the call site deserves. An id
/// that can never be registered gets both: the call lowers and is checked, and
/// if one is ever actually placed the dispatcher answers with the
/// `unregistered` error reply rather than dispatching to a stranger.
pub(super) const UNBOUND_CONTROLLER: ControllerId = ControllerId(u32::MAX);

impl Interpreter {
    /// Raises [`Bridge::action_depth`] for the duration of `lower`, marking
    /// everything lowered inside it as action position (RFC-0028 §4).
    pub(super) fn in_action_position<T>(&mut self, lower: impl FnOnce(&mut Self) -> T) -> T {
        self.bridge.action_depth += 1;
        let out = lower(self);
        self.bridge.action_depth -= 1;
        out
    }

    /// Whether a host has wired a controller registry in. `false` under
    /// `byard check` and in headless tests, where an unresolved `inject` is
    /// unknowable rather than wrong.
    pub(super) fn has_dispatcher(&self) -> bool {
        self.bridge.dispatcher.is_some()
    }

    /// Wires this interpreter to the async world and seeds one ambient
    /// controller handle per registered controller, so `inject Http as http`
    /// resolves (RFC-0028 §3).
    ///
    /// Call once, before [`lower_view`](Interpreter::lower_view): `inject`
    /// resolves against the ambient chain at lower time, so a handle provided
    /// afterwards would be invisible to the view that wanted it.
    pub fn set_dispatcher(&mut self, dispatcher: Dispatcher) {
        self.bridge.dispatcher = Some(dispatcher);
        self.provide_controllers();
    }

    /// Declares that `names` will be provided at run time, without wiring a
    /// live dispatcher (RFC-0028 §3).
    ///
    /// This is what lets `byard check` resolve `inject Http as http` and check
    /// the calls on it. The framework's own capabilities are knowable
    /// statically, unlike an app's, so treating them as unknown would report a
    /// warning on the one half of the controller vocabulary the checker can be
    /// certain about.
    ///
    /// The handles are unbound: a checker never places a call, and if some
    /// other caller did, the dispatcher would answer with the `unregistered`
    /// error reply rather than dispatching to a stranger.
    pub fn declare_controllers(&mut self, names: &[&str]) {
        for name in names {
            self.env
                .provide(Symbol::intern(name), Value::Controller(UNBOUND_CONTROLLER));
        }
    }

    /// Seeds one ambient `Value::Controller` per registered controller into
    /// the root environment, so `resolve_inject` (unchanged since RFC-0002)
    /// finds a handle where it used to find only values.
    pub(super) fn provide_controllers(&mut self) {
        let Some(dispatcher) = self.bridge.dispatcher.clone() else {
            return;
        };
        for (index, name) in dispatcher.registry().names().enumerate() {
            let id = ControllerId(u32::try_from(index).unwrap_or(u32::MAX));
            self.env
                .provide(Symbol::intern(name), Value::Controller(id));
        }
    }

    /// Whether a call's receiver resolves to anything at all, so a diagnostic
    /// about the *call* is not raised on top of one about its `inject`.
    pub(super) fn receiver_is_bound(&self, callee: &Expr) -> bool {
        let Expr::Member { base, .. } = callee else {
            return true;
        };
        let Expr::Ident(receiver, _) = base.as_ref() else {
            return true;
        };
        self.env.lookup(receiver).is_some()
    }

    /// The `ControllerId` bound to `name` in the current environment, if that
    /// name resolves to a controller handle at all.
    fn controller_handle(&self, name: &Symbol) -> Option<ControllerId> {
        match self.env.lookup(name) {
            Some(Value::Controller(id)) => Some(*id),
            _ => None,
        }
    }

    /// Lowers `receiver.method(args)` when `receiver` is a controller handle,
    /// with optional result arms (RFC-0028 §4). Returns `None` when the
    /// receiver is not a controller, so the caller falls through to the
    /// ordinary member-call paths (collection methods, `anim.*`).
    pub(super) fn lower_controller_call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        ok: Option<&ResultArm>,
        err: Option<&ResultArm>,
        span: Span,
    ) -> Option<Lowered> {
        let Expr::Member { base, field, .. } = callee else {
            return None;
        };
        let Expr::Ident(receiver, _) = base.as_ref() else {
            return None;
        };
        let controller = self.controller_handle(receiver)?;

        // RFC-0028 §4: a call is legal only in action position. `action_depth`
        // is raised by `lower_action` and by a bare statement member, so
        // anything reached at depth zero, a `let`, a memo body, an attribute
        // value, is a projection, and a projection that could place a call
        // would place it again on every re-pull.
        if self.bridge.action_depth == 0 {
            self.errors.push(CompileError::EffectInPureContext {
                span,
                context: "a value position".to_string(),
            });
            return Some(Box::new(|_| Value::Unit));
        }

        let method = field.as_str().to_string();
        let mut arg_computes: Vec<Lowered> = args
            .iter()
            .map(|a| self.lower_expr(&a.value, None))
            .collect();

        // The arms are lowered **here**, once, in the environment the call was
        // written in. Lowering them when the reply lands would be lowering
        // them somewhere else: the row, the branch, the instance the call
        // belongs to may all be out of scope by then, and the arm would
        // resolve `t` against an environment that no longer has one.
        let arms = if ok.is_some() || err.is_some() {
            let ok_action = ok.and_then(|arm| {
                self.lower_action(&arm.action, Some(arm.binding.clone()))
                    .ok()
            });
            let err_action = err.and_then(|arm| {
                self.lower_action(&arm.action, Some(arm.binding.clone()))
                    .ok()
            });
            Some(Rc::new(RefCell::new(CallArms {
                ok: ok_action,
                err: err_action,
            })))
        } else {
            None
        };

        let queue = Rc::clone(&self.bridge.queue);
        let diagnostics = Rc::clone(&self.bridge.diagnostics);
        let running_effect = Rc::clone(&self.bridge.running_effect);
        Some(Box::new(move |ctx| {
            let mut host_args = Vec::with_capacity(arg_computes.len());
            for compute in &mut arg_computes {
                let value = compute(ctx);
                let Some(host) = value_to_host(&value) else {
                    // A handle, a signal or a callback reached the boundary.
                    // The call is abandoned rather than truncated: sending
                    // `Unit` in its place would run the controller against
                    // arguments the author never wrote.
                    diagnostics
                        .borrow_mut()
                        .push(CompileError::NonDataControllerArg {
                            span,
                            method: method.clone(),
                        });
                    return Value::Unit;
                };
                host_args.push(host);
            }
            queue.borrow_mut().push(PendingCall {
                controller,
                method: method.clone(),
                args: host_args,
                arms: arms.clone(),
                owner: running_effect.get(),
                span,
            });
            Value::Unit
        }))
    }

    /// Schedules every call raised since the last drain (RFC-0028 §5 step 2).
    ///
    /// Called at the two `&mut self` points every frame passes through, so a
    /// call raised by an event handler, by a mount effect, or by another
    /// call's result arm all reach the pool inside the frame that raised them.
    pub(super) fn drain_calls(&mut self) {
        // Take the queue out first: a call's own arguments have already been
        // evaluated, so nothing here can push more, but taking it keeps the
        // `RefCell` un-borrowed across the spawn either way.
        let pending: Vec<PendingCall> = std::mem::take(&mut *self.bridge.queue.borrow_mut());
        for call in pending {
            let Some(dispatcher) = self.bridge.dispatcher.clone() else {
                // Nothing to dispatch onto, so this is a check and not a run:
                // `byard check` renders every view to validate it, which runs
                // its `on mount` effects, and a checker that reported "this
                // call did not happen" would fail every correct data-backed
                // screen. The call is simply not placed.
                continue;
            };
            let continuation = match call.arms {
                Some(arms) => self.bridge.open(arms, call.owner, call.span, false),
                // Fire-and-forget still gets an id, so the reply has somewhere
                // to be discarded rather than being an untracked message.
                None => 0,
            };
            dispatcher.spawn_call(call.controller, call.method, call.args, continuation);
        }
    }

    /// Applies everything the pool completed since the last tick (RFC-0028 §5
    /// step 4). Returns whether any of it ran an arm, which is what tells the
    /// relay whether the frame is worth waking a `Wait`-mode renderer for.
    #[must_use]
    pub fn apply_io_results(&mut self, results: Vec<IoResult>) -> bool {
        let mut applied = false;
        for result in results {
            let result = match result.downcast::<ControllerReply>() {
                Ok(reply) => {
                    applied |= self.resume(reply.continuation_id, reply.result);
                    continue;
                }
                Err(other) => other,
            };
            // RFC-0029 §5: a timer tick is a zero-argument reply, delivered
            // through this same path so there is exactly one place where async
            // work becomes a `var` write.
            if let Ok(tick) = result.downcast::<TimerTick>() {
                applied |= self.fire_timer(tick.continuation_id);
            }
        }
        // A result arm may itself have called a controller.
        self.drain_calls();
        applied
    }

    /// Runs the arm `continuation_id` names, binding `result`'s payload.
    fn resume(&mut self, continuation_id: u64, result: Result<HostValue, HostValue>) -> bool {
        // Fire-and-forget: the reply is expected and there is nothing to run.
        if continuation_id == 0 {
            return false;
        }
        let Some(continuation) = self.bridge.continuations.remove(&continuation_id) else {
            // Its view unmounted, or a hot reload replaced the program
            // (INV-14). Never applied to a stale `var`; reported so the
            // developer does not read the silence as "no answer came back",
            // and reported **at the call that was dropped**, which is the only
            // place the message is actionable.
            if let Some(span) = self.bridge.discarded.remove(&continuation_id) {
                self.errors
                    .push(CompileError::DiscardedControllerReply { span });
            }
            return false;
        };
        let span = continuation.span;
        let (payload, is_ok) = match result {
            Ok(value) => (value, true),
            Err(value) => (value, false),
        };
        let value = host_to_value(&payload);
        let mut arms = continuation.arms.borrow_mut();
        let arm = if is_ok {
            arms.ok.as_mut()
        } else {
            arms.err.as_mut()
        };
        let Some(arm) = arm else {
            // The other arm was written but not this one. A success nobody
            // asked about is fine; an *error* nobody asked about is a silent
            // failure, so it is surfaced.
            drop(arms);
            if !is_ok {
                self.report_unhandled_error(&value, span);
            }
            return false;
        };
        arm(&mut self.ctx, Some(&value));
        true
    }

    /// Surfaces a controller error whose call site wrote no `err` arm.
    fn report_unhandled_error(&mut self, value: &Value, span: Span) {
        let message = match value {
            Value::Record(fields) => fields
                .iter()
                .find(|(k, _)| k.as_str() == "message")
                .and_then(|(_, v)| match v {
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "the controller call failed".to_string()),
            Value::Str(s) => s.clone(),
            _ => "the controller call failed".to_string(),
        };
        self.errors
            .push(CompileError::ControllerCallFailed { span, message });
    }

    /// Runs a timer's action (RFC-0029 §5). Wired in a later slice; the
    /// delivery path is shared with [`resume`](Self::resume) so the two can
    /// never drift.
    fn fire_timer(&mut self, continuation_id: u64) -> bool {
        let Some(continuation) = self.bridge.continuations.get(&continuation_id) else {
            // Its scope unmounted between the tick being queued and this drain.
            // Silently dropped rather than reported: unlike a controller reply,
            // a timer tick that lands one frame late is an ordinary race, not a
            // sign that something went wrong.
            return false;
        };
        let arms = Rc::clone(&continuation.arms);
        // An `after` fires once. Removing the continuation here, rather than
        // relying on the task having ended, means a duplicate delivery can
        // never run a one-shot action twice.
        if !continuation.repeating {
            self.bridge.continuations.remove(&continuation_id);
        }
        let mut arms = arms.borrow_mut();
        let Some(arm) = arms.ok.as_mut() else {
            return false;
        };
        arm(&mut self.ctx, None);
        true
    }

    // ── structural effects (RFC-0028 §4b) ────────────────────────────────

    /// Registers a lowered lifecycle effect and returns its slot index.
    pub(super) fn register_effect(&mut self, kind: EffectKind, action: Action) -> usize {
        let index = self.bridge.effects.len();
        self.bridge.effects.push(EffectSlot {
            kind,
            action: Rc::new(RefCell::new(CallArms {
                ok: Some(action),
                err: None,
            })),
            // Not `frame_seq`: an effect lowered mid-reconcile must still be
            // *visited* to count as mounted, so the sweep sees one consistent
            // rule rather than a lowering-order exception.
            seen: 0,
            mounted: false,
            timer: None,
            continuation: None,
        });
        index
    }

    /// Records that effect `index` was reached by this frame's structural
    /// walk, i.e. its scope is mounted.
    pub(super) fn mark_effect_seen(&mut self, index: usize) {
        let frame = self.frame_seq;
        if let Some(slot) = self.bridge.effects.get_mut(index) {
            slot.seen = frame;
        }
    }

    /// Runs the mount and unmount edges this frame's walk revealed, then
    /// schedules anything they called (RFC-0028 §4b).
    ///
    /// Unmounts run before mounts: a `when` flip is one scope leaving and
    /// another arriving, and the leaving one's teardown has to happen before
    /// the arriving one's setup, or a screen that releases a resource on
    /// unmount would release the one its replacement had just acquired.
    pub(super) fn settle_effects(&mut self) -> bool {
        let frame = self.frame_seq;
        let mut unmounted = Vec::new();
        let mut mounted = Vec::new();
        for (index, slot) in self.bridge.effects.iter_mut().enumerate() {
            let live = slot.seen == frame;
            if slot.mounted && !live {
                slot.mounted = false;
                unmounted.push(index);
            } else if !slot.mounted && live {
                slot.mounted = true;
                mounted.push(index);
            }
        }
        let fired_unmount = !unmounted.is_empty();
        for index in unmounted {
            // A scope that is gone cannot be resumed into, so anything it had
            // in flight is dropped now rather than applied to its corpse
            // (INV-14, INV-10: nothing survives the scope that started it).
            self.bridge
                .continuations
                .retain(|_, c| c.owner != Some(index));
            self.disarm_timer(index);
            self.run_effect(index, &EffectKind::Unmount);
        }
        let fired = !mounted.is_empty();
        for index in mounted {
            self.arm_timer(index);
            self.run_effect(index, &EffectKind::Mount);
        }
        self.drain_calls();
        fired || fired_unmount
    }

    /// Arms effect `index`'s timer, if it is one (RFC-0029 §5).
    ///
    /// The action is registered as a continuation first, so a tick is resumed
    /// through exactly the same path a controller reply is: one delivery
    /// mechanism, one consistency boundary, one waker amendment. A timer tick
    /// really is a zero-argument reply, and giving it a second path would mean
    /// a second set of ordering and leak rules to keep in step with the first.
    fn arm_timer(&mut self, index: usize) {
        let Some(slot) = self.bridge.effects.get(index) else {
            return;
        };
        let EffectKind::Timer { every, dur_ms } = slot.kind else {
            return;
        };
        let arms = Rc::clone(&slot.action);
        let Some(dispatcher) = self.bridge.dispatcher.clone() else {
            // A check, not a run (see `drain_calls`): there is no runtime to
            // arm against and nothing is waiting for a tick.
            return;
        };
        let continuation = self.bridge.open(arms, Some(index), Span::new(0, 0), every);
        let handle = dispatcher.spawn_timer(every, dur_ms, continuation);
        if let Some(slot) = self.bridge.effects.get_mut(index) {
            slot.continuation = Some(continuation);
            slot.timer = handle;
        }
    }

    /// Cancels effect `index`'s timer and forgets its continuation.
    fn disarm_timer(&mut self, index: usize) {
        if let Some(slot) = self.bridge.effects.get_mut(index) {
            // Dropping the handle aborts the Tokio task (INV-10).
            slot.timer = None;
            slot.continuation = None;
        }
    }

    /// Runs effect `index` if it fires on `edge`.
    fn run_effect(&mut self, index: usize, edge: &EffectKind) {
        let Some(slot) = self.bridge.effects.get(index) else {
            return;
        };
        // A timer has no mount/unmount action of its own: arming and
        // cancelling *is* its lifecycle, and `arm_timer`/`disarm_timer` own it.
        if !matches!(
            (&slot.kind, edge),
            (EffectKind::Mount, EffectKind::Mount) | (EffectKind::Unmount, EffectKind::Unmount)
        ) {
            return;
        }
        let action = Rc::clone(&slot.action);
        // Attributed while it runs, so a call the effect places is owned by it
        // and dies with it.
        self.bridge.running_effect.set(Some(index));
        if let Some(run) = action.borrow_mut().ok.as_mut() {
            run(&mut self.ctx, None);
        }
        self.bridge.running_effect.set(None);
    }

    /// Moves diagnostics raised inside lowered closures onto the error list.
    pub(super) fn drain_closure_diagnostics(&mut self) {
        let raised: Vec<CompileError> = std::mem::take(&mut *self.bridge.diagnostics.borrow_mut());
        self.errors.extend(raised);
    }

    /// Drops every outstanding continuation and forgets every effect's mount
    /// state, so a reloaded program starts its lifecycles over (RFC-0028 §5,
    /// "a view unmount drops pending continuations").
    pub(super) fn reset_bridge_state(&mut self) {
        self.remember_discarded(|_| true);
        self.bridge.effects.clear();
        self.bridge.queue.borrow_mut().clear();
    }

    /// Drops every continuation matching `doomed`, keeping its call site so a
    /// reply that arrives afterwards can be reported where it was written.
    fn remember_discarded(&mut self, doomed: impl Fn(&Continuation) -> bool) {
        let ids: Vec<u64> = self
            .bridge
            .continuations
            .iter()
            .filter(|(_, c)| doomed(c))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(continuation) = self.bridge.continuations.remove(&id) {
                // A repeating continuation is a timer's: its task is aborted
                // alongside it, so a tick that still lands is an ordinary race
                // rather than an answer nobody will hear, and reporting it
                // would put a diagnostic on every screen that closes.
                if !continuation.repeating {
                    self.bridge.discarded.insert(id, continuation.span);
                }
            }
        }
    }

    /// How many controller replies are still outstanding. Test-facing: the
    /// direct witness that a continuation is one-shot and that an unmount
    /// takes its scope's calls with it.
    #[must_use]
    pub fn outstanding_continuations(&self) -> usize {
        self.bridge.continuations.len()
    }

    /// How many structural effects are currently mounted. Test-facing, the
    /// witness for the `on mount`/`on unmount` edges.
    #[must_use]
    pub fn mounted_effects(&self) -> usize {
        self.bridge.effects.iter().filter(|e| e.mounted).count()
    }
}
