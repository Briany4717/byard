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
/// RFC-0029 makes those first-party, and the reason a pure-`byld` example can
/// fetch from the network at all.
///
/// The set comes from `byard-core`, not from a list here, so the dev runner
/// and a shipped `App` cannot end up offering different capabilities. An app
/// that behaves one way under `byard dev` and another way when shipped is the
/// single most expensive difference a framework can have.
///
/// `project` is the manifest's project name, which is what decides where the
/// `Store` capability writes (RFC-0029 O5). Keyed on the project rather than
/// on the path, so a store does not move when the directory does, and two
/// projects never share one settings file.
#[must_use]
pub fn registry(project: &str) -> ControllerRegistry {
    byard_core::cap::default_registry(project)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_capability_the_dev_runner_offers_is_a_reserved_name() {
        // Every name here becomes unavailable to an app controller
        // (RFC-0029 §7 reserved names), so the set is asserted rather than
        // assumed: a capability added without a decision would silently take
        // a name out of the app's vocabulary.
        for name in registry("demo").names() {
            assert!(
                byard_core::cap::is_reserved(name),
                "`{name}` is offered but not reserved"
            );
        }
    }

    #[test]
    fn the_dev_runner_offers_the_same_set_a_shipped_app_does() {
        let dev: Vec<&str> = registry("demo").names().collect();
        let shipped: Vec<&str> = byard_core::cap::default_registry("demo").names().collect();
        assert_eq!(dev, shipped);
    }
}
