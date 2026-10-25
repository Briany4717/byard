//! Navigation containers (RFC-0026): `NavStack` and `NavHost`.
//!
//! The model is that navigation state is a reactive `var`. A container's
//! `NavPool` holds no intent of its own, it observes the driving projection
//! each frame and reconciles its history to match. This is that
//! reconciliation (pushing, popping and replacing a route, the transition
//! between two screens and the edge swipe that drives it by hand), the
//! navigation actions an event handler calls, and the deep links that open a
//! route from a URL.

use super::{Interpreter, Lowered, PerfWarning, RenderNode, members_span};
use crate::diagnostics::{CompileError, Span};
use crate::interp::env::{SignalId, Value};
use crate::interp::reactive::ScopeId;
use crate::parser::ast::{Arg, Attr, ElementNode, Expr, Member, RouteKind, StateBlock, StrPart};
use crate::symbol::Symbol;

/// Which navigation container a [`NavPool`] backs (RFC-0026).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NavKind {
    /// `NavStack`, a push/pop history stack driven by a `path` var.
    Stack,
    /// `NavHost`, a flat set of preserved tabs driven by an `active` var.
    Host,
}

/// One compiled case of a navigation container: its pattern, its optional
/// params binding, and the body AST that is lowered *once per live entry* and
/// then preserved (RFC-0026 §4).
struct RouteDef {
    /// The compiled pattern (a literal tab name for a `NavHost`).
    pattern: crate::interp::nav::RoutePattern,
    /// The `{|params| … }` binding name, if written.
    params_binding: Option<Symbol>,
    /// The case body, re-lowered per entry (each entry gets its own state).
    body: Vec<Member>,
}

/// One live screen of a navigation container: a route instance whose lowered
/// subtree, and therefore every `var`, scroll offset and animation inside it,
/// stays alive while it is on the stack (RFC-0026 §4 state preservation).
pub(super) struct NavEntry {
    /// The concrete path this entry was mounted for (`/detail/42`), or the tab
    /// name for a `NavHost`.
    pub(super) path: String,
    /// The lowered subtree. Never re-lowered while the entry lives.
    pub(super) nodes: Vec<RenderNode>,
    /// The source range the entry's body spans, so a discarded entry takes its
    /// animation state with it (RFC-0025: an animation lives and dies with its
    /// element).
    body_span: Span,
}

/// An in-flight route transition (RFC-0026 §5): two entries are alive and
/// composited at once, placed from a single progress scalar.
#[derive(Clone, Copy)]
struct NavAnim {
    /// The entry being covered/left.
    outgoing: usize,
    /// The entry being revealed.
    incoming: usize,
    /// Whether this is a pop (reverses the direction).
    pop: bool,
    /// The `0 → 1` progress spring. Sampled once per frame; both screens'
    /// geometry is a closed form of it.
    motion: byard_core::frame::Motion,
    /// Whether progress is currently driven by an edge-swipe gesture rather
    /// than the clock (RFC-0026 §"Swipe-back gesture"). A gesture-driven
    /// transition never settles on its own, the release decides.
    gesture: bool,
    /// Gesture progress, while `gesture` is set.
    gesture_p: f32,
}

/// One screen a navigation container paints this frame, with the offset and
/// opacity its transition places it at.
#[derive(Clone, Copy)]
pub(super) struct LiveScreen {
    /// Index into [`NavPool::entries`].
    pub(super) entry: usize,
    /// Whether this is the screen being revealed (as opposed to the one being
    /// covered/left), the half of the transition geometry that is per-screen.
    pub(super) incoming: bool,
    /// Whether this screen is the one in the container's normal flow. Exactly
    /// one is: the screen the navigation state currently names, which is what
    /// gives the container its size. Its transitioning partner is laid out
    /// absolutely over the same rect, so it overlaps without displacing
    /// anything and without perturbing the container's measured size.
    pub(super) in_flow: bool,
}

/// The slice of a navigation container's state that lowered *action closures*
/// need at fire time (RFC-0026 §"navigation actions").
///
/// An action lowers to a `FnMut(&mut ReactiveCtx)`, it never sees the
/// interpreter, but `back(navPath)` has to know what is underneath the top of
/// the stack, and `replace(…)` has to tell the next reconcile not to stack. Both
/// are answered by this one shared cell, updated whenever the stack moves. `Rc`/
/// `RefCell` are sound here for the same reason the radio groups' are: the
/// interpreter and its event closures are single-threaded logic-thread state
/// (`!Send`; signals are only ever touched on the logic thread).
pub(super) type NavSharedCell = std::rc::Rc<std::cell::RefCell<NavShared>>;

#[derive(Default)]
pub(super) struct NavShared {
    /// The live entries' paths, mirroring [`NavPool::entries`].
    paths: Vec<String>,
    /// The visible entry's index.
    current: usize,
    /// Set by `replace(…)`: the next navigation takes the current entry's slot
    /// instead of stacking on top of it.
    replace_next: bool,
}

/// A navigation container's state (RFC-0026), indexed by
/// [`RenderNode::Nav::pool`].
///
/// The whole model is: *navigation state is a reactive `var`*. This pool holds
/// no navigation intent of its own, it observes the driving `path`/`active`
/// projection each frame and reconciles its history stack to match, which is
/// what makes `navPath = "/detail/42"` a push and `navPath = "/"` a pop with no
/// controller object anywhere (RFC-0003: no widget references).
pub(super) struct NavPool {
    /// Stack or tabs.
    pub(super) kind: NavKind,
    /// The container element's own attributes (background, padding, `grow`, …).
    pub(super) attrs: Vec<Attr>,
    /// `on <state> { … }` blocks on the container.
    pub(super) state_blocks: Vec<StateBlock>,
    /// The transition family (`slide` for a stack, `fade` for tabs by default).
    pub(super) transition: crate::interp::nav::NavTransition,
    /// RFC-0026 `swipe_back`: the Cupertino interactive edge-pop gesture.
    pub(super) swipe_back: bool,
    /// RFC-0026 `deep_link`: this stack accepts OS URL intents.
    deep_link: bool,
    /// RFC-0026 `max_depth` (default 10, `0` disables): the runaway-push guard.
    max_depth: usize,
    /// The signal behind `path:`/`active:` when it is a writable `var`, the
    /// engine writes it back for `back(…)`, a completed swipe-back, a rejected
    /// over-deep push, and a delivered deep link. Reflected state, never a
    /// second source of truth.
    path_sig: Option<SignalId>,
    /// The compiled route table, in declaration order (first match wins).
    routes: Vec<RouteDef>,
    /// User-view names in scope at lower time.
    known_views: Vec<String>,
    /// The instance env captured at lower time (RFC-0019), restored when a new
    /// entry's body is lowered.
    env_snapshot: Vec<(Symbol, Value)>,
    /// The live history stack (a `NavHost`'s instantiated, preserved tabs).
    pub(super) entries: Vec<NavEntry>,
    /// Index into `entries` of the screen the navigation state names.
    pub(super) current: usize,
    /// The in-flight transition, if any.
    anim: Option<NavAnim>,
    /// The screens to lay out and paint this frame, one at rest, two during a
    /// transition, in painter's order (the moving edge last).
    pub(super) live: Vec<LiveScreen>,
    /// This frame's transition progress (`1.0` at rest).
    pub(super) progress: f32,
    /// Whether this frame's transition is a pop (reverses the direction).
    pub(super) popping: bool,
    /// The last navigation value observed, so a change is edge-triggered.
    last_path: Option<String>,
    /// The stack state the lowered `back`/`replace` closures read and write.
    shared: NavSharedCell,
    /// A settled navigation whose `route_change` has not fired yet.
    pending_change: Option<String>,
    /// Paths already reported as unmatched, so a steady-state mismatch
    /// diagnoses once instead of every frame.
    warned_paths: Vec<String>,
    /// The container's source span, for diagnostics.
    span: Span,
}

/// Default `NavStack` `max_depth` (RFC-0026 resolved question "memory
/// pressure"): a stack deeper than this is almost certainly a push loop, not a
/// screen hierarchy. `max_depth: 0` disables the guard.
const DEFAULT_NAV_MAX_DEPTH: i64 = 10;

/// Fraction of the viewport an edge-swipe must cross to complete a pop on
/// release (RFC-0026 §"Swipe-back gesture"); short of it, the screen snaps back.
const NAV_SWIPE_COMMIT: f32 = 0.5;

/// Width of the left-edge strip an interactive swipe-back may start in, in
/// logical px, the iOS convention, and narrow enough that a horizontally
/// scrolling child in the middle of the screen is never stolen from.
const NAV_SWIPE_EDGE: f32 = 24.0;

/// The progress curve for the *remaining* `fraction` of a transition, what a
/// released swipe-back hands over to (RFC-0026): the same ramp, shortened to the
/// distance still to travel, with a floor so a hand-off at the very end is still
/// a frame or two of motion rather than a snap.
fn nav_transition_tail(
    transition: crate::interp::nav::NavTransition,
    fraction: f32,
) -> byard_core::frame::MotionCurve {
    /// Shortest tail worth animating: one 60 Hz frame.
    const MIN_TAIL_MS: f32 = 16.0;
    let mut curve = nav_progress_curve(transition);
    curve.params[0] = (curve.params[0] * fraction.clamp(0.0, 1.0)).max(MIN_TAIL_MS);
    curve
}

/// The `0 → 1` progress curve driving a route transition (RFC-0026
/// §"Transitions"): a **fixed-duration, monotone** ramp, decelerating into
/// place for the positional transitions, symmetric for the cross-fade.
///
/// Deliberately *not* a spring, even though a spring drives every other
/// animation in the engine. A spring is the right primitive for a value that
/// may be retargeted mid-flight; it is the wrong one for normalized progress.
/// RFC-0010's default spring is underdamped (`ζ ≈ 0.69`), and a screen's
/// arrival must not overshoot: past `p = 1` the incoming screen would slide off
/// its own leading edge, and the undershoot that follows would walk it visibly
/// back out again, a wobble at the exact moment the transition is meant to be
/// over. Clamping `p` hides the overshoot but not the undershoot, and it makes
/// the settle test (which reads the *unclamped* curve) disagree with what is on
/// screen, so the engine keeps asking for frames long after the motion looks
/// finished and can park with the screen a few pixels out of place.
///
/// A duration ramp has none of those problems: bounded to `0..=1` by
/// construction, monotone, and landing on exactly `1.0` at exactly
/// [`duration_ms`](crate::interp::nav::NavTransition::duration_ms). The
/// decelerating cubic is the shape a *critically damped* spring traces, the
/// feel RFC-0026 asks for, without the asymptotic tail that never ends.
fn nav_progress_curve(
    transition: crate::interp::nav::NavTransition,
) -> byard_core::frame::MotionCurve {
    use crate::interp::nav::NavTransition;
    let kind = match transition {
        // Decelerate into place: fast off the mark, easing to a stop.
        NavTransition::Slide | NavTransition::SlideUp => byard_core::frame::MotionCurve::EASE_OUT,
        // A cross-fade has no direction to decelerate along.
        NavTransition::Fade | NavTransition::None => byard_core::frame::MotionCurve::EASE_IN_OUT,
    };
    byard_core::frame::MotionCurve {
        kind,
        #[allow(clippy::cast_precision_loss)]
        params: [transition.duration_ms() as f32, 0.0, 0.0],
    }
}

/// The navigable path inside a deep-link URL (RFC-0026 §"Deep linking"): the
/// scheme and authority belong to the platform, the path to the route table.
/// `byard://item/42` → `/item/42`; a bare `/item/42` passes through unchanged.
/// A query string or fragment is dropped, v1 routes match on the path alone.
fn deep_link_path(url: &str) -> String {
    let path = match url.split_once("://") {
        Some((scheme, rest)) => {
            // A web link's first segment is a host (`app.example/item/42`); a
            // custom app scheme has no authority, so `byard://item/42` is all
            // path. Told apart by the scheme and by the shape of that first
            // segment, a host has a dot or a port, a route segment does not.
            let authority = rest.split('/').next().unwrap_or("");
            let has_host = matches!(scheme, "http" | "https")
                || authority.contains('.')
                || authority.contains(':');
            if has_host {
                rest.find('/').map_or("/", |i| &rest[i..])
            } else {
                rest
            }
        }
        // `byard:/item/42` (scheme-relative) or a bare `/item/42`.
        None => url.split_once(':').map_or(url, |(_, p)| p),
    };
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or(path)
        .trim_end_matches('/');
    if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

impl Interpreter {
    // ── Navigation (RFC-0026) ───────────────────────────────────────────────

    /// Lowers a `NavStack`/`NavHost` (RFC-0026): validates and compiles its
    /// route table into a [`NavPool`] and returns the single [`RenderNode::Nav`]
    /// that stands for the container.
    ///
    /// No screen is lowered here. A route's View subtree is instantiated the
    /// first time navigation actually reaches it and preserved from then on, so
    /// mounting a ten-route table costs ten compiled patterns, not ten View
    /// trees (RFC-0026's "lazy route loading", which falls out of the
    /// entry model rather than needing a separate mechanism).
    pub(super) fn lower_nav(
        &mut self,
        el: &ElementNode,
        attrs: &[Attr],
        state_blocks: Vec<StateBlock>,
        known_views: &[&str],
    ) -> RenderNode {
        use crate::interp::nav::{NavTransition, RoutePattern};

        self.errors.extend(super::intrinsics::validate_nav(el));
        let kind = if el.name.as_str() == "NavStack" {
            NavKind::Stack
        } else {
            NavKind::Host
        };
        let want = if kind == NavKind::Stack {
            RouteKind::Route
        } else {
            RouteKind::Tab
        };

        let mut routes = Vec::new();
        for child in &el.children {
            let Member::Route {
                kind: written,
                pattern,
                params,
                body,
                pattern_span,
                ..
            } = child
            else {
                continue; // diagnosed by `validate_nav`
            };
            if *written != want {
                continue; // likewise
            }
            let Ok(compiled) = RoutePattern::compile(pattern, *pattern_span) else {
                continue;
            };
            routes.push(RouteDef {
                pattern: compiled,
                params_binding: params.clone(),
                body: body.clone(),
            });
        }

        // The navigation state: `NavStack(path: navPath)` / `NavHost(active:
        // tab)`, read as an ordinary reactive projection. A container written
        // without it (already an arity diagnostic) falls back to its first case,
        // so a malformed source still renders instead of blanking.
        let path_expr = el.content.first().map_or_else(
            || {
                let first = routes
                    .first()
                    .map_or_else(String::new, |r| r.pattern.raw.clone());
                Expr::StrLit(vec![StrPart::Text(first)], el.span)
            },
            |arg| arg.value.clone(),
        );
        // The writable side of the same value, when it is a `var` (the usual
        // case): reflected state the engine writes back for `back(…)`, a
        // completed swipe, a rejected over-deep push and a delivered deep link.
        let path_sig = match &path_expr {
            Expr::Ident(name, _) => match self.env.lookup(name) {
                Some(Value::Signal(sig)) => Some(*sig),
                _ => None,
            },
            _ => None,
        };
        let path = self.bind_value(&path_expr);

        let transition = Self::enum_prop(attrs, "transition")
            .and_then(NavTransition::from_token)
            // A stack push has a direction; a tab switch does not.
            .unwrap_or(if kind == NavKind::Stack {
                NavTransition::Slide
            } else {
                NavTransition::Fade
            });
        let swipe_back =
            kind == NavKind::Stack && self.eval_bool_prop(attrs, "swipe_back").unwrap_or(false);
        let deep_link = self.eval_bool_prop(attrs, "deep_link").unwrap_or(false);
        #[allow(clippy::cast_sign_loss)]
        let max_depth = self
            .eval_int_prop(attrs, "max_depth")
            .unwrap_or(DEFAULT_NAV_MAX_DEPTH)
            .max(0) as usize;

        let shared = match path_sig {
            Some(sig) => self.nav_shared_cell(sig),
            None => NavSharedCell::default(),
        };
        let pool = self.nav_pools.len();
        self.nav_pools.push(NavPool {
            kind,
            attrs: attrs.to_vec(),
            state_blocks,
            transition,
            swipe_back,
            deep_link,
            max_depth,
            path_sig,
            routes,
            known_views: known_views.iter().map(|s| (*s).to_string()).collect(),
            env_snapshot: self.capture_env_snapshot(),
            entries: Vec::new(),
            current: 0,
            anim: None,
            live: Vec::new(),
            progress: 1.0,
            popping: false,
            last_path: None,
            shared,
            pending_change: None,
            warned_paths: Vec::new(),
            span: el.span,
        });
        self.nav_elems.push(None);
        RenderNode::Nav { pool, path }
    }

    /// Reconciles one navigation container against its driving projection
    /// (RFC-0026), then advances its transition and descends into the live
    /// screens so nested `when`/`for`/`NavStack` reconcile too. Returns `true`
    /// when something structural changed, so the caller re-pulls.
    pub(super) fn reconcile_nav(&mut self, pool: usize, path: ScopeId, depth: u32) -> bool {
        let mut dirtied = false;
        // The navigation value. Anything that is not a `Str` names no route, so
        // fall back to the first case, a `NavStack` always shows *something*.
        let target = match self.binding_value(path) {
            Some(Value::Str(s)) => s,
            _ => self.nav_pools[pool]
                .routes
                .first()
                .map_or_else(String::new, |r| r.pattern.raw.clone()),
        };
        if self.nav_pools[pool].last_path.as_deref() != Some(target.as_str()) {
            self.nav_pools[pool].last_path = Some(target.clone());
            dirtied |= self.navigate_to(pool, &target);
        }
        dirtied |= self.advance_nav(pool);

        let live: Vec<usize> = self.nav_pools[pool].live.iter().map(|s| s.entry).collect();
        for entry in live {
            // Take the subtree out so a nested container growing `self.nav_pools`
            // (or the `for`/`when` pools) can never alias this entry's vector.
            let nodes = std::mem::take(&mut self.nav_pools[pool].entries[entry].nodes);
            dirtied |= self.reconcile_structure(&nodes, depth + 1);
            self.nav_pools[pool].entries[entry].nodes = nodes;
        }
        dirtied
    }

    /// Moves a navigation container to `target` (RFC-0026 §3): pops to a
    /// preserved entry if the path is already on the stack, switches to an
    /// instantiated tab, or mounts a new screen. Returns `true` if the live set
    /// changed.
    fn navigate_to(&mut self, pool: usize, target: &str) -> bool {
        // A navigation that lands mid-transition finishes the previous one
        // outright: entry indices stay stable, and the user's latest intent,
        // not a half-finished animation, decides what is on screen.
        self.finish_nav(pool);
        let kind = self.nav_pools[pool].kind;
        let from = self.nav_pools[pool].current;
        // `replace(…)` armed this navigation to take the current slot.
        let replacing = kind == NavKind::Stack
            && std::mem::take(&mut self.nav_pools[pool].shared.borrow_mut().replace_next);
        if !replacing
            && self.nav_pools[pool]
                .entries
                .get(from)
                .is_some_and(|e| e.path == target)
        {
            return false;
        }

        // A path already on the stack is a *pop* back to it; a tab already
        // instantiated is simply re-shown. Either way the preserved subtree,
        // its `var`s, scroll offsets and controllers, is reused as it stands.
        // A replace deliberately skips this: it must mint a fresh screen in the
        // slot it is taking over, not resurrect an older one.
        if !replacing {
            if let Some(to) = self.nav_pools[pool]
                .entries
                .iter()
                .position(|e| e.path == target)
            {
                let pop = kind == NavKind::Stack && to < from;
                self.begin_nav_anim(pool, from, to, pop);
                self.sync_nav_shared(pool);
                return true;
            }
        }

        let Some((route, params)) = self.match_route(pool, target) else {
            // RFC-0026: an unmatched path is a runtime warning, the container
            // keeps showing the last matched route rather than blanking. Warned
            // once per path so a steady-state mismatch cannot spam the log.
            if !self.nav_pools[pool]
                .warned_paths
                .iter()
                .any(|p| p == target)
            {
                self.nav_pools[pool].warned_paths.push(target.to_string());
                let span = self.nav_pools[pool].span;
                self.errors.push(CompileError::UnmatchedRoute {
                    span,
                    path: target.to_string(),
                });
            }
            return false;
        };

        // RFC-0026 resolved question "memory pressure": refuse to grow a stack
        // past `max_depth` and reflect the refusal back into the navigation
        // `var`, so app state and screen agree instead of silently diverging.
        let depth = self.nav_pools[pool].entries.len();
        let max = self.nav_pools[pool].max_depth;
        if kind == NavKind::Stack && !replacing && max > 0 && depth >= max {
            // Reported on the frame the push is refused. It cannot repeat on its
            // own, reflecting the path back means the next reconcile sees no
            // change, so the host hears about each distinct runaway once.
            self.perf_warnings.push(PerfWarning::DeepNavStack {
                depth,
                path: target.to_string(),
            });
            self.reflect_nav_path(pool);
            return false;
        }

        // A stack only ever pushes onto its top: anything above `current` was
        // already discarded by the pop that got us here. A replace goes one
        // further and drops the screen it is standing on, so the new one lands
        // in that slot rather than on top of it.
        let from = if replacing {
            self.drop_nav_entries(pool, from);
            let below = from.saturating_sub(1);
            self.nav_pools[pool].current = below;
            below
        } else {
            if kind == NavKind::Stack {
                self.drop_nav_entries(pool, from + 1);
            }
            from
        };
        let to = self.mount_nav_entry(pool, route, target, &params);
        self.begin_nav_anim(pool, from, to, false);
        self.sync_nav_shared(pool);
        true
    }

    /// Mirrors the stack's shape into the cell the lowered `back`/`replace`
    /// closures read (see [`NavShared`]). Called whenever entries or `current`
    /// move, which is the only time it can go stale.
    fn sync_nav_shared(&mut self, pool: usize) {
        let paths: Vec<String> = self.nav_pools[pool]
            .entries
            .iter()
            .map(|e| e.path.clone())
            .collect();
        let current = self.nav_pools[pool].current;
        let mut shared = self.nav_pools[pool].shared.borrow_mut();
        shared.paths = paths;
        shared.current = current;
    }

    /// The first route whose pattern matches `path`, with its extracted
    /// parameters, declaration order, first match wins (RFC-0026).
    fn match_route(&self, pool: usize, path: &str) -> Option<(usize, Vec<(String, String)>)> {
        self.nav_pools[pool]
            .routes
            .iter()
            .enumerate()
            .find_map(|(i, r)| r.pattern.match_path(path).map(|params| (i, params)))
    }

    /// Instantiates a route's View subtree for `path` and appends it as a new
    /// entry, returning its index. The body is lowered with `route` (and the
    /// `{|params| … }` binding, if written) in scope, bound to the extracted
    /// parameters as records, `Str`-valued in v1 (RFC-0026 resolved question).
    fn mount_nav_entry(
        &mut self,
        pool: usize,
        route: usize,
        path: &str,
        params: &[(String, String)],
    ) -> usize {
        let body = self.nav_pools[pool].routes[route].body.clone();
        let binding = self.nav_pools[pool].routes[route].params_binding.clone();
        let known: Vec<String> = self.nav_pools[pool].known_views.clone();
        let env_snap = self.nav_pools[pool].env_snapshot.clone();

        let env_base = self.env.len();
        for (k, v) in &env_snap {
            self.env.push(k.clone(), v.clone());
        }
        let params_record = Value::Record(
            params
                .iter()
                .map(|(k, v)| (Symbol::intern(k), Value::Str(v.clone())))
                .collect(),
        );
        self.env.push(
            Symbol::intern("route"),
            Value::Record(vec![
                (Symbol::intern("path"), Value::Str(path.to_string())),
                (Symbol::intern("params"), params_record.clone()),
            ]),
        );
        if let Some(name) = binding {
            self.env.push(name, params_record);
        }
        let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
        // Mark that we are inside a route body, so anything re-lowered at render
        // time (an event action, a reactive prop) captures `route`/`params` in
        // its env snapshot, the bindings are truncated out just below.
        self.nav_depth += 1;
        let nodes = self.lower_members(&body, &known_refs);
        self.nav_depth -= 1;
        self.env.truncate(env_base);

        let body_span = members_span(&self.nav_pools[pool].routes[route].body);
        self.nav_pools[pool].entries.push(NavEntry {
            path: path.to_string(),
            nodes,
            body_span,
        });
        self.nav_pools[pool].entries.len() - 1
    }

    /// Starts the transition from entry `from` to entry `to`, or completes the
    /// move instantly when there is nothing to animate (`transition: none`, a
    /// first mount, or a host that never advanced the clock).
    fn begin_nav_anim(&mut self, pool: usize, from: usize, to: usize, pop: bool) {
        use crate::interp::nav::NavTransition;

        self.nav_pools[pool].current = to;
        let transition = self.nav_pools[pool].transition;
        let first_mount = self.nav_pools[pool].live.is_empty();
        if transition == NavTransition::None || !self.clock_set || first_mount || from == to {
            self.nav_pools[pool].anim = None;
            self.complete_nav(pool, pop, !first_mount);
            return;
        }
        self.nav_pools[pool].anim = Some(NavAnim {
            outgoing: from,
            incoming: to,
            pop,
            motion: byard_core::frame::Motion {
                from: 0.0,
                to: 1.0,
                start_ms: self.now_ms,
                curve: nav_progress_curve(transition),
            },
            gesture: false,
            gesture_p: 0.0,
        });
    }

    /// Completes any in-flight transition: a pop discards the screens it left
    /// behind (with their animation state, RFC-0025), and the settled path is
    /// queued for `route_change`.
    fn finish_nav(&mut self, pool: usize) {
        let Some(anim) = self.nav_pools[pool].anim.take() else {
            return;
        };
        self.complete_nav(pool, anim.pop, true);
    }

    /// The settled state of a navigation, whether it animated or not: a pop
    /// discards what it left behind, and the arrival is queued for
    /// `route_change` unless this was the container's first mount.
    fn complete_nav(&mut self, pool: usize, pop: bool, announce: bool) {
        if pop {
            let keep = self.nav_pools[pool].current + 1;
            self.drop_nav_entries(pool, keep);
        }
        if announce {
            self.nav_pools[pool].pending_change = self.nav_pools[pool]
                .entries
                .get(self.nav_pools[pool].current)
                .map(|e| e.path.clone());
        }
        self.sync_nav_shared(pool);
    }

    /// Discards entries from `keep` upward (RFC-0026 §4: only the back target is
    /// preserved, the routes a multi-pop skipped over are gone), dropping each
    /// discarded screen's animation state along with it.
    fn drop_nav_entries(&mut self, pool: usize, keep: usize) {
        while self.nav_pools[pool].entries.len() > keep {
            let Some(entry) = self.nav_pools[pool].entries.pop() else {
                break;
            };
            self.drop_animation_state(entry.body_span);
        }
    }

    /// Samples this frame's transition progress, settles it if it has arrived,
    /// and rebuilds the live screen set. Returns `true` when the live set
    /// changed (a screen mounted or unmounted), which is what makes a
    /// transition's start and end structural events.
    fn advance_nav(&mut self, pool: usize) -> bool {
        let before: Vec<usize> = self.nav_pools[pool].live.iter().map(|s| s.entry).collect();
        let (p, pop) = match self.nav_pools[pool].anim {
            None => (1.0, false),
            Some(anim) if anim.gesture => (anim.gesture_p, anim.pop),
            Some(anim) => {
                // A duration ramp is *done* when its duration has elapsed,
                // exactly, on the frame it reaches `p = 1`. Settling on the
                // clock rather than on an epsilon around the value keeps "the
                // motion has stopped" and "the pixels have stopped" the same
                // instant: an epsilon test keeps frames coming through a tail
                // the eye cannot see, and can call it settled at a `p` that is
                // not quite `1`, leaving the screen fractionally out of place
                // until something else forces a redraw.
                let elapsed = self.now_ms.saturating_sub(anim.motion.start_ms);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let duration = anim.motion.curve.params[0].max(0.0) as u32;
                if elapsed >= duration {
                    self.finish_nav(pool);
                    (1.0, false)
                } else {
                    // Keep the frames coming while a screen is still moving.
                    self.any_active = true;
                    (anim.motion.sample(self.now_ms), anim.pop)
                }
            }
        };
        self.rebuild_nav_live(pool, p, pop);
        before
            != self.nav_pools[pool]
                .live
                .iter()
                .map(|s| s.entry)
                .collect::<Vec<_>>()
    }

    /// Recomputes which screens are alive this frame, in painter's order: at
    /// rest exactly one (the current entry); mid-transition the covered screen
    /// first and the one on the moving edge above it, so a pushed screen slides
    /// *over* its predecessor and a popped one slides *off* the screen beneath.
    fn rebuild_nav_live(&mut self, pool: usize, p: f32, pop: bool) {
        let current = self.nav_pools[pool].current;
        let live = match self.nav_pools[pool].anim {
            Some(anim) => {
                // On a push the arriving screen is on top; on a pop the leaving
                // one is, since it is the screen sliding away over the other.
                let (back, front) = if pop {
                    (anim.incoming, anim.outgoing)
                } else {
                    (anim.outgoing, anim.incoming)
                };
                [back, front]
                    .into_iter()
                    .map(|entry| LiveScreen {
                        entry,
                        incoming: entry == anim.incoming,
                        in_flow: entry == current,
                    })
                    .collect()
            }
            None if current < self.nav_pools[pool].entries.len() => vec![LiveScreen {
                entry: current,
                incoming: true,
                in_flow: true,
            }],
            None => Vec::new(),
        };
        self.nav_pools[pool].progress = p;
        self.nav_pools[pool].popping = pop;
        self.nav_pools[pool].live = live;
    }

    /// Writes the current entry's path back into the navigation `var`, when it
    /// is one (RFC-0026: `path` is *reflected*, the engine and the app share
    /// one source of truth, so an engine-side move updates it).
    fn reflect_nav_path(&mut self, pool: usize) {
        let Some(sig) = self.nav_pools[pool].path_sig else {
            return;
        };
        let current = self.nav_pools[pool].current;
        let Some(path) = self.nav_pools[pool]
            .entries
            .get(current)
            .map(|e| e.path.clone())
        else {
            return;
        };
        // Keep `last_path` in step so the write is not read back as a fresh
        // navigation on the next reconcile.
        self.nav_pools[pool].last_path = Some(path.clone());
        self.ctx.write_signal(sig, Value::Str(path));
    }

    /// Fires `route_change` for every container whose navigation has settled
    /// since the last tick (RFC-0026 §1). Runs after the render that registered
    /// the handlers, like the other engine-fired events.
    pub(super) fn fire_route_changes(&mut self) {
        for pool in 0..self.nav_pools.len() {
            let (Some(path), Some(&Some(elem))) = (
                self.nav_pools[pool].pending_change.take(),
                self.nav_elems.get(pool),
            ) else {
                continue;
            };
            self.router.fire_event(
                &mut self.ctx,
                elem,
                super::events::EventKind::RouteChange,
                Some(&Value::Str(path)),
            );
        }
    }

    /// Lowers one of the RFC-0026 navigation actions to a reactive computation.
    /// Returns `None` for anything else, so ordinary calls keep their handling.
    ///
    /// All three end in the same place, a write to the navigation `var`, which
    /// is the whole model: navigation state *is* that `var`, so an action and an
    /// assignment are the same event downstream. What the two non-trivial
    /// actions add is the bit an assignment cannot know: `back` reads the
    /// history to find the path underneath, and `replace` marks the write so the
    /// next reconcile takes the current screen's slot instead of stacking.
    pub(super) fn lower_nav_action(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        payload_name: Option<&Symbol>,
    ) -> Option<Lowered> {
        let Expr::Ident(name, _) = callee else {
            return None;
        };
        let action = name.as_str();
        if !matches!(action, "navigate" | "back" | "replace") {
            return None;
        }
        // The first argument names the navigation `var`; without a matching
        // container there is nothing to drive, so leave the call alone rather
        // than swallow an app's own `navigate`/`back`/`replace` binding.
        let target = args.first()?;
        let Expr::Ident(var, _) = &target.value else {
            return None;
        };
        let sig = match self.env.lookup(var) {
            Some(Value::Signal(sig)) => *sig,
            _ => return None,
        };
        // Keyed by the `var`, so lowering order does not matter: whichever of
        // the action and the container is lowered first creates the cell.
        let shared = self.nav_shared_cell(sig);

        if action == "back" {
            return Some(Box::new(move |ctx| {
                let previous = {
                    let state = shared.borrow();
                    (state.current > 0)
                        .then(|| state.paths.get(state.current - 1).cloned())
                        .flatten()
                };
                // At the root there is nothing to pop, a no-op, not an error
                // (RFC-0026 §6). The returned `Bool` says which happened.
                match previous {
                    Some(path) => {
                        ctx.write_signal(sig, Value::Str(path));
                        Value::Bool(true)
                    }
                    None => Value::Bool(false),
                }
            }));
        }

        let mut path = self.lower_expr(&args.get(1)?.value, payload_name);
        let is_replace = action == "replace";
        Some(Box::new(move |ctx| {
            let value = path(ctx);
            let Value::Str(target) = value else {
                return Value::Unit;
            };
            if is_replace {
                shared.borrow_mut().replace_next = true;
            }
            ctx.write_signal(sig, Value::Str(target));
            Value::Unit
        }))
    }

    /// Starts an interactive edge-swipe pop on the stack under `pos`, if one is
    /// enabled there, the press landed in the leading edge strip, and there is
    /// somewhere to pop to (RFC-0026 §"Swipe-back gesture").
    ///
    /// The gesture *is* the transition: it moves `current` to the entry below
    /// immediately and drives the same pop geometry by hand, so what the finger
    /// drags is the real previous screen, already preserved on the stack, not
    /// a snapshot of it. Returns `true` if a swipe began, so the caller knows
    /// the press is spoken for.
    pub(super) fn begin_nav_swipe(&mut self, pos: (f32, f32)) -> bool {
        let (px, py) = pos;
        let Some(&(_, pool)) = self.nav_targets.iter().rev().find(|(r, _)| {
            px >= r.x && px < r.x + NAV_SWIPE_EDGE.min(r.w) && py >= r.y && py < r.y + r.h
        }) else {
            return false;
        };
        let from = self.nav_pools[pool].current;
        if from == 0 || self.nav_pools[pool].anim.is_some() {
            return false;
        }
        self.nav_pools[pool].current = from - 1;
        self.nav_pools[pool].anim = Some(NavAnim {
            outgoing: from,
            incoming: from - 1,
            pop: true,
            motion: byard_core::frame::Motion::resting(0.0),
            gesture: true,
            gesture_p: 0.0,
        });
        self.nav_swipe = Some((pool, px));
        self.sync_nav_shared(pool);
        true
    }

    /// Tracks a live edge swipe: the pop's progress is the fraction of the
    /// container's width the finger has travelled.
    pub(super) fn drive_nav_swipe(&mut self, pos: (f32, f32)) {
        let Some((pool, start_x)) = self.nav_swipe else {
            return;
        };
        let width = self
            .nav_targets
            .iter()
            .find(|(_, p)| *p == pool)
            .map_or(0.0, |(r, _)| r.w);
        if width <= 0.0 {
            return;
        }
        let p = ((pos.0 - start_x) / width).clamp(0.0, 1.0);
        if let Some(anim) = self.nav_pools[pool].anim.as_mut() {
            anim.gesture_p = p;
        }
    }

    /// Releases an edge swipe (RFC-0026): past [`NAV_SWIPE_COMMIT`] the pop
    /// completes, otherwise it springs back. Either way the hand-off is the
    /// same, the gesture's progress becomes a spring's starting point, so the
    /// screen never jumps at the moment the finger lifts.
    pub(super) fn release_nav_swipe(&mut self) {
        let Some((pool, _)) = self.nav_swipe.take() else {
            return;
        };
        let Some(anim) = self.nav_pools[pool].anim else {
            return;
        };
        let p = anim.gesture_p;
        // The gesture already covered `p` of the distance, so the ramp that
        // finishes (or undoes) it runs only for the fraction still to travel: a
        // finger released a hair from the end must not sit through a whole
        // transition's worth of frames to cross the last few pixels.
        let remaining = if p >= NAV_SWIPE_COMMIT { 1.0 - p } else { p };
        let curve = nav_transition_tail(self.nav_pools[pool].transition, remaining);
        if p >= NAV_SWIPE_COMMIT {
            // Commit: finish the pop the finger started, and reflect the new
            // path so app state and screen agree (`current` is already there).
            self.nav_pools[pool].anim = Some(NavAnim {
                motion: byard_core::frame::Motion {
                    from: p,
                    to: 1.0,
                    start_ms: self.now_ms,
                    curve,
                },
                gesture: false,
                ..anim
            });
            self.reflect_nav_path(pool);
        } else {
            // Cancel: the same two screens, run the other way. A cancelled pop
            // is a push back to where we were, resumed at the mirrored
            // progress, which is why nothing on screen moves at the hand-off.
            self.nav_pools[pool].current = anim.outgoing;
            self.sync_nav_shared(pool);
            self.nav_pools[pool].anim = Some(NavAnim {
                outgoing: anim.incoming,
                incoming: anim.outgoing,
                pop: false,
                motion: byard_core::frame::Motion {
                    from: 1.0 - p,
                    to: 1.0,
                    start_ms: self.now_ms,
                    curve,
                },
                gesture: false,
                gesture_p: 0.0,
            });
        }
    }

    /// The shared navigation cell for the `var` `sig`, created on first use.
    fn nav_shared_cell(&mut self, sig: SignalId) -> NavSharedCell {
        std::rc::Rc::clone(self.nav_shared.entry(sig).or_default())
    }

    /// Delivers an OS URL intent to every `deep_link: true` navigation stack
    /// whose route table matches it (RFC-0026 §"Deep linking").
    ///
    /// The URL's *path* is what navigates, scheme and authority are the
    /// platform's business, so `byard://item/42`, `https://app.example/item/42`
    /// and `/item/42` all reach the same route. Returns `true` if any stack
    /// accepted the link; a URL no stack has a route for is rejected here rather
    /// than navigating something to a blank screen.
    ///
    /// This is the whole of the deep-link contract the compiler owns: the host
    /// registers the scheme with the OS and hands the URL here, and from this
    /// point on it is an ordinary navigation, the same push, the same
    /// transition, the same `route_change`.
    ///
    /// Only *mounted* containers can receive a link: a stack nested in a tab the
    /// app has never shown does not exist yet, so call this after the first
    /// render, and put the stack that should answer deep links in the tab the
    /// app starts on.
    pub fn apply_deep_link(&mut self, url: &str) -> bool {
        let path = deep_link_path(url);
        let mut accepted = false;
        for pool in 0..self.nav_pools.len() {
            if !self.nav_pools[pool].deep_link || self.match_route(pool, &path).is_none() {
                continue;
            }
            let Some(sig) = self.nav_pools[pool].path_sig else {
                continue;
            };
            self.ctx.write_signal(sig, Value::Str(path.clone()));
            accepted = true;
        }
        accepted
    }

    /// Whether any navigation container declares `deep_link: true`, what a host
    /// checks before registering a URL scheme with the OS.
    #[must_use]
    pub fn accepts_deep_links(&self) -> bool {
        self.nav_pools.iter().any(|p| p.deep_link)
    }

    /// The path currently shown by each navigation container, in declaration
    /// order, the observable navigation state, for tests and host tooling.
    #[must_use]
    pub fn nav_paths(&self) -> Vec<String> {
        self.nav_pools
            .iter()
            .map(|p| {
                p.entries
                    .get(p.current)
                    .map_or_else(String::new, |e| e.path.clone())
            })
            .collect()
    }

    /// The history depth of each navigation container, in declaration order.
    #[must_use]
    pub fn nav_depths(&self) -> Vec<usize> {
        self.nav_pools.iter().map(|p| p.entries.len()).collect()
    }
}
