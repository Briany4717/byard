//! The framework's own capabilities (RFC-0029).
//!
//! A capability is an ordinary [`Controller`](crate::bridge::Controller) that
//! Byard happens to ship, so `inject Http as http` inside a view resolves
//! exactly the way `inject WeatherApi as api` resolves in an app that wrote
//! its own. Nothing here holds a privileged position: the engine `provide`s
//! them by default, and an app that wants a different HTTP client simply never
//! injects this one.
//!
//! Each is behind its own Cargo feature, the `telemetry`/`image` precedent, so
//! an app pays for exactly what it uses. Everything degrades cleanly: no
//! feature, no capability, no dependency.
//!
//! | Feature | Default | Gives |
//! |---|---|---|
//! | `json` | on | [`Json`], and the `.json` field on an HTTP response |
//! | `net` | on | [`Http`] (reqwest + rustls) |

#[cfg(feature = "net")]
pub mod http;
#[cfg(feature = "json")]
pub mod json;

#[cfg(feature = "net")]
pub use http::Http;
#[cfg(feature = "json")]
pub use json::Json;

/// The type names the framework reserves for its own capabilities
/// (RFC-0029 §7).
///
/// Listed unconditionally, not per feature. A name has to mean the same thing
/// in every build, otherwise an app that compiles with `--no-default-features`
/// and defines its own `Http` would break the moment someone turned the
/// feature back on, and the failure would look like a dependency problem
/// rather than a name collision.
pub const RESERVED_NAMES: &[&str] = &["Http", "Json", "Store", "Timer"];

/// The capability names this build actually **provides**, as opposed to those
/// it merely reserves (RFC-0029 §7, §8).
///
/// The two lists differ, and the difference matters to anything that reasons
/// about a name without wanting to instantiate the thing behind it. `Timer` is
/// reserved but has no controller (a timer is a language effect, not an
/// injectable service), and a build with `--no-default-features` reserves every
/// name while providing none.
///
/// Exists so a caller can tell "this is checkable" from "this is merely spoken
/// for" without building a registry.
#[must_use]
pub fn provided_names() -> Vec<&'static str> {
    // Written as a filter over the reserved list rather than as a list of its
    // own, so a name can never appear here without being reserved.
    RESERVED_NAMES
        .iter()
        .copied()
        .filter(|name| match *name {
            "Json" => cfg!(feature = "json"),
            "Http" => cfg!(feature = "net"),
            // `Timer` is reserved because `every`/`after` own the word; there
            // is no controller behind it and never will be.
            _ => false,
        })
        .collect()
}

/// Whether `name` is reserved for a built-in capability.
#[must_use]
pub fn is_reserved(name: &str) -> bool {
    RESERVED_NAMES.contains(&name)
}

/// The capabilities enabled in this build, ready to register (RFC-0029 §7).
///
/// Built here rather than at each host so `byard dev` and a shipped `App`
/// cannot end up offering different sets, which would make an app behave one
/// way under the dev runner and another way when shipped, the single most
/// expensive kind of difference a framework can have.
#[must_use]
pub fn default_registry() -> crate::bridge::ControllerRegistry {
    #[allow(unused_mut)]
    let mut registry = crate::bridge::ControllerRegistry::new();
    #[cfg(feature = "json")]
    registry.insert(std::sync::Arc::new(Json));
    #[cfg(feature = "net")]
    registry.insert(std::sync::Arc::new(Http::new()));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provided_names_are_exactly_what_the_registry_holds() {
        // Two lists that must describe the same set, maintained separately
        // because one of them has to be answerable without instantiating a
        // controller. As *sets*: the registry's order is its `ControllerId`
        // assignment and is nobody else's business.
        let mut registered: Vec<&str> = default_registry().names().collect();
        let mut provided = provided_names();
        registered.sort_unstable();
        provided.sort_unstable();
        assert_eq!(registered, provided);
    }

    #[test]
    fn a_reserved_name_is_not_automatically_a_provided_one() {
        // `Timer` is reserved because `every`/`after` own the word, and it has
        // no controller behind it. Anything that treats reserved as provided
        // would call `inject Timer as t` checkable and let a runtime failure
        // through.
        assert!(is_reserved("Timer"));
        assert!(!provided_names().contains(&"Timer"));
    }

    #[test]
    fn every_shipped_capability_uses_a_reserved_name() {
        // The two lists are maintained by hand and describe the same set, so
        // this is the assertion that keeps them describing it. A capability
        // registered under an unreserved name could be shadowed by an app's
        // controller and silently stop being the one the docs describe.
        for name in default_registry().names() {
            assert!(is_reserved(name), "`{name}` is registered but not reserved");
        }
    }

    #[test]
    fn the_default_set_matches_the_features_this_build_enabled() {
        let names: Vec<&str> = default_registry().names().collect();
        #[cfg(feature = "json")]
        assert!(names.contains(&"Json"), "{names:?}");
        #[cfg(feature = "net")]
        assert!(names.contains(&"Http"), "{names:?}");
        #[cfg(not(feature = "json"))]
        assert!(!names.contains(&"Json"), "{names:?}");
    }
}
