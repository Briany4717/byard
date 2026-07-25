//! Route patterns and transition geometry for RFC-0026 navigation.
//!
//! Two small, pure pieces, kept out of the interpreter so they can be reasoned
//! about (and tested) on their own:
//!
//! * [`RoutePattern`] — a `route "/user/:uid/post/:pid"` pattern compiled once,
//!   at mount time, into a segment vector, plus the `O(depth)` match that walks
//!   a concrete path against it and extracts the dynamic segments.
//! * [`NavTransition`] — the built-in transition family and the closed-form
//!   [`screen_motion`](NavTransition::screen_motion) that places each of the two
//!   simultaneously-live screens for a given progress. No per-frame state lives
//!   here: the interpreter samples one `Motion` for the progress and asks this
//!   for the geometry, which is what keeps the whole transition a handful of
//!   `f32` per frame.

use crate::diagnostics::{CompileError, Span};

/// One compiled segment of a [`RoutePattern`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteSegment {
    /// A literal segment (`detail`) — matches itself.
    Literal(String),
    /// A dynamic `:name` segment — matches any one segment, bound to `name`.
    Param(String),
    /// The `*` catch-all — matches every remaining segment (including none).
    Wildcard,
}

/// How far a screen's parallax partner travels, as a fraction of the viewport
/// (RFC-0026 `slide`): the covered screen drifts rather than tracking the
/// incoming one one-for-one, which is what reads as depth.
const PARALLAX: f32 = 0.3;

/// A route pattern compiled from its source spelling (RFC-0026 §"Route
/// matching"). Matching is `O(path depth)` against the segment vector; the
/// table is scanned in declaration order so the RFC's "routes are matched
/// top-to-bottom" rule is exactly what the code does — the first pattern that
/// matches wins, and a `*` written last is the catch-all it looks like.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutePattern {
    /// The pattern as written, for diagnostics and `route.path`.
    pub raw: String,
    /// The compiled segments, in order.
    pub segments: Vec<RouteSegment>,
}

/// Splits a path into its non-empty segments (`/detail/42` → `["detail", "42"]`;
/// `/` → `[]`). Leading, trailing, and doubled separators are all ignored, so
/// `/a/`, `a`, and `//a` name the same route.
pub fn split_path(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

impl RoutePattern {
    /// Compiles `raw` into segments.
    ///
    /// # Errors
    ///
    /// Returns [`CompileError::InvalidRoutePattern`] for an unnamed `:`
    /// segment, a `*` that is not the final segment, or a repeated parameter
    /// name — each of which would otherwise match in a way the author did not
    /// write (INV-4: never a silent surprise).
    pub fn compile(raw: &str, span: Span) -> Result<Self, CompileError> {
        let bad = |reason: &str| CompileError::InvalidRoutePattern {
            span,
            pattern: raw.to_string(),
            reason: reason.to_string(),
        };
        let mut segments = Vec::new();
        let mut names: Vec<&str> = Vec::new();
        let parts: Vec<&str> = split_path(raw).collect();
        for (i, part) in parts.iter().enumerate() {
            if let Some(name) = part.strip_prefix(':') {
                if name.is_empty() {
                    return Err(bad("a `:` segment must name a parameter, as in `:id`"));
                }
                if names.contains(&name) {
                    return Err(bad(&format!("the parameter `{name}` appears twice")));
                }
                names.push(name);
                segments.push(RouteSegment::Param(name.to_string()));
            } else if *part == "*" {
                if i + 1 != parts.len() {
                    return Err(bad(
                        "`*` matches the rest of the path, so it must come last",
                    ));
                }
                segments.push(RouteSegment::Wildcard);
            } else if part.contains(':') || part.contains('*') {
                return Err(bad(
                    "`:` and `*` are whole-segment forms; write `/detail/:id`, not `/detail:id`",
                ));
            } else {
                segments.push(RouteSegment::Literal((*part).to_string()));
            }
        }
        Ok(Self {
            raw: raw.to_string(),
            segments,
        })
    }

    /// Matches `path` against this pattern, returning the extracted parameters
    /// (in pattern order) or `None` if the pattern does not apply. Parameters
    /// are `Str` in v1 (RFC-0026 resolved question) — the receiving View parses
    /// what it needs.
    #[must_use]
    pub fn match_path(&self, path: &str) -> Option<Vec<(String, String)>> {
        let mut params = Vec::new();
        let mut actual = split_path(path);
        for seg in &self.segments {
            match seg {
                // `*` swallows whatever is left, including nothing at all.
                RouteSegment::Wildcard => return Some(params),
                RouteSegment::Literal(lit) => {
                    if actual.next()? != lit {
                        return None;
                    }
                }
                RouteSegment::Param(name) => {
                    params.push((name.clone(), actual.next()?.to_string()));
                }
            }
        }
        // Every pattern segment consumed: the path must be exhausted too, or
        // `/detail/:id` would swallow `/detail/42/edit`.
        actual.next().is_none().then_some(params)
    }
}

/// A built-in route transition (RFC-0026 §"Transitions").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NavTransition {
    /// Incoming slides in from the trailing edge, outgoing drifts out under it;
    /// reversed on a pop. The default, and the iOS/Material push idiom.
    #[default]
    Slide,
    /// Incoming rises from the bottom over a stationary outgoing — the
    /// modal-style push.
    SlideUp,
    /// Cross-fade.
    Fade,
    /// Instant swap: no second live screen, no frames requested.
    None,
}

/// Where one of a transition's two live screens sits at a given progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenMotion {
    /// Horizontal offset in logical px.
    pub dx: f32,
    /// Vertical offset in logical px.
    pub dy: f32,
    /// Opacity multiplier (`1.0` for the purely positional transitions).
    pub opacity: f32,
}

impl ScreenMotion {
    /// A screen at rest: on-screen, fully opaque.
    pub const REST: Self = Self {
        dx: 0.0,
        dy: 0.0,
        opacity: 1.0,
    };
}

impl NavTransition {
    /// Parses a `transition:` token; `None` for an unrecognized name (the
    /// checker's enum rule reports it).
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "slide" => Some(Self::Slide),
            "slide_up" => Some(Self::SlideUp),
            "fade" => Some(Self::Fade),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// How long one transition lasts, in ms — the duration of the fade, and the
    /// natural settling time budgeted for the positional springs.
    #[must_use]
    pub const fn duration_ms(self) -> u32 {
        match self {
            Self::Slide | Self::SlideUp => 320,
            Self::Fade => 200,
            Self::None => 0,
        }
    }

    /// Places one live screen for transition progress `p` (`0` at the start,
    /// `1` when settled).
    ///
    /// `incoming` is the screen being revealed, `pop` reverses the direction.
    /// Both screens are placed from the same `p`, so they move as one gesture:
    /// the screen on the *leading* side of the motion (the one entering on a
    /// push, leaving on a pop) travels the full viewport, its partner drifts by
    /// [`PARALLAX`] of it.
    #[must_use]
    pub fn screen_motion(
        self,
        p: f32,
        pop: bool,
        incoming: bool,
        width: f32,
        height: f32,
    ) -> ScreenMotion {
        // Distance from rest, in `0..=1`: an incoming screen closes it, an
        // outgoing screen opens it. One expression for both directions.
        let t = if incoming { 1.0 - p } else { p };
        // The screen on the moving edge: entering from it on a push, leaving
        // through it on a pop.
        let front = incoming != pop;
        match self {
            Self::None => ScreenMotion::REST,
            Self::Fade => ScreenMotion {
                opacity: (1.0 - t).clamp(0.0, 1.0),
                ..ScreenMotion::REST
            },
            Self::Slide => ScreenMotion {
                dx: if front {
                    width * t
                } else {
                    -width * PARALLAX * t
                },
                ..ScreenMotion::REST
            },
            // A modal push comes straight up over a screen that stays put, so
            // only the front screen moves.
            Self::SlideUp => ScreenMotion {
                dy: if front { height * t } else { 0.0 },
                ..ScreenMotion::REST
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 1)
    }

    fn pattern(raw: &str) -> RoutePattern {
        RoutePattern::compile(raw, sp()).expect("a valid pattern")
    }

    #[test]
    fn the_root_matches_only_itself() {
        let root = pattern("/");
        assert_eq!(root.match_path("/"), Some(Vec::new()));
        assert_eq!(root.match_path(""), Some(Vec::new()));
        assert_eq!(root.match_path("/home"), None);
    }

    #[test]
    fn a_literal_path_matches_exactly() {
        let p = pattern("/settings");
        assert_eq!(p.match_path("/settings"), Some(Vec::new()));
        // Trailing separators and the bare form name the same route.
        assert_eq!(p.match_path("settings/"), Some(Vec::new()));
        assert_eq!(p.match_path("/settings/deep"), None);
        assert_eq!(p.match_path("/"), None);
    }

    #[test]
    fn dynamic_segments_are_extracted_in_order() {
        let p = pattern("/user/:uid/post/:pid");
        assert_eq!(
            p.match_path("/user/5/post/12"),
            Some(vec![
                ("uid".to_string(), "5".to_string()),
                ("pid".to_string(), "12".to_string()),
            ])
        );
        assert_eq!(p.match_path("/user/5/post"), None);
        assert_eq!(p.match_path("/user/5/note/12"), None);
    }

    #[test]
    fn a_param_never_swallows_extra_segments() {
        let p = pattern("/detail/:id");
        assert_eq!(
            p.match_path("/detail/42"),
            Some(vec![("id".to_string(), "42".to_string())])
        );
        assert_eq!(p.match_path("/detail/42/edit"), None);
    }

    #[test]
    fn the_wildcard_matches_anything_including_nothing() {
        let p = pattern("*");
        assert_eq!(p.match_path("/"), Some(Vec::new()));
        assert_eq!(p.match_path("/a/b/c"), Some(Vec::new()));
    }

    #[test]
    fn a_wildcard_may_follow_a_prefix() {
        let p = pattern("/files/*");
        assert_eq!(p.match_path("/files/a/b"), Some(Vec::new()));
        assert_eq!(p.match_path("/files"), Some(Vec::new()));
        assert_eq!(p.match_path("/other/a"), None);
    }

    #[test]
    fn malformed_patterns_are_rejected_with_a_reason() {
        for bad in ["/detail/:", "/*/tail", "/a/:id/b/:id", "/detail:id"] {
            let err = RoutePattern::compile(bad, sp()).unwrap_err();
            assert!(
                matches!(err, CompileError::InvalidRoutePattern { .. }),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn a_push_slides_the_incoming_screen_in_from_the_right() {
        let t = NavTransition::Slide;
        let start = t.screen_motion(0.0, false, true, 400.0, 800.0);
        let mid = t.screen_motion(0.5, false, true, 400.0, 800.0);
        let end = t.screen_motion(1.0, false, true, 400.0, 800.0);
        assert!((start.dx - 400.0).abs() < 1e-3);
        assert!((mid.dx - 200.0).abs() < 1e-3);
        assert!(end.dx.abs() < 1e-3);
    }

    #[test]
    fn the_covered_screen_drifts_the_other_way_by_the_parallax_fraction() {
        let m = NavTransition::Slide.screen_motion(1.0, false, false, 400.0, 800.0);
        assert!((m.dx + 400.0 * PARALLAX).abs() < 1e-3);
    }

    #[test]
    fn a_pop_mirrors_the_push() {
        let t = NavTransition::Slide;
        // The screen being popped leaves through the right edge…
        let leaving = t.screen_motion(1.0, true, false, 400.0, 800.0);
        assert!((leaving.dx - 400.0).abs() < 1e-3);
        // …revealing the one underneath, which arrives from its parallax offset.
        let revealed_start = t.screen_motion(0.0, true, true, 400.0, 800.0);
        let revealed_end = t.screen_motion(1.0, true, true, 400.0, 800.0);
        assert!((revealed_start.dx + 400.0 * PARALLAX).abs() < 1e-3);
        assert!(revealed_end.dx.abs() < 1e-3);
    }

    #[test]
    fn a_fade_crosses_the_two_opacities() {
        let t = NavTransition::Fade;
        let inc = t.screen_motion(0.25, false, true, 400.0, 800.0);
        let out = t.screen_motion(0.25, false, false, 400.0, 800.0);
        assert!((inc.opacity - 0.25).abs() < 1e-3);
        assert!((out.opacity - 0.75).abs() < 1e-3);
        assert!(inc.dx.abs() < 1e-3 && inc.dy.abs() < 1e-3);
    }

    #[test]
    fn slide_up_leaves_the_covered_screen_where_it_is() {
        let t = NavTransition::SlideUp;
        assert!((t.screen_motion(0.0, false, true, 400.0, 800.0).dy - 800.0).abs() < 1e-3);
        assert!(t.screen_motion(0.5, false, false, 400.0, 800.0).dy.abs() < 1e-3);
    }

    #[test]
    fn none_never_moves_anything() {
        let m = NavTransition::None.screen_motion(0.5, false, true, 400.0, 800.0);
        assert_eq!(m, ScreenMotion::REST);
        assert_eq!(NavTransition::None.duration_ms(), 0);
    }
}
