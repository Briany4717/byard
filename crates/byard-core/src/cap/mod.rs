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
//! | `storage` | on | [`Store`], durable key/value in the OS data dir |

#[cfg(feature = "net")]
pub mod http;
#[cfg(feature = "json")]
pub mod json;
#[cfg(feature = "storage")]
pub mod store;

#[cfg(feature = "net")]
pub use http::Http;
#[cfg(feature = "json")]
pub use json::Json;
#[cfg(feature = "storage")]
pub use store::Store;

/// The type names the framework reserves for its own capabilities
/// (RFC-0029 §7).
///
/// Listed unconditionally, not per feature. A name has to mean the same thing
/// in every build, otherwise an app that compiles with `--no-default-features`
/// and defines its own `Http` would break the moment someone turned the
/// feature back on, and the failure would look like a dependency problem
/// rather than a name collision.
pub const RESERVED_NAMES: &[&str] = &["Http", "Json", "Store", "Timer"];

/// Whether `name` is reserved for a built-in capability.
#[must_use]
pub fn is_reserved(name: &str) -> bool {
    RESERVED_NAMES.contains(&name)
}

/// The capabilities enabled in this build, ready to register (RFC-0029 §7).
///
/// `app` names the application, which is what decides where [`Store`] writes:
/// two apps must not share a settings file, and a store keyed by anything less
/// stable than the project name would move when the binary did.
///
/// Built here rather than at each host so `byard dev` and a shipped `App`
/// cannot end up offering different sets, which would make an app behave one
/// way under the dev runner and another way when shipped, the single most
/// expensive kind of difference a framework can have.
#[must_use]
pub fn default_registry(app: &str) -> crate::bridge::ControllerRegistry {
    #[allow(unused_mut, unused_variables)]
    let mut registry = crate::bridge::ControllerRegistry::new();
    #[cfg(feature = "json")]
    registry.insert(std::sync::Arc::new(Json));
    #[cfg(feature = "net")]
    registry.insert(std::sync::Arc::new(Http::new()));
    #[cfg(feature = "storage")]
    registry.insert(std::sync::Arc::new(Store::for_app(app)));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_capability_uses_a_reserved_name() {
        // The two lists are maintained by hand and describe the same set, so
        // this is the assertion that keeps them describing it. A capability
        // registered under an unreserved name could be shadowed by an app's
        // controller and silently stop being the one the docs describe.
        for name in default_registry("test").names() {
            assert!(is_reserved(name), "`{name}` is registered but not reserved");
        }
    }

    #[test]
    fn the_default_set_matches_the_features_this_build_enabled() {
        let names: Vec<&str> = default_registry("test").names().collect();
        #[cfg(feature = "json")]
        assert!(names.contains(&"Json"), "{names:?}");
        #[cfg(feature = "net")]
        assert!(names.contains(&"Http"), "{names:?}");
        #[cfg(not(feature = "json"))]
        assert!(!names.contains(&"Json"), "{names:?}");
    }
}
