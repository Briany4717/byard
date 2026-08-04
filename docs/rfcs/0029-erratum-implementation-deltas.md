# Erratum to RFC-0029: six things building it corrected

- **Status:** Active erratum (amends, does not replace, RFC-0029)
- **Author(s):** Briany4717
- **Created:** 2026-08-04
- **Applies to:** RFC-0029 §2 (what the idle gate has to carry), §3 (the
  response shape and the client's lifetime), §5 (the first tick, and where a
  timer's lifetime comes from), §6 (`get`'s missing-key answer, and the scope
  of the write lock), §7 (what a reserved name means to `App::provide`), and
  RFC-0028 §9's treatment of an `inject` a static check cannot resolve.
- **Authority:** the implementation, and the tests named against each
  correction below.

---

RFC-0029 was **Draft** while this work landed, so its body has been corrected
in place. This erratum exists because a reader who only has the RFC would not
otherwise learn which of its sentences were wrong, and each of these was wrong
in a way that cost something to find.

---

## 1. A condvar is not enough; the gate needs a flag (§2)

§2 says to "replace the `IDLE_PARK` spin/park with a condvar signalled by the
I/O sender and timer driver". A bare condvar loses every wakeup that arrives
while the logic thread is still working, which on a busy tick is most of them:
the thread finishes, parks, and sleeps out the full fallback with a result
already queued.

**Correction.** The gate carries a `pending` flag as well as the condvar. A
signal sets the flag; a park that finds it set returns immediately and clears
it. The timeout stays, but only as a lost-signal backstop, not as the delivery
latency.

Covered by `a_result_queued_while_the_logic_thread_was_busy_does_not_sleep_through_the_park`
and `parking_ends_the_moment_a_result_is_sent`.

## 2. The waker needs both directions, not one (§2)

§2 says a tick that applied an I/O result "counts as changed" and wakes the
renderer. Half of that is right. The other half, that a result which changed
*nothing* must not cost a repaint, is not stated, and a naive reading of the
sentence makes every reply, including one whose arm nobody wrote, wake a
`Wait`-mode loop.

**Correction.** `LogicRuntime::apply_io_results` returns whether anything was
applied, and the relay wakes on that answer. Both directions are tested:
`an_applied_result_wakes_a_wait_mode_render_loop_with_no_input_at_all` and
`a_result_that_changed_nothing_does_not_cost_a_repaint`.

## 3. The HTTP client is per process, not per call (§3)

§3 describes `Http` as a `Controller` whose methods build a `reqwest::Request`,
which reads as though a client is at hand. It never says where the client comes
from, and the obvious implementation — one per call — re-runs the TLS handshake
on every request and turns a 40 ms refresh into a 300 ms one.

**Correction.** One `reqwest::Client` in a `OnceLock`, shared by every `Http`
value. It owns the connection pool and the TLS session cache, so two `Http`s
(the default one and an app's `with_base_url`) share sockets rather than
competing for them.

The response record also gained an `ok: Bool` field alongside `status`, which
§3 does not list: `res.ok` is what a view actually wants to branch on, and
deriving it from `status` at every call site is arithmetic the language should
not ask for.

## 4. `every`'s first tick has to be thrown away (§5)

§5 says `every` "uses `tokio::time::interval`". Taken literally that is wrong in
a way that only shows up in production: a Tokio interval completes its **first**
tick immediately, so `every 5min` would fire once at mount and then every five
minutes. For a refresh that means one unwanted request at every screen mount,
which is exactly the request an app is trying not to make.

**Correction.** The driver consumes the immediate tick. `every 5min` means "in
five minutes". Covered by `the_first_tick_of_every_waits_out_its_interval`.

§5 is also silent on `every 0ms`, which arms an interval that fires as fast as
the pool can send — a livelock dressed as a timer. A zero interval is now
refused (`a_zero_interval_is_refused_rather_than_armed`).

And where §5 says "unmount cancels the Tokio task", the implementation makes
that structural rather than procedural: `TimerHandle`'s `Drop` aborts the task,
and the effect slot owns the handle, so there is no cancel path to forget to
call. The leak this prevents is the one with no symptom — a closed screen that
goes on polling — so it should not depend on remembering.

## 5. The store's write lock has to span the write (§6)

§6 says "concurrent `set`s are serialized by an async `Mutex` inside `Store`",
which is true of the *cache* and not of the *file*. Holding the lock only for
the mutation lets two writes interleave: both write the same temporary file,
the first rename moves it away, and the second fails with `ENOENT`. That is a
lost write and a storage error for an ordinary pair of calls.

**Correction.** The lock is held across the `spawn_blocking` that writes, which
is why it must be an async `Mutex` and could not be a `std` one.
`concurrent_writers_all_land` fires eight concurrent `set`s and asserts every
key survives; it failed before this and passes after.

§6's `store.get(key) -> value | Unit` also gained a two-argument form,
`get(key, default)`. Every app hits a missing key on its first run, and the
shape almost all of them want is a default; without one, `ok saved => { todos =
saved }` assigns `Unit` to a `List` var and the symptom is a count that renders
blank. A missing key is still never an error.

## 6. A reserved name is a rejection, not a shadow (§7)

§7 says a user controller sharing a reserved name is a
`CompileError::ReservedControllerName`, which places the failure in the
**compiler**. The compiler never sees an app's Rust types; the registry does.
Leaving it there would mean the rule is stated and never enforced.

**Correction.** `App::provide` rejects the controller and `run()` fails naming
it. The reserved list is also unconditional rather than per feature: a name has
to mean the same thing in every build, or an app compiled with
`--no-default-features` that defined its own `Http` would break the moment
someone re-enabled the feature, and the failure would look like a dependency
problem rather than a collision.

## 7. What a static check cannot know about `inject` (RFC-0028 §9)

RFC-0028 §9 lists `UnresolvedInject` as the diagnostic for a missing
controller. That is right where a registry exists to contradict the name, and
wrong everywhere else: a two-layer app registers its controllers at run time, so
`byard check` on a correct app would fail on every `inject` of an app
controller.

**Correction.** The answer depends on whether anyone *could* have known. With a
registry wired in, an unresolved `inject` is an error. Without one, it is a
warning (`UncheckableInject`) and the binding is still made, as an unbound
handle, so the calls on it are lowered and checked normally. If an unbound
handle is ever actually called, the dispatcher answers with an `unregistered`
error reply, so the failure surfaces at the call site's `err` arm instead of
becoming a call that quietly never returns.

The framework's own capability names *are* knowable statically, so `byard check`
declares them and checks calls on `Http`, `Json` and `Store` properly. It uses
the reserved-name list rather than building a registry, because a checker must
not resolve a data directory or open a file the app owns.

---

## Unchanged, and worth saying so

Everything else in RFC-0029 survived contact with the implementation as
written: the `runtime-io`/`net`/`json`/`storage` feature split and its
dependency rules, rustls with bundled roots, JSON key-order preservation,
integral-vs-fractional number mapping, a non-2xx going to the `err` arm,
delivering timer ticks through the RFC-0028 apply path rather than a bespoke
channel, and single-file JSON key/value persistence with an atomic temp+rename
write.
