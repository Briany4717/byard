//! The Rust half of a two-layer Byard app (RFC-0028).
//!
//! Run it from the repository root:
//!
//! ```text
//! cargo run -p controller-demo
//! ```
//!
//! Press the button. The view calls a Rust method that deliberately takes half
//! a second, and the window stays responsive the whole time: the spinner text
//! keeps its layout, the window keeps dragging, and the answer appears when it
//! arrives. That responsiveness is the thing being demonstrated, the work runs
//! on the Tokio pool and only its `HostValue` result crosses back to the logic
//! thread (INV-12, INV-2).
//!
//! Ask for `""` and the controller returns an `Err`, so the `err` arm runs
//! instead. Both arms are ordinary actions: they write a `var`, and the normal
//! reactive path repaints.

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
    ///
    /// `Err` for an empty name: the point is that a failure has its own arm at
    /// the call site, not that greeting nobody is especially dangerous.
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

fn main() -> Result<(), byard::ByardError> {
    byard::App::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.byd"))
        .title("Byard, controller demo")
        .size(720, 480)
        .provide(Greeter {
            prefix: "Hello".to_string(),
        })
        .run()
}
