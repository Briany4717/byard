# RFC-0049: The clock

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-05
- **Last updated:** 2026-11-05
- **Depends on:** RFC-0029 (async I/O, timers and persistence; §5 is the
  `every`/`after` effect this reads beside, and §7 reserves the `Timer` name),
  RFC-0028 (the controller boundary; its action-position rule is the one this
  borrows), RFC-0027 (data and collection operations; the closed method set and
  the "user data never panics" rule), RFC-0047 (text methods, whose `slice`
  `now()` is made to be read with).

> **Placement:** `docs/rfcs/0049-the-clock.md`

---

## Summary

`byld` gets two built-in functions, `now()` and `nowUtc()`. Each reads the
clock, synchronously, and answers ISO text with seconds: `"2026-11-05T14:03:12"`
for the local wall clock, `"2026-11-05T12:03:12Z"` for UTC. That is the shape
`formatDate` already reads, it sorts as text, and it compares with `<`.
`now().slice(0, 10)` is today's date.

They may be called only from an action: a button handler, `on mount`, an
`every` or `after` body, an `ok`/`err` arm, a named action `let`. A `let`, an
attribute, an interpolation, a `var`'s initialiser or a `fn` cannot call them,
and the checker says so with a new diagnostic, `ClockOutsideAction`, because a
projection that read the clock would show the time it was first computed and
never change. The interpreter reads the time through a clock it owns, so a test
or `byard shot --now` can fix it, and app time advances it. `formatDate` gains
the seconds token `ss`. Date arithmetic is out of scope.

## Motivation

`examples/inventory` stamps every stock movement with the time it happened.
`byld` has no clock, so the example wrote one in Rust:

```rust
#[byard_controller]
impl Stockroom {
    async fn now(&self) -> String { /* format the system time */ }
}
```

and every action that stamps something is written around the answer:

```byld
Button("Receive") => room.now() ok at => {
    movements = movements.push({ at: at, kind: "in", qty: qty })
}
```

The cost is not the Rust. It is that a clock read has become an asynchronous
call: the handler splits in two, the second half runs a tick later, a screen
reading `movements` in between sees the old list, and every test has to
`answer_all()` after each tap before it can look at anything. RFC-0028 built
that boundary for work that may take time and fail: a request, a file. Reading
the system clock does neither, and putting it behind the boundary charged it
for both.

Two other things are wrong in the same place. The guide's chapter on data and
I/O says the app reaches "the network, JSON, the clock and storage" through
"the four that come with Byard", listing `Timer`, and there is no `Timer`
capability to inject (`cap/` holds `Http`, `Json` and `Store`; RFC-0029 §7 only
reserves the name). The clock is promised and not delivered. And a screen that
shows the time cannot be built at all without a controller, however small.

## Guide-level explanation

```byld
View Clock() {
    var time = ""
    var log: List<Str> = []

    on mount => time = now()
    every 1s => time = now()

    Column {
        Text(time.formatDate("dddd D MMMM, HH:mm:ss"))
        Button("Note it") => log = log.push(now())
        Text(log.join("\n"))
    }
}
```

`now()` is the local date and time as text, `2026-11-05T14:03:12`: seconds, no
zone, always nineteen characters. `nowUtc()` is the same
instant in UTC with a `Z`: `2026-11-05T12:03:12Z`.

| Form | Result |
|---|---|
| `now()` | Local wall clock, `YYYY-MM-DDTHH:MM:SS` |
| `nowUtc()` | UTC, `YYYY-MM-DDTHH:MM:SSZ` |
| `now().slice(0, 10)` | Today, `2026-11-05` |
| `now().slice(11, 16)` | The minute, `14:03` |
| `a < b` for two of them | Earlier than; text order is time order |
| `now().formatDate("ddd D MMM")` | `Thu 5 Nov` |

`now()` reads the clock **once, when an action runs**. It is not a value that
stays current. This is the one rule to hold, and it is why the show-the-time
pattern is a `var` an action writes:

```byld
var time = ""
every 1s => time = now()
```

Writing it where a value is expected is an error, not a clock that never ticks:

```byld
let today = now().slice(0, 10)   // ClockOutsideAction
Text("{now()}")                  // ClockOutsideAction
fn stamp() => now()              // ClockOutsideAction
```

> `now()` reads the clock once, when an action runs; to show the time, write it
> to a `var` in an action, such as `every 1s => time = now()`.

Where it may appear, and where it may not:

| Position | `now()` |
|---|---|
| A button or event handler | yes |
| `on mount`, `on unmount`, `on change` | yes |
| An `every` or `after` body | yes |
| An `ok` or `err` arm of a controller call | yes |
| A named action: `let stamp = () => { at = now() }` | yes, where it is called from an action |
| A `let`, an attribute, a text interpolation | `ClockOutsideAction` |
| A `var`'s initialiser | `ClockOutsideAction` (write `on mount => t = now()`) |
| A `fn`, view-level or top-level | `ClockOutsideAction` |

A named action is inlined where it is called, so it follows its caller: the
same `stamp()` is fine in a handler and an error in a `let`.

### In a test, and in a shot

The clock is the system's until a host fixes it. `byard shot --now
2026-11-05T09:00:00` draws the project as if that were the time at the start,
and `--at` still advances it, so `--now 2026-11-05T09:00:00 --at 3000` is
`09:00:03` when the picture is taken:

```
byard shot crates/byard-cli/examples/timers --now 2026-11-05T09:00:00 -o clock.png
```

A test does the same with `Interpreter::set_wall_clock("2026-11-05T09:00:00")`,
and moves it forward by advancing app time with `set_now_ms`, the call a test
already makes every frame. A shot or test that does not fix the clock sees the
real one and cannot assert on it, which is the same position every other
program is in.

## Reference-level explanation

### 1. The functions

`now()` and `nowUtc()` take no arguments and answer `Str`. They are names of
the language, like `untrack`: a program that binds `now` as a `var`, a `let` or
a `fn` of its own gets its own, because the lookup finds that first and the
built-in is the last resort. The checker knows both (`BUILT_IN_CALLS`), types
them `Str`, and so `now().slice(0, 10)` and `now() < deadline` check as text.

The text is `YYYY-MM-DDTHH:MM:SS` for the local clock and
`YYYY-MM-DDTHH:MM:SSZ` for UTC. Always the same width, nineteen characters and a twentieth for the `Z`:
four-digit year, two-digit everything else, no fraction, no offset on the local
form. Fixed width is what makes the text sort as time (the argument for ISO 8601
in the first place), and it is the width `slice(0, 10)` and `slice(11, 16)`
rely on. Years beyond 9999 and before 0 are not representable and are not
reachable: the clock is the system's.

Local means the operating system's zone for the process, with the daylight
rule in force at that instant, as the platform reports it. The offset is not
written, because a local time with an offset is a different thing to compare:
`2026-11-05T14:03:12+02:00 < 2026-11-05T13:00:00Z` is false by text and true by
instant. Two values from `now()` compare correctly with each other; one from
`now()` and one from `nowUtc()` do not, and the guide says to not mix them.
During the hour a daylight change repeats, two `now()` values can sort in the
wrong order. That is the cost of a local time with no offset, and the
alternative that avoids it, `nowUtc()`, is there for a sort key that must never
go backwards.

### 2. `formatDate` and the `Z`

`formatDate` already reads this text (`interp/dates.rs`):
`YYYY-MM-DD`, optionally followed by `T` or a space and `HH:MM`, and ignores
whatever follows the minute. So a trailing `Z`, `+02:00` or a fraction is
accepted, with no change. This RFC makes two changes to it:

- **The seconds are read.** After `HH:MM`, a `:` and two digits `00` to `59`
  are the second. Anything else after the minute is still ignored. A `:` that
  is not followed by a valid second makes the text not a date, as `25:00` does.
- **A token for them, `ss`**, the two-digit second (`05`). `formatDate` had
  `HH`, `H` and `mm` and no way to show the seconds that `now()` now supplies.
  It is `ss` and not `s`, because `mm` is the only minute token and the two
  should pair; a one-digit second is not a thing a clock shows.

The zone is not applied. `formatDate` writes the fields as given: a `Z` text
formatted with `HH:mm` shows the UTC hours. That is already what it does and it
is the right default, since converting needs a zone and the language has none.

### 3. Where it may be called

The rule is RFC-0028 §4's, and it is enforced the same way: the interpreter
counts how deep a lowering is inside an action (`action_depth`, raised by
`lower_action` and by a bare statement member), and a `now()` lowered at depth
zero is `ClockOutsideAction { span, name }`. Everything a value is needed for,
an attribute, a `let`, an interpolation, a `var` seed, goes through the lowering
at depth zero, so one test rejects them all and no position can be forgotten.
Controller `ok` and `err` arms are lowered inside the call, at action depth, and
are allowed with no further rule.

Functions are the exception that needs its own rule. A named action and a `fn`
are both inlined at their call, so a `fn` called from a handler would be lowered
at action depth and let `now()` through, in the one place the language promises
purity. The interpreter therefore marks a `fn` (view-level with parameters, and
top-level) as pure for the lowering of its body, and `now()` inside it is
`ClockOutsideAction` whatever the caller is. A view-level `fn` with no
parameters is a memo and is rejected by depth. Arguments are lowered in the
caller, so `stamp(now())` from a handler is fine. A named action
(`let stamp = () => { ... }`, which is not a `fn`) is lowered at its caller's
depth, as in the table above.

The diagnostic's message is the sentence quoted in the guide-level section. It
names the repair, because the mistake is natural (the author wants the time
shown) and the fix is not obvious (an action writing a `var`).

### 4. The clock the interpreter owns

The interpreter holds one clock, shared by every lowered `now()`. It has two
states:

- **System**, the default. `now()` reads the platform's local time, `nowUtc()`
  the platform's UTC. Nothing else is involved.
- **Fixed**, set by a host with `Interpreter::set_wall_clock(text)`. The text
  is the same ISO text, with or without seconds or a `T`; one that is not a
  date is an error the setter returns. The fixed time is anchored at the app
  time at the moment it is set. From then on, the clock reads the fixed time
  plus the app time elapsed since: `set_now_ms` (which the runner already calls
  once per frame, which `byard shot --at` already advances, and which every
  animation test already sets) moves it. No second clock, no second thing to
  keep in step.

A fixed clock has no zone: `nowUtc()` is the same text as `now()` with the
`Z`. A test that fixed the local time has told the interpreter nothing about
an offset, and reading the machine's would make the same test pass in one
country and fail in another. It is a stated simplification of the fixed clock
only.

The relation to `every`/`after` is stated here, not hidden. Those timers do not
run on app time: they are Tokio timers (RFC-0029 §5) driven by the real
reactor, and a headless shot has no dispatcher, so none of them fires there.
App time and those timers meet only at the clock: a test that has an `every`
running and sets `set_now_ms` forward sees the next tick write the advanced
time, and `byard shot` shows what `on mount` wrote at the fixed time and what a
`--tap` writes at the app time of the tap. This RFC does not change how timers
are driven (Unresolved).

### 5. Cost

`now()` reads the clock only when called, in the one call. There is no timer, no
thread, no subscription and no change to the tick: a program that never calls
it pays one field in the interpreter, and a program that calls it pays one
system call per call. `every 1s => time = now()` is a once-a-second wake that
the author asked for with the `every`, as before; `now()` adds nothing to it.

### 6. The CLI

`byard shot` gains `--now <ISO>`, which calls `set_wall_clock` before the first
frame. A malformed value is an error before anything is drawn, with the
accepted shape in the message. Without it the shot reads the real clock. The
flag is shown in `cli-reference.md` beside `--at`, which it composes with.

### 7. The `Timer` name

`Timer` stays in `RESERVED_NAMES`. The reservation costs nothing, keeps an app
from providing a `Timer` that would read like the `every` line, and removing
it would let an app take the name and later collide with a capability that
might come. What changes is the guide, which no longer says four capabilities
come with Byard. Three do, `Http`, `Json` and `Store`; timers and the clock are
part of the language and need no `inject`. The reserved-names paragraph says
`Timer` is reserved and is not a capability.

## Drawbacks

- **A function in a language of methods.** `now()` is a free function where
  every other helper is a method on its value (`slice`, `formatDate`). There is
  no receiver for a clock. `untrack` is the precedent for a free built-in, and
  the checker knows both.
- **An error where JavaScript has a value.** `new Date()` works in a template.
  The rule here costs an author one line (a `var`, an `every`) and buys a
  diagnostic where the other choice buys a screen that shows the time it
  launched at.
- **Local time with no offset** cannot say which of two repeated hours a time
  is in, and compares wrongly with a UTC string. Both are documented; neither
  is hit by a log of events an app shows in order, which is the common use.
- **Fixed and system clocks differ in zone**, as §4 says. A fixed clock is
  deterministic by being less like the real one.
- **A new dependency for the local zone.** Reading it needs a system call that
  `unsafe_code = "deny"` keeps out of this workspace's own code, so the
  interpreter depends on `chrono` with its `clock` feature only, and nothing
  else of it.

## Rationale and alternatives

- **A `Clock` capability, `inject Clock as clock; clock.now()`.** The shape the
  inventory example used. A capability exists for work that happens off the
  logic thread and answers later, or can fail (RFC-0028 §1). A clock read does
  neither: it is instant, total, and the same in every program. Injecting it
  buys a handle to write and an `ok at => { … }` to unwrap for an answer that
  was never late, and it is the thing this RFC removes. There is also nothing
  to inject: a capability is Rust behind the boundary, and a read of the system
  time needs no state. For tests, a capability is swapped at the registry; the
  interpreter's clock is swapped with one call and with no controller at all.
- **A `Timer` capability.** What the guide promised. An `every` line already is
  the timer and is better than a handle at it (RFC-0029 §5: it is scoped and
  cancels itself). The clock and the timer are separate needs and the second is
  met.
- **`now` as a value that is reactive and ticks.** `Text(now())` showing a live
  clock is what a reader would try first. It would need a timer the language
  starts on the author's behalf, at a rate the author did not choose, in every
  view that mentions it, which breaks "no timer, no thread" for the apps that do
  not want one and picks a rate (a second?) that is wrong for half of the rest.
  `every 1s => time = now()` is one line, states the rate, and is where the
  cost is visible.
- **Allowing it in a `let` and documenting that it is read once.** A `let`
  re-evaluates when what it reads changes, and `now()` reads nothing, so it
  would be computed at first use and then never again: right on one run and
  stale on the next, and silent. The diagnostic exists so that cannot happen.
- **Also rejecting a `var` initialiser** is a stricter rule than strictly needed,
  since a seed runs once. It is kept because the rule is then one sentence
  ("only in actions"), and `on mount => t = now()` is the same length.
- **A number: epoch milliseconds.** Cheap to compare and to subtract, and the
  right answer for a duration. It does not display, does not read, and does not
  sort in a JSON file a human opens. The text already feeds `formatDate`.
- **A record, `{ year, month, day, hour, … }`.** Fields without a sort order
  and without a way to be compared; `now() < deadline` would need a function.
- **Fixing the clock by a capability for tests only.** Two ways to read time,
  one for tests, is how a test passes and the app fails. The same lowered code
  reads the same clock object, and the host decides which.

## Prior art

- **JavaScript** `new Date().toISOString()` is UTC with a `Z` and milliseconds;
  local time has no ISO method, and `toLocaleString` is a different shape per
  machine. This RFC makes local the default and ISO for both.
- **Elm** keeps the clock out of pure code (`Time.now` is a command, answered
  by a message), for the reason given here, and pays the asynchronous round
  trip that this RFC avoids by not having the pure/effect boundary at the clock.
- **Rust's `chrono` and `time`** read the zone by a platform call and keep a
  `NaiveDateTime` for a time with no zone; the text here is that type's.
- **Test clocks** (`tokio::time::pause`, `jest.useFakeTimers`) fix and advance
  time for a test. The interpreter's clock is anchored to app time for the same
  reason those exist: a time that a test sets and the harness's own frame
  clock moves.

## Resolved questions

### Q1, synchronous or an answer that arrives later?

**Question.** The inventory example asks Rust for the time and is answered a
tick later.

**Options.** (a) Keep the controller; (b) a capability with an `ok` arm; (c) a
built-in that answers at once.

**Resolution: (c).** A clock read is neither slow nor fallible, so it owes
nothing to the boundary built for work that is. The cost of (a) and (b) is the
shape of every action that stamps something and every test of one.

### Q2, a capability or a built-in?

**Question.** Is the clock a service to `inject`?

**Options.** (a) A `Clock` capability; (b) a built-in function.

**Resolution: (b).** There is no async answer to deliver and no state to hold:
the same system call returns the same kind of answer in every program, so there
is nothing for a program to choose between by injecting one. Tests swap the
interpreter's clock without a registry. This is also why `Timer`, the name
RFC-0029 reserved, is not a capability and the guide stops saying it is.

### Q3, where may it be called?

**Question.** A value position that reads the clock never updates.

**Options.** (a) Anywhere, documented as read-once; (b) actions only, with a
diagnostic; (c) anywhere, and reactive to a timer.

**Resolution: (b).** (a) fails silently and (c) starts timers nobody wrote.
The diagnostic costs a sentence and names the repair. `fn` is included because
it is declared pure, whatever calls it.

### Q4, in what shape, with what zone?

**Question.** What text does `now()` give, and which zone is it in?

**Options.** (a) ISO local with seconds and no offset; (b) ISO with an offset;
(c) UTC only.

**Resolution: (a), with `nowUtc()` as the UTC form.** It is what `formatDate`
reads, it has a fixed width, it sorts and compares. The offset is dropped
because an app shows local times and compares them with each other. `nowUtc()`
is there for a key that must not move with the zone.

### Q5, does `formatDate` get seconds?

**Question.** `now()` now has seconds that `formatDate` could not show.

**Options.** (a) No; (b) `ss`; (c) `ss` and `s`.

**Resolution: (b).** The two-digit form pairs with `mm`. A bare `s` has no use
on a clock.

### Q6, how does a test control the time?

**Question.** A test needs to fix `now()` and to move it.

**Options.** (a) A separate clock with its own `advance`; (b) one that is fixed
once and follows app time.

**Resolution: (b).** App time is already set every frame by every host, it is
what `--at` advances, and a second clock would be a second thing to forget to
move. A fixed instant plus app time elapsed is a deterministic clock with no
extra API beyond the setter.

## Unresolved questions

### Date arithmetic

Days between two dates, a day added to one, "is this before the first of the
month", are all out of scope. They want a type or a family of functions, and
the shape is not settled. The sketch that fits what is here is text in, text
out, as `formatDate` is: methods on a date text, total, returning nothing for
text that is not a date.

```byld
let due = "2026-11-05T09:00:00"
Text("{due.addDays(7).formatDate("D MMM")}")        // 12 Nov
Text("{due.daysUntil(now())} days")                  // whole days from due to now
Text("{due.startOfMonth()}")                         // "2026-11-01T00:00:00"
```

`addDays`, `daysUntil` and `startOfMonth` would join `STR_METHODS` like
`formatDate`. Hours and minutes need the same family, and the daylight rule
needs a zone the language does not have: until it does, arithmetic would be on
the wall-clock fields, as `formatDate` is. That is the decision a separate RFC
has to make, and why it is not smuggled in here.

### Timers on app time

`every` and `after` run on Tokio's reactor, so a headless shot cannot show a
tick and a test must wait in real time. Driving them from app time would make
`byard shot --at 3000` show what `every 1s` wrote three times. That is a change
to RFC-0029 §5, not to the clock, and it is not needed for this RFC.

## Future possibilities

- Date arithmetic, as sketched above.
- A zone: `now()` in a named zone, and a fixed clock that can say which.
- Timers on app time, so a shot shows the effect of the `every` lines.
- `startOfDay`, `weekday`, and a duration type, once arithmetic exists.
