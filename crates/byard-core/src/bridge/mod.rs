//! The controller boundary (RFC-0028): the `Send`-only wire type, the
//! object-safe [`Controller`] trait, and the [`ControllerRegistry`], all in
//! `byard-core` so the trait that both the app crate and the interpreter speak
//! drags **no** `byard-compiler` dependency into core (INV-1). Nothing here
//! knows about `Signal`/`Value`/views; the `Value ⇄ HostValue` conversions live
//! one layer up in `byard-compiler`, which depends on core, never the reverse.
//!
//! Everything that crosses the logic ↔ Tokio-pool boundary is `Send` data
//! (INV-2): [`HostValue`] is `Send + 'static` and holds no `Signal`, `Fn`, or
//! view handle (INV-13, statically asserted below).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A boxed, `Send` future, the return shape of an async controller method
/// after type erasure (RFC-0028 §2).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The neutral, `Send`, serialization-free boundary value (RFC-0028 §1). It
/// mirrors the data subset of the interpreter's `Value` (and RFC-0027's
/// `Record`), so a controller's arguments and results drop straight into the
/// reactive tree without copying through serde. `Signal`/`Memo`/`Fn` have **no**
/// `HostValue` form, passing one as a controller argument is a compile error
/// (`NonDataControllerArg`) in `byard-compiler`.
#[derive(Clone, Debug, PartialEq)]
pub enum HostValue {
    /// The unit value.
    Unit,
    /// A boolean.
    Bool(bool),
    /// A 64-bit integer.
    Int(i64),
    /// A 64-bit float.
    Float(f64),
    /// A UTF-8 string.
    Str(String),
    /// An ordered list.
    List(Vec<HostValue>),
    /// A name-keyed, ordered record (RFC-0027 §6 shape).
    Record(Vec<(String, HostValue)>),
}

// INV-13: the boundary type is `Send + 'static` and owns only plain data.
const _: () = {
    const fn assert_send_static<T: Send + 'static>() {}
    assert_send_static::<HostValue>();
};

impl HostValue {
    /// Reads a record field by name, if this is a [`HostValue::Record`] that has
    /// it. A convenience for controller code assembling replies.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&HostValue> {
        match self {
            HostValue::Record(fields) => fields.iter().find(|(k, _)| k == name).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Converts a [`HostValue`] argument into a controller method's Rust parameter
/// type (RFC-0028 §2). Total but lenient: a mismatched shape yields the type's
/// [`Default`]-ish fallback rather than panicking (INV-4, arguments are
/// user-derived). Implemented for scalars, `String`, `HostValue` itself, and
/// `Vec<T>`; `#[derive(HostValue)]` structs get it too.
pub trait FromHostValue: Sized {
    /// Builds `Self` from a boundary value.
    fn from_host(value: HostValue) -> Self;
}

/// Converts a controller method's return/error type into a [`HostValue`] to send
/// back across the boundary (RFC-0028 §2). Implemented for scalars, `String`,
/// `HostValue` itself, `Vec<T>`, and (via derive) records.
pub trait IntoHostValue {
    /// Consumes `self` into a boundary value.
    fn into_host(self) -> HostValue;
}

impl FromHostValue for HostValue {
    fn from_host(value: HostValue) -> Self {
        value
    }
}
impl IntoHostValue for HostValue {
    fn into_host(self) -> HostValue {
        self
    }
}

impl FromHostValue for () {
    fn from_host(_: HostValue) -> Self {}
}
impl IntoHostValue for () {
    fn into_host(self) -> HostValue {
        HostValue::Unit
    }
}

impl FromHostValue for bool {
    fn from_host(value: HostValue) -> Self {
        matches!(value, HostValue::Bool(true))
    }
}
impl IntoHostValue for bool {
    fn into_host(self) -> HostValue {
        HostValue::Bool(self)
    }
}

impl FromHostValue for String {
    fn from_host(value: HostValue) -> Self {
        match value {
            HostValue::Str(s) => s,
            other => format!("{other:?}"),
        }
    }
}
impl IntoHostValue for String {
    fn into_host(self) -> HostValue {
        HostValue::Str(self)
    }
}
impl IntoHostValue for &str {
    fn into_host(self) -> HostValue {
        HostValue::Str(self.to_string())
    }
}

impl IntoHostValue for f64 {
    fn into_host(self) -> HostValue {
        HostValue::Float(self)
    }
}
impl FromHostValue for f64 {
    #[allow(clippy::cast_precision_loss)]
    fn from_host(value: HostValue) -> Self {
        match value {
            HostValue::Float(f) => f,
            HostValue::Int(n) => n as f64,
            _ => 0.0,
        }
    }
}

/// Generates `FromHostValue`/`IntoHostValue` for every signed/unsigned integer
/// type, going through `i64` (the single integer `HostValue` variant).
macro_rules! int_host_value {
    ($($t:ty),*) => {$(
        impl FromHostValue for $t {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::cast_sign_loss)]
            fn from_host(value: HostValue) -> Self {
                match value {
                    HostValue::Int(n) => n as $t,
                    HostValue::Float(f) => f as $t,
                    _ => 0,
                }
            }
        }
        impl IntoHostValue for $t {
            fn into_host(self) -> HostValue {
                HostValue::Int(i64::from(self))
            }
        }
    )*};
}
int_host_value!(i8, i16, i32, u8, u16, u32);

impl FromHostValue for i64 {
    fn from_host(value: HostValue) -> Self {
        match value {
            HostValue::Int(n) => n,
            #[allow(clippy::cast_possible_truncation)]
            HostValue::Float(f) => f as i64,
            _ => 0,
        }
    }
}
impl IntoHostValue for i64 {
    fn into_host(self) -> HostValue {
        HostValue::Int(self)
    }
}

impl<T: FromHostValue> FromHostValue for Vec<T> {
    fn from_host(value: HostValue) -> Self {
        match value {
            HostValue::List(xs) => xs.into_iter().map(T::from_host).collect(),
            _ => Vec::new(),
        }
    }
}
impl<T: IntoHostValue> IntoHostValue for Vec<T> {
    fn into_host(self) -> HostValue {
        HostValue::List(self.into_iter().map(IntoHostValue::into_host).collect())
    }
}

/// A Rust struct exposed to `byld` as an ambient, async-dispatchable service
/// (RFC-0028 §2). `#[byard_controller]` generates the implementation; apps may
/// also implement it by hand. Object-safe so the registry can hold
/// `Arc<dyn Controller>`.
pub trait Controller: Send + Sync {
    /// The stable type name used as the `inject` key, the struct's ident.
    fn type_name(&self) -> &'static str;

    /// Dispatches one async method by name, converting `args` into the method's
    /// Rust parameter types, awaiting it, and mapping `Ok`/`Err` back to
    /// [`HostValue`]. Returns a boxed future; it never blocks the caller (the
    /// blocking/async work runs on the Tokio pool, INV-12). An unknown method
    /// resolves to an `Err` reply, never a panic (INV-4).
    fn invoke(
        &self,
        method: &str,
        args: Vec<HostValue>,
    ) -> BoxFuture<'static, Result<HostValue, HostValue>>;
}

/// A `Copy` index into the [`ControllerRegistry`]. Read only on the logic thread
/// (it only *schedules* work onto the pool, never dereferences a controller off
/// that thread, INV-2), so it stays arena-friendly and cheap to store in a
/// `Value`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ControllerId(pub u32);

/// The engine-owned set of registered controllers (RFC-0028 §3), keyed by
/// `type_name()`. Held by the app/engine and reachable from the logic thread;
/// `App::provide(c)` inserts `c.type_name() → Arc::new(c)`.
#[derive(Default, Clone)]
pub struct ControllerRegistry {
    /// Insertion order preserved so [`ControllerId`] indices are stable.
    controllers: Vec<Arc<dyn Controller>>,
    index: HashMap<&'static str, u32>,
}

impl ControllerRegistry {
    /// A new, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `controller`, returning its stable [`ControllerId`]. Re-inserting
    /// a controller with the same `type_name()` replaces the earlier one but
    /// keeps its id (last provider wins, arena-stable).
    pub fn insert(&mut self, controller: Arc<dyn Controller>) -> ControllerId {
        let name = controller.type_name();
        if let Some(&idx) = self.index.get(name) {
            self.controllers[idx as usize] = controller;
            return ControllerId(idx);
        }
        let idx = u32::try_from(self.controllers.len()).unwrap_or(u32::MAX);
        self.controllers.push(controller);
        self.index.insert(name, idx);
        ControllerId(idx)
    }

    /// The [`ControllerId`] registered under `name`, if any.
    #[must_use]
    pub fn id_of(&self, name: &str) -> Option<ControllerId> {
        self.index.get(name).copied().map(ControllerId)
    }

    /// Whether a controller named `name` is registered.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    /// The controller handle at `id`, if the id is in range.
    #[must_use]
    pub fn get(&self, id: ControllerId) -> Option<Arc<dyn Controller>> {
        self.controllers.get(id.0 as usize).cloned()
    }

    /// The registered type names, in insertion order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.controllers.iter().map(|c| c.type_name())
    }

    /// How many controllers are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.controllers.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.controllers.is_empty()
    }
}

/// A completed controller call delivered back to the logic thread (RFC-0028 §5).
/// Sent over the relay's `io_result` channel as a `Box<dyn Any + Send>` and
/// downcast by `Interpreter::apply_io_results`, which runs the matching `ok`/`err`
/// arm keyed by `continuation_id` (a one-shot continuation, INV-14).
pub struct ControllerReply {
    /// The continuation this reply resumes; a reply whose id was dropped (its
    /// view unmounted) is discarded, never applied (INV-14).
    pub continuation_id: u64,
    /// The success (`Ok`) or error (`Err`) payload, as `Send` data.
    pub result: Result<HostValue, HostValue>,
}

/// An armed timer, cancelled when this handle is dropped (RFC-0029 §5).
///
/// Cancellation on drop rather than an explicit `stop()` because the failure
/// mode of the explicit form is a timer that keeps firing into a view that no
/// longer exists, and that failure is invisible until something writes a `var`
/// nobody is watching. Tying it to ownership makes "the scope went away" and
/// "the timer stopped" the same event.
#[cfg(feature = "runtime-io")]
pub struct TimerHandle {
    abort: tokio::task::AbortHandle,
}

#[cfg(feature = "runtime-io")]
impl Drop for TimerHandle {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

/// A timer effect firing (RFC-0029 §5): a zero-argument reply delivered through
/// the same logic-thread apply path as a [`ControllerReply`], running the
/// timer's action.
pub struct TimerTick {
    /// The timer's continuation (its bound action).
    pub continuation_id: u64,
}

/// The interpreter's whole view of the async world (RFC-0028 §5 step 2).
///
/// The logic thread needs three things to place a call: the registry to look
/// the controller up in, a runtime handle to spawn on, and the sender the
/// reply comes back through. Bundling them here rather than handing the
/// interpreter a `tokio::runtime::Handle` directly is what keeps
/// `byard-compiler` free of any async dependency: it holds one `Dispatcher`,
/// calls [`spawn_call`](Dispatcher::spawn_call), and never names a future, a
/// runtime or a channel.
///
/// Cheap to clone (three handles) and `Send`, so it can be moved into the
/// logic-thread factory closure alongside the compiled views.
#[derive(Clone)]
pub struct Dispatcher {
    registry: ControllerRegistry,
    handle: tokio::runtime::Handle,
    tx: crate::relay::IoSender,
}

impl Dispatcher {
    /// Bundles a registry, a runtime handle and a reply sender.
    #[must_use]
    pub fn new(
        registry: ControllerRegistry,
        handle: tokio::runtime::Handle,
        tx: crate::relay::IoSender,
    ) -> Self {
        Self {
            registry,
            handle,
            tx,
        }
    }

    /// The registered controllers, so the interpreter can seed one ambient
    /// handle per controller at mount and resolve `inject T as x`.
    #[must_use]
    pub fn registry(&self) -> &ControllerRegistry {
        &self.registry
    }

    /// Arms a timer that delivers a [`TimerTick`] for `continuation_id`
    /// (RFC-0029 §5), repeating every `dur_ms` when `every`, or once after it.
    ///
    /// Returns the handle whose drop **cancels** the timer. That is the whole
    /// leak story (INV-10): the effect that armed the timer owns the handle,
    /// and an effect that unmounts drops its state, so there is no separate
    /// "stop" path to forget to call and no way for a task to outlive the
    /// scope that started it.
    ///
    /// A `0 ms` interval is refused rather than armed: a zero-period
    /// `tokio::time::interval` fires as fast as the pool can send, which is a
    /// livelock dressed as a timer.
    #[cfg(feature = "runtime-io")]
    #[must_use]
    pub fn spawn_timer(
        &self,
        every: bool,
        dur_ms: u64,
        continuation_id: u64,
    ) -> Option<TimerHandle> {
        if dur_ms == 0 {
            return None;
        }
        let period = std::time::Duration::from_millis(dur_ms);
        let tx = self.tx.clone();
        let task = self.handle.spawn(async move {
            if every {
                let mut ticks = tokio::time::interval(period);
                // A stalled runtime must not produce a burst. Tokio's default
                // is `Burst`, which delivers every tick the stall swallowed as
                // fast as it can once things recover: a laptop closed for an
                // hour with `every 5min` running would wake to twelve
                // back-to-back refreshes, which for a polling timer means
                // twelve back-to-back requests. `Skip` resumes at the steady
                // cadence, which is what "every five minutes" means.
                ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                // The first tick of a Tokio interval completes immediately;
                // `every 5min` means "in five minutes", not "now and then every
                // five minutes", so the immediate one is consumed here.
                ticks.tick().await;
                loop {
                    ticks.tick().await;
                    if tx.send(Box::new(TimerTick { continuation_id })).is_err() {
                        // The logic thread is gone (shutdown): stop rather than
                        // spin sending into a closed channel.
                        break;
                    }
                }
            } else {
                tokio::time::sleep(period).await;
                let _ = tx.send(Box::new(TimerTick { continuation_id }));
            }
        });
        Some(TimerHandle {
            abort: task.abort_handle(),
        })
    }

    /// Schedules `method` on the controller at `id` and arranges for its
    /// result to arrive on the logic thread as a [`ControllerReply`] tagged
    /// with `continuation_id` (RFC-0028 §5 steps 2–3).
    ///
    /// Returns immediately: nothing here awaits, and the controller's own work
    /// runs entirely on the pool (INV-12). An unregistered `id` yields an
    /// **error reply** rather than silence, so the call site's `err` arm runs
    /// and the developer sees the mismatch instead of a call that vanished.
    pub fn spawn_call(
        &self,
        id: ControllerId,
        method: String,
        args: Vec<HostValue>,
        continuation_id: u64,
    ) {
        let Some(controller) = self.registry.get(id) else {
            // Delivered through the same channel as a real reply so the
            // failure is observed at the same place, and in the same tick
            // order, as a successful one.
            let _ = self.tx.send(Box::new(ControllerReply {
                continuation_id,
                result: Err(HostValue::Record(vec![
                    ("kind".into(), HostValue::Str("unregistered".into())),
                    (
                        "message".into(),
                        HostValue::Str(format!(
                            "no controller is registered for this handle (method `{method}`)"
                        )),
                    ),
                ])),
            }));
            return;
        };
        let tx = self.tx.clone();
        self.handle.spawn(async move {
            let result = controller.invoke(&method, args).await;
            // The relay may already be gone on shutdown; a dropped send is
            // correct then, there is no logic thread left to apply it.
            let _ = tx.send(Box::new(ControllerReply {
                continuation_id,
                result,
            }));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_value_round_trips_every_variant() {
        let values = [
            HostValue::Unit,
            HostValue::Bool(true),
            HostValue::Int(-7),
            HostValue::Float(2.5),
            HostValue::Str("hi".into()),
            HostValue::List(vec![HostValue::Int(1), HostValue::Int(2)]),
            HostValue::Record(vec![
                ("id".into(), HostValue::Int(3)),
                ("done".into(), HostValue::Bool(false)),
            ]),
        ];
        for v in values {
            assert_eq!(v.clone(), v);
        }
    }

    #[test]
    fn record_field_access() {
        let r = HostValue::Record(vec![("tempC".into(), HostValue::Int(21))]);
        assert_eq!(r.field("tempC"), Some(&HostValue::Int(21)));
        assert_eq!(r.field("missing"), None);
    }

    struct Counter;
    impl Controller for Counter {
        fn type_name(&self) -> &'static str {
            "Counter"
        }
        fn invoke(
            &self,
            method: &str,
            args: Vec<HostValue>,
        ) -> BoxFuture<'static, Result<HostValue, HostValue>> {
            let out = match (method, args.first()) {
                ("add", Some(HostValue::Int(n))) => Ok(HostValue::Int(n + 1)),
                _ => Err(HostValue::Str(format!("unknown method {method}"))),
            };
            Box::pin(async move { out })
        }
    }

    #[test]
    fn registry_insert_lookup_and_stable_ids() {
        let mut reg = ControllerRegistry::new();
        let id = reg.insert(Arc::new(Counter));
        assert_eq!(reg.id_of("Counter"), Some(id));
        assert!(reg.contains("Counter"));
        assert!(reg.get(id).is_some());
        assert_eq!(reg.names().collect::<Vec<_>>(), vec!["Counter"]);
        // Re-inserting keeps the id (last provider wins).
        let id2 = reg.insert(Arc::new(Counter));
        assert_eq!(id, id2);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn controller_invoke_dispatches_and_errors_on_unknown() {
        let c = Counter;
        let ok = pollster::block_on(c.invoke("add", vec![HostValue::Int(4)]));
        assert_eq!(ok, Ok(HostValue::Int(5)));
        let err = pollster::block_on(c.invoke("nope", vec![]));
        assert!(matches!(err, Err(HostValue::Str(_))));
    }
}
