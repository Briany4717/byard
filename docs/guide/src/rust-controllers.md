# Controllers

You reach for Rust when byld does not do what you need: a database, a file the app
owns, a device, a library with no byld equivalent. A *controller* is a Rust struct a
view can call. This chapter builds one, calls it from a view, and covers what crosses
between them.

The `byard` crate is what an app depends on. It is not on crates.io yet, so depend on it
by path to your clone. Controllers are `async`, so an app that sleeps or does I/O in
one also wants `tokio`:

```toml
[package]
name = "greeter"
version = "0.1.0"
edition = "2024"

[dependencies]
byard = { path = "../byard/crates/byard" }
tokio = { version = "1", features = ["time"] }
```

## A controller

```rust,ignore,excerpt
use byard::bridge::HostValue;

/// A greeter that takes its time, standing in for a network round trip.
#[byard::byard_controller]
#[derive(Clone)]
struct Greeter {
    /// Prepended to every greeting, so the view can show that the *controller*
    /// produced the answer and not the view itself.
    prefix: String,
}

#[byard::byard_controller]
impl Greeter {
    /// Greets `name` after a deliberate delay.
    async fn greet(&self, name: String) -> Result<HostValue, HostValue> {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if name.trim().is_empty() {
            return Err(HostValue::Record(vec![
                ("kind".into(), HostValue::Str("empty_name".into())),
                ("message".into(), HostValue::Str("type a name first".into())),
            ]));
        }
        Ok(HostValue::Record(vec![
            (
                "text".into(),
                HostValue::Str(format!("{}, {name}!", self.prefix)),
            ),
            (
                "length".into(),
                HostValue::Int(i64::try_from(name.chars().count()).unwrap_or(i64::MAX)),
            ),
        ]))
    }
}
```

That is the whole controller. Two things to notice.

`#[byard::byard_controller]` goes on the struct, and again on its `impl`. On the struct
it records the field names. On the `impl` it generates the glue that lets a view call
each `async fn` by name. The struct must be `Clone`, because each call runs on its own
copy.

Every `async fn` in that `impl` becomes a method a view can call. A method that is not
`async` is not exposed. A method that returns `Result<T, E>` has its two sides become the
view's `ok` and `err` arms, and one that returns anything else always succeeds.

## Registering it

The app gives its controllers to the engine when it starts:

```rust,ignore,excerpt
fn main() -> Result<(), byard::ByardError> {
    byard::App::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.byd"))
        .title("Byard, controller demo")
        .provide(Greeter {
            prefix: "Hello".to_string(),
        })
        .run()
}
```

`provide` registers one controller. The name a view uses is the struct's name. The rest
of `App` is in [the next chapters](rust-app.md).

## Calling it from a view

```byld,ignore
// `Greeter` is Rust, so `byard check` cannot see it. It warns, and the warning is expected.
View Main() {
    inject Greeter as greeter

    var name: Str = "world"
    var answer: Str = ""
    var detail: Str = ""

    Column #[bg: 0x14141C, p: 32, gap: 20, width: 640] {
        TextField #[value: name, placeholder: "who to greet…", grow: 1]
        Button("Greet") #[bg: 0x477A96, radius: 10, color: 0xFFFFFF]
            => {
                answer = ""
                greeter.greet(name)
                    ok reply => {
                        answer = reply.text
                        detail = "the controller counted {reply.length} characters"
                    }
                    err e => {
                        answer = e.message
                        detail = "kind: {e.kind}"
                    }
            }
        Text("{answer}") #[color: 0xFFFFFF, size: 20]
        Text("{detail}") #[color: 0x9AA0AC, size: 13]
    }
}
```

`inject Greeter as greeter` resolves the controller the app registered, and
`greeter.greet(name)` calls it. The call does not block. It converts its arguments, hands
them to the thread pool and returns, so the window keeps drawing and dragging for the
half second the controller takes. The reply comes back on the logic thread and runs
`ok` or `err`, which write `var`s, and the screen follows.

The sample is not checked as a whole, because `byard check` cannot see Rust. What it does
is tell you so:

```byld,fails
View Main() {
    inject Greeter as greeter
    var answer = ""
    Button("Greet") => greeter.greet("Ada")
        ok reply => { answer = reply.text }
    Text(answer)
}
```

```diagnostic
warning[UncheckableInject]: `Greeter` is not an ambient value here
```

A warning does not fail the check. It says the calls through `greeter` could not be
verified, because they depend on Rust the checker has not linked. A call to a capability
the engine provides, `Http`, `Json` and `Store`, is checked, and a misspelt method is an
error. A call to your own controller is checked when the app runs.

## What crosses

Arguments and replies cross as a `HostValue`, which owns only plain data and can be sent
between threads:

```rust,ignore,excerpt
pub enum HostValue {
    Unit,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<HostValue>),
    Record(Vec<(String, HostValue)>),
    Bytes(Vec<u8>),
}
```

A byld number becomes `Int` or `Float`, text becomes `Str`, a list becomes `List`, a record
becomes `Record`, and a tuple becomes a `List`. A `Record` arrives in a view as a record,
so `reply.text` reads its `text` field. `Bytes` is for pictures and file contents given to
a native view, and byld reads it as nothing.

An `async fn`'s parameters are converted for you. The types that convert are `bool`,
`String`, `f64`, `f32`, `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `(f32, f32)`, `Vec<T>` of any of them, `()`, and
`HostValue` itself, which takes whatever was sent. The same types convert back out as
return values. Conversion is lenient: a value of the wrong shape gives the type's empty
value (`0`, `""`, an empty list) and never a panic, because the arguments are user data.

Reply with a record when a view needs several things, built from `HostValue::Record`. The
fields `kind` and `message` are what a view expects of an error, which is what the
engine's own capabilities send.

## Rules of the boundary

- **Never block.** A controller runs on the async pool. Sleep with `tokio::time::sleep`,
  never `std::thread::sleep`, and keep long synchronous work out of it. Blocking a worker
  is a smaller version of the mistake of blocking the window.
- **A reply belongs to the logic thread.** The `ok` and `err` arms run there, so they may
  write any `var` of the view that made the call.
- **Calling an unknown method is an error reply**, never a panic.
- **Four names are taken.** `Http`, `Json`, `Store` and `Timer` are reserved for the
  engine's own capabilities. `App::provide` rejects a controller named one of them and
  `run` fails saying so. Name yours something else.

## When a controller is the wrong tool

If the work is a request to a server that sends JSON, `Http` already does it, with no
Rust. Reach for a controller when you need a library, a secret that must not live in a
`.byd` file, or a result computed off the logic thread. The same boundary is how a
[native view](rust-native-views.md) asks for data.
