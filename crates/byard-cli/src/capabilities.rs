//! The capability set `byard dev` provides to every `.byd` file it runs
//! (RFC-0029 §7).
//!
//! A capability is an ordinary controller (RFC-0028 §2) that the framework
//! happens to ship, so `inject Http as http` inside a view resolves exactly
//! the way `inject WeatherApi as api` resolves in an app that wrote its own.
//! Providing them by default is the DX floor RFC-0029 argues for: a weather
//! app should work from `byard new` without the developer wiring an HTTP
//! client by hand.
//!
//! An app that wants its own client simply does not `inject` this one; the
//! built-ins are non-privileged and hold no position an app controller cannot
//! also take (`App::provide` replaces one of the same name).

use byard_core::bridge::ControllerRegistry;

/// The registry `byard dev` hands to the interpreter.
///
/// Empty of app controllers by design: `byard dev` runs a `.byd` file, and a
/// file is not a crate, so there is no Rust half for it to register. What it
/// can offer is the framework's own capability set, which is the whole reason
/// RFC-0029 makes those first-party.
#[must_use]
pub fn registry() -> ControllerRegistry {
    ControllerRegistry::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_registry_reserves_no_app_names() {
        // Every name here becomes unavailable to an app controller
        // (RFC-0029 §7 reserved names), so the set is asserted rather than
        // assumed: a capability added without a decision would silently take
        // a name out of the app's vocabulary.
        let names: Vec<&str> = registry().names().collect();
        assert!(
            names.is_empty(),
            "unexpected default capabilities: {names:?}"
        );
    }
}
