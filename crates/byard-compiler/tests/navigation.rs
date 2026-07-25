//! RFC-0026 navigation & routing, end to end through the interpreter.
//!
//! The unit tests in `interp::nav` cover pattern matching and transition
//! geometry in isolation; these drive the real thing — lower a `.byd` source,
//! move its navigation `var`, render, and assert on the frame the engine would
//! ship: which screens exist, where they are, what survives a pop, and what the
//! diagnostics say when the source is wrong.

use byard_compiler::diagnostics::CompileError;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, PerfWarning};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::platform::{EventKind, InputEvent};

const W: f32 = 400.0;
const H: f32 = 800.0;

/// A lowered app: the interpreter plus its render tree, ready to drive.
struct App {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
}

impl App {
    /// Lowers `src`'s first `View`, asserting it parses cleanly.
    fn new(src: &str) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let mut interp = Interpreter::new();
        let names: Vec<String> = parsed.views.iter().map(|v| v.name.to_string()).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &refs);
        interp.tick();
        Self { interp, tree }
    }

    /// Renders one frame and returns it.
    fn frame(&mut self) -> RenderFrame {
        let mut frame = RenderFrame::new();
        self.interp.render(&self.tree, &mut frame, W, H);
        frame
    }

    /// Renders and returns the painted text runs, in emission order.
    fn texts(&mut self) -> Vec<String> {
        self.frame()
            .texts()
            .iter()
            .map(|t| t.text.clone())
            .collect()
    }

    /// The on-screen x of the text run whose content is `needle`.
    fn text_x(&mut self, needle: &str) -> Option<f32> {
        self.frame()
            .texts()
            .iter()
            .find(|t| t.text == needle)
            .map(|t| t.x)
    }

    /// Writes a `var` by name.
    fn set(&mut self, var: &str, value: &str) {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(var))
            .unwrap_or_else(|| panic!("no `var {var}`"));
        self.interp.write_var(sig, Value::Str(value.to_string()));
        self.interp.tick();
    }

    /// Reads a `var` by name as a string.
    fn get(&self, var: &str) -> String {
        let sig = self.interp.var_signal(&Symbol::intern(var)).unwrap();
        match self.interp.peek(sig) {
            Value::Str(s) => s,
            other => panic!("`{var}` is not a Str: {other:?}"),
        }
    }

    /// Advances the engine clock to `ms` and renders.
    fn at(&mut self, ms: u32) -> RenderFrame {
        self.interp.set_now_ms(ms);
        self.frame()
    }

    fn errors(&self) -> &[CompileError] {
        self.interp.errors()
    }
}

/// How many distinct unmatched-path warnings the app has raised.
fn unmatched(app: &App) -> usize {
    app.errors()
        .iter()
        .filter(|e| matches!(e, CompileError::UnmatchedRoute { .. }))
        .count()
}

fn pointer(kind: EventKind, pos: (f32, f32), t: u64) -> InputEvent {
    InputEvent {
        kind,
        pos,
        delta: (0.0, 0.0),
        payload: None,
        time_ms: t,
    }
}

/// A two-route stack whose detail screen reads its `:id` parameter.
const STACK: &str = r#"
View App() {
    var navPath = "/"
    NavStack(path: navPath) #[grow: 1] {
        route "/" { Text("home") }
        route "/detail/:id" {|params| Text("detail {params.id}") }
        route "/settings" { Text("settings") }
    }
}
"#;

// ── the core model: a `var` is the navigation ────────────────────────────────

#[test]
fn a_stack_mounts_its_first_route() {
    let mut app = App::new(STACK);
    assert_eq!(app.texts(), ["home"]);
    assert_eq!(app.interp.nav_paths(), ["/"]);
    assert_eq!(app.interp.nav_depths(), [1]);
    assert!(app.errors().is_empty(), "{:?}", app.errors());
}

#[test]
fn setting_the_path_pushes_and_binds_the_route_params() {
    let mut app = App::new(STACK);
    app.frame();
    app.set("navPath", "/detail/42");
    assert_eq!(app.texts(), ["detail 42"]);
    assert_eq!(app.interp.nav_depths(), [2], "the stack grew by one");
    assert!(app.errors().is_empty(), "{:?}", app.errors());
}

#[test]
fn setting_the_path_back_pops_to_the_preserved_entry() {
    let mut app = App::new(STACK);
    app.frame();
    app.set("navPath", "/detail/42");
    app.frame();
    app.set("navPath", "/");
    assert_eq!(app.texts(), ["home"]);
    assert_eq!(
        app.interp.nav_depths(),
        [1],
        "the detail entry is discarded"
    );
}

#[test]
fn a_multi_level_pop_discards_the_routes_it_skipped() {
    let mut app = App::new(STACK);
    app.frame();
    app.set("navPath", "/detail/1");
    app.frame();
    app.set("navPath", "/settings");
    app.frame();
    assert_eq!(app.interp.nav_depths(), [3]);
    app.set("navPath", "/");
    app.frame();
    // RFC-0026 §4: only the back target survives a multi-pop.
    assert_eq!(app.interp.nav_depths(), [1]);
    assert_eq!(app.texts(), ["home"]);
}

#[test]
fn a_pushed_screen_keeps_the_one_underneath_alive_with_its_state() {
    // The home screen owns a counter; pushing and popping must not reset it.
    let src = r#"
View App() {
    var navPath = "/"
    var count = 0
    NavStack(path: navPath) #[grow: 1] {
        route "/" { Text("home {count}") }
        route "/detail/:id" {|params| Text("detail {params.id}") }
    }
}
"#;
    let mut app = App::new(src);
    app.frame();
    let count = app.interp.var_signal(&Symbol::intern("count")).unwrap();
    app.interp.write_var(count, Value::Int(7));
    app.interp.tick();
    assert_eq!(app.texts(), ["home 7"]);
    app.set("navPath", "/detail/1");
    app.frame();
    app.set("navPath", "/");
    assert_eq!(
        app.texts(),
        ["home 7"],
        "the preserved screen kept its state"
    );
}

#[test]
fn re_pushing_the_same_route_with_new_params_is_a_new_screen() {
    let mut app = App::new(STACK);
    app.frame();
    app.set("navPath", "/detail/1");
    app.frame();
    app.set("navPath", "/detail/2");
    assert_eq!(app.texts(), ["detail 2"]);
    assert_eq!(
        app.interp.nav_depths(),
        [3],
        "each concrete path is its own entry"
    );
}

#[test]
fn the_route_record_exposes_the_path_and_its_params() {
    let src = r#"
View App() {
    var navPath = "/user/5/post/12"
    NavStack(path: navPath) #[grow: 1] {
        route "/user/:uid/post/:pid" { Text("{route.params.uid}/{route.params.pid} at {route.path}") }
    }
}
"#;
    let mut app = App::new(src);
    assert_eq!(app.texts(), ["5/12 at /user/5/post/12"]);
}

#[test]
fn routes_match_top_to_bottom_and_a_wildcard_catches_the_rest() {
    let src = r#"
View App() {
    var navPath = "/"
    NavStack(path: navPath) #[grow: 1] {
        route "/" { Text("home") }
        route "/detail/:id" {|params| Text("detail {params.id}") }
        route "*" { Text("not found") }
    }
}
"#;
    let mut app = App::new(src);
    app.frame();
    app.set("navPath", "/detail/9");
    assert_eq!(app.texts(), ["detail 9"], "the specific route wins");
    app.frame();
    app.set("navPath", "/nowhere/at/all");
    assert_eq!(app.texts(), ["not found"], "the catch-all takes the rest");
    assert!(
        app.errors().is_empty(),
        "a matched catch-all is not a warning"
    );
}

#[test]
fn an_unmatched_path_keeps_the_current_screen_and_warns_once() {
    let mut app = App::new(STACK);
    app.frame();
    app.set("navPath", "/nope");
    assert_eq!(app.texts(), ["home"], "the last matched route stays up");
    assert_eq!(unmatched(&app), 1);
    app.frame();
    app.frame();
    assert_eq!(
        unmatched(&app),
        1,
        "a steady-state mismatch warns once, not per frame"
    );
}

// ── transitions ──────────────────────────────────────────────────────────────

/// A slide-transition stack with distinguishable screens.
const SLIDING: &str = r#"
View App() {
    var navPath = "/"
    NavStack(path: navPath) #[transition: slide, grow: 1] {
        route "/" { Text("home") }
        route "/detail" { Text("detail") }
    }
}
"#;

#[test]
fn a_push_keeps_both_screens_alive_until_the_transition_settles() {
    let mut app = App::new(SLIDING);
    app.at(0);
    app.set("navPath", "/detail");

    // Mid-transition both screens paint, the incoming one still off to the right.
    let mid = app.at(40);
    let texts: Vec<&str> = mid.texts().iter().map(|t| t.text.as_str()).collect();
    assert!(
        texts.contains(&"home") && texts.contains(&"detail"),
        "both screens are alive mid-transition: {texts:?}"
    );
    let incoming = mid.texts().iter().find(|t| t.text == "detail").unwrap().x;
    assert!(
        incoming > W * 0.25,
        "the arriving screen is still off to the right: {incoming}"
    );

    // Once it settles only the incoming screen remains, at rest.
    app.at(2_000);
    assert_eq!(app.texts(), ["detail"]);
    assert!(
        app.text_x("detail").unwrap() < 1.0,
        "settled flush to the left"
    );
    assert!(
        !app.interp.has_active_animations(),
        "and asks for no more frames"
    );
}

#[test]
fn the_incoming_screen_slides_monotonically_into_place() {
    let mut app = App::new(SLIDING);
    app.at(0);
    app.set("navPath", "/detail");
    let mut previous = f32::MAX;
    for ms in [20, 60, 100, 160, 240] {
        app.at(ms);
        let x = app
            .frame()
            .texts()
            .iter()
            .find(|t| t.text == "detail")
            .map(|t| t.x)
            .expect("the incoming screen paints every frame");
        assert!(
            x <= previous + 0.5,
            "the slide never goes backwards: {x} after {previous}"
        );
        previous = x;
    }
    assert!(
        previous < W * 0.25,
        "and gets most of the way home: {previous}"
    );
}

#[test]
fn transition_none_swaps_instantly_and_requests_no_frames() {
    let src = SLIDING.replace("transition: slide", "transition: none");
    let mut app = App::new(&src);
    app.at(0);
    app.set("navPath", "/detail");
    assert_eq!(app.at(1).texts().len(), 1, "only the new screen paints");
    assert!(!app.interp.has_active_animations());
}

#[test]
fn a_transition_keeps_requesting_frames_while_it_runs() {
    let mut app = App::new(SLIDING);
    app.at(0);
    app.set("navPath", "/detail");
    app.at(30);
    assert!(
        app.interp.has_active_animations(),
        "a moving screen needs frames"
    );
}

// ── tabs ─────────────────────────────────────────────────────────────────────

const TABS: &str = r#"
View App() {
    var activeTab = "home"
    var typed = "x"
    NavHost(active: activeTab) #[grow: 1] {
        tab "home" { Text("home {typed}") }
        tab "search" { Text("search") }
        tab "profile" { Text("profile") }
    }
}
"#;

#[test]
fn a_host_shows_one_tab_at_a_time() {
    let mut app = App::new(TABS);
    assert_eq!(app.texts(), ["home x"]);
    app.frame();
    app.set("activeTab", "search");
    assert_eq!(app.texts(), ["search"]);
}

#[test]
fn tabs_are_preserved_and_never_re_instantiated() {
    let mut app = App::new(TABS);
    app.frame();
    let typed = app.interp.var_signal(&Symbol::intern("typed")).unwrap();
    app.interp.write_var(typed, Value::Str("hello".into()));
    app.interp.tick();
    assert_eq!(app.texts(), ["home hello"]);

    app.set("activeTab", "profile");
    app.frame();
    app.set("activeTab", "home");
    assert_eq!(app.texts(), ["home hello"], "the tab kept its state");
    // Three tabs visited ⇒ three preserved entries, and no more.
    app.set("activeTab", "search");
    app.frame();
    app.set("activeTab", "home");
    app.frame();
    assert_eq!(app.interp.nav_depths(), [3]);
}

#[test]
fn an_unvisited_tab_is_never_instantiated() {
    let mut app = App::new(TABS);
    app.frame();
    // Only the mounted tab exists — the other two cost nothing until visited.
    assert_eq!(app.interp.nav_depths(), [1]);
}

#[test]
fn a_stack_inside_a_tab_keeps_its_own_history() {
    let src = r#"
View App() {
    var tab = "home"
    var homePath = "/"
    var profilePath = "/"
    NavHost(active: tab) #[grow: 1] {
        tab "home" {
            NavStack(path: homePath) #[grow: 1] {
                route "/" { Text("home root") }
                route "/deep" { Text("home deep") }
            }
        }
        tab "profile" {
            NavStack(path: profilePath) #[grow: 1] {
                route "/" { Text("profile root") }
            }
        }
    }
}
"#;
    let mut app = App::new(src);
    app.frame();
    app.set("homePath", "/deep");
    assert_eq!(app.texts(), ["home deep"]);
    app.frame();
    app.set("tab", "profile");
    assert_eq!(app.texts(), ["profile root"]);
    app.frame();
    app.set("tab", "home");
    assert_eq!(
        app.texts(),
        ["home deep"],
        "the nested stack kept its own position"
    );
    assert_eq!(
        app.interp.nav_depths(),
        [2, 2, 1],
        "host, home stack, profile stack"
    );
}

// ── navigation actions ───────────────────────────────────────────────────────

#[test]
fn back_pops_the_stack_and_is_a_no_op_at_the_root() {
    let src = r#"
View App() {
    var navPath = "/"
    Column #[grow: 1] {
        Button("back") => back(navPath)
        NavStack(path: navPath) #[grow: 1] {
            route "/" { Text("home") }
            route "/detail" { Text("detail") }
        }
    }
}
"#;
    let mut app = App::new(src);
    app.frame();
    app.set("navPath", "/detail");
    app.frame();

    let rects = app.interp.router.handler_rects();
    let (_, _, r) = rects
        .iter()
        .copied()
        .find(|(_, k, _)| matches!(k, byard_compiler::interp::events::EventKind::Tap))
        .expect("the back button registered a tap");
    let hit = (r.x + r.w / 2.0, r.y + r.h / 2.0);

    app.interp.dispatch_events(&[
        pointer(EventKind::PointerDown, hit, 0),
        pointer(EventKind::Tap, hit, 1),
    ]);
    app.interp.tick();
    assert_eq!(app.get("navPath"), "/", "back reflected into the var");
    assert_eq!(app.texts(), ["back", "home"]);

    // At the root there is nothing to pop: the tap is inert, not an error.
    app.frame();
    app.interp.dispatch_events(&[
        pointer(EventKind::PointerDown, hit, 2),
        pointer(EventKind::Tap, hit, 3),
    ]);
    app.interp.tick();
    assert_eq!(app.get("navPath"), "/");
    assert_eq!(app.interp.nav_depths(), [1]);
}

#[test]
fn replace_swaps_the_top_without_growing_the_stack() {
    let src = r#"
View App() {
    var navPath = "/"
    Column #[grow: 1] {
        Button("go") => replace(navPath, "/settings")
        NavStack(path: navPath) #[grow: 1] {
            route "/" { Text("home") }
            route "/detail" { Text("detail") }
            route "/settings" { Text("settings") }
        }
    }
}
"#;
    let mut app = App::new(src);
    app.frame();
    app.set("navPath", "/detail");
    app.frame();
    assert_eq!(app.interp.nav_depths(), [2]);

    let rects = app.interp.router.handler_rects();
    let (_, _, r) = rects
        .iter()
        .copied()
        .find(|(_, k, _)| matches!(k, byard_compiler::interp::events::EventKind::Tap))
        .unwrap();
    let hit = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    app.interp.dispatch_events(&[
        pointer(EventKind::PointerDown, hit, 0),
        pointer(EventKind::Tap, hit, 1),
    ]);
    app.interp.tick();

    assert_eq!(app.texts(), ["go", "settings"]);
    assert_eq!(
        app.interp.nav_depths(),
        [2],
        "the replaced entry is gone, not buried"
    );
    app.frame();
    app.set("navPath", "/");
    assert_eq!(
        app.texts(),
        ["go", "home"],
        "and `/` is still directly below"
    );
}

#[test]
fn route_change_fires_once_a_navigation_settles() {
    let src = r#"
View App() {
    var navPath = "/"
    var seen = ""
    NavStack(path: navPath) #[grow: 1, transition: none, route_change(e) => seen = e] {
        route "/" { Text("home") }
        route "/detail" { Text("detail") }
    }
}
"#;
    let mut app = App::new(src);
    app.frame();
    app.interp.dispatch_events(&[]);
    app.interp.tick();
    assert_eq!(app.get("seen"), "", "mounting is not a navigation");

    app.set("navPath", "/detail");
    app.frame();
    app.interp.dispatch_events(&[]);
    app.interp.tick();
    assert_eq!(app.get("seen"), "/detail");
}

// ── deep linking ─────────────────────────────────────────────────────────────

#[test]
fn a_deep_link_url_navigates_a_stack_that_opted_in() {
    let src = r#"
View App() {
    var navPath = "/"
    NavStack(path: navPath) #[grow: 1, deep_link: true] {
        route "/" { Text("home") }
        route "/item/:id" {|params| Text("item {params.id}") }
    }
}
"#;
    let mut app = App::new(src);
    app.frame();
    assert!(app.interp.accepts_deep_links());
    assert!(app.interp.apply_deep_link("byard://item/42"));
    app.interp.tick();
    assert_eq!(app.texts(), ["item 42"]);
    assert_eq!(app.get("navPath"), "/item/42", "reflected into the var");
}

#[test]
fn deep_link_urls_are_accepted_in_every_platform_spelling() {
    let src = r#"
View App() {
    var navPath = "/"
    NavStack(path: navPath) #[grow: 1, deep_link: true] {
        route "/" { Text("home") }
        route "/item/:id" {|params| Text("item {params.id}") }
    }
}
"#;
    for url in [
        "byard://item/7",
        "https://app.example/item/7",
        "/item/7",
        "byard://item/7?utm=x",
        "byard://item/7/",
    ] {
        let mut app = App::new(src);
        app.frame();
        assert!(app.interp.apply_deep_link(url), "{url} should be accepted");
        app.interp.tick();
        assert_eq!(app.texts(), ["item 7"], "{url}");
    }
}

#[test]
fn a_url_no_route_matches_is_rejected_rather_than_blanking_the_app() {
    let src = r#"
View App() {
    var navPath = "/"
    NavStack(path: navPath) #[grow: 1, deep_link: true] {
        route "/" { Text("home") }
    }
}
"#;
    let mut app = App::new(src);
    app.frame();
    assert!(!app.interp.apply_deep_link("byard://nowhere"));
    assert_eq!(app.texts(), ["home"]);
    assert!(
        app.errors().is_empty(),
        "a rejected link is not a route warning"
    );
}

#[test]
fn a_stack_that_did_not_opt_in_ignores_deep_links() {
    let mut app = App::new(STACK);
    app.frame();
    assert!(!app.interp.accepts_deep_links());
    assert!(!app.interp.apply_deep_link("byard://detail/1"));
    assert_eq!(app.texts(), ["home"]);
}

// ── swipe-back ───────────────────────────────────────────────────────────────

const SWIPEABLE: &str = r#"
View App() {
    var navPath = "/"
    NavStack(path: navPath) #[grow: 1, swipe_back: true] {
        route "/" { Text("home") }
        route "/detail" { Text("detail") }
    }
}
"#;

#[test]
fn an_edge_swipe_past_halfway_completes_the_pop() {
    let mut app = App::new(SWIPEABLE);
    app.at(0);
    app.set("navPath", "/detail");
    app.at(1); // the push starts on the next render…
    app.at(2_000); // …and is long settled by here
    assert_eq!(app.texts(), ["detail"]);

    app.interp.dispatch_events(&[
        pointer(EventKind::PointerDown, (4.0, H / 2.0), 0),
        pointer(EventKind::PointerMove, (W * 0.7, H / 2.0), 16),
    ]);
    // Mid-gesture both screens are alive and the revealed one is on its way in.
    let dragging = app.at(2_016);
    let texts: Vec<&str> = dragging.texts().iter().map(|t| t.text.as_str()).collect();
    assert!(
        texts.contains(&"home") && texts.contains(&"detail"),
        "{texts:?}"
    );

    app.interp
        .dispatch_events(&[pointer(EventKind::PointerUp, (W * 0.7, H / 2.0), 32)]);
    app.interp.tick();
    app.at(4_000);
    assert_eq!(app.texts(), ["home"], "the pop completed");
    assert_eq!(app.get("navPath"), "/", "and reflected into the var");
    assert_eq!(app.interp.nav_depths(), [1]);
}

#[test]
fn a_short_edge_swipe_springs_back_to_the_screen_it_started_on() {
    let mut app = App::new(SWIPEABLE);
    app.at(0);
    app.set("navPath", "/detail");
    app.at(1);
    app.at(2_000);

    app.interp.dispatch_events(&[
        pointer(EventKind::PointerDown, (4.0, H / 2.0), 0),
        pointer(EventKind::PointerMove, (W * 0.2, H / 2.0), 16),
        pointer(EventKind::PointerUp, (W * 0.2, H / 2.0), 32),
    ]);
    app.interp.tick();
    app.at(4_000);
    assert_eq!(app.texts(), ["detail"], "back where it started");
    assert_eq!(app.get("navPath"), "/detail");
    assert_eq!(app.interp.nav_depths(), [2], "and nothing was discarded");
}

#[test]
fn a_press_away_from_the_edge_is_not_a_swipe() {
    let mut app = App::new(SWIPEABLE);
    app.at(0);
    app.set("navPath", "/detail");
    app.at(1);
    app.at(2_000);
    app.interp.dispatch_events(&[
        pointer(EventKind::PointerDown, (W / 2.0, H / 2.0), 0),
        pointer(EventKind::PointerMove, (W * 0.9, H / 2.0), 16),
        pointer(EventKind::PointerUp, (W * 0.9, H / 2.0), 32),
    ]);
    app.interp.tick();
    app.at(4_000);
    assert_eq!(app.texts(), ["detail"]);
}

#[test]
fn swipe_back_is_off_unless_asked_for() {
    let mut app = App::new(SLIDING);
    app.at(0);
    app.set("navPath", "/detail");
    app.at(1);
    app.at(2_000);
    app.interp.dispatch_events(&[
        pointer(EventKind::PointerDown, (4.0, H / 2.0), 0),
        pointer(EventKind::PointerMove, (W * 0.9, H / 2.0), 16),
        pointer(EventKind::PointerUp, (W * 0.9, H / 2.0), 32),
    ]);
    app.interp.tick();
    app.at(4_000);
    assert_eq!(app.texts(), ["detail"]);
}

// ── guards & diagnostics ─────────────────────────────────────────────────────

#[test]
fn a_runaway_push_is_refused_at_max_depth_and_reflected_back() {
    let src = r#"
View App() {
    var navPath = "/"
    NavStack(path: navPath) #[grow: 1, max_depth: 3, transition: none] {
        route "/" { Text("home") }
        route "/x/:n" {|params| Text("x {params.n}") }
    }
}
"#;
    let mut app = App::new(src);
    app.frame();
    for n in 1..=2 {
        app.set("navPath", &format!("/x/{n}"));
        app.frame();
    }
    assert_eq!(app.interp.nav_depths(), [3]);

    app.set("navPath", "/x/3");
    app.frame();
    assert!(
        app.interp
            .perf_warnings()
            .iter()
            .any(|w| matches!(w, PerfWarning::DeepNavStack { depth: 3, .. })),
        "the depth guard is surfaced on the frame the push is refused: {:?}",
        app.interp.perf_warnings()
    );
    assert_eq!(app.interp.nav_depths(), [3], "the push was refused");
    assert_eq!(app.texts(), ["x 2"], "still on the screen it was showing");
    assert_eq!(
        app.get("navPath"),
        "/x/2",
        "and the var agrees with the screen"
    );
}

#[test]
fn max_depth_zero_disables_the_guard() {
    let src = r#"
View App() {
    var navPath = "/"
    NavStack(path: navPath) #[grow: 1, max_depth: 0, transition: none] {
        route "/" { Text("home") }
        route "/x/:n" {|params| Text("x {params.n}") }
    }
}
"#;
    let mut app = App::new(src);
    app.frame();
    for n in 1..=12 {
        app.set("navPath", &format!("/x/{n}"));
        app.frame();
    }
    assert_eq!(app.interp.nav_depths(), [13]);
    assert!(app.interp.perf_warnings().is_empty());
}

/// Lowers `src` and returns the diagnostics its nav containers produce.
fn diagnose(src: &str) -> Vec<CompileError> {
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let _ = interp.lower_view(&parsed.views[0], &[]);
    let mut errs: Vec<CompileError> = parsed.errors;
    errs.extend(interp.errors().iter().cloned());
    errs
}

#[test]
fn a_route_outside_a_nav_stack_is_diagnosed() {
    let errs = diagnose(r#"View App() { Column { route "/" { Text("x") } } }"#);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            CompileError::MisplacedNavCase { keyword, container, .. }
                if keyword == "route" && container == "NavStack"
        )),
        "{errs:?}"
    );
}

#[test]
fn a_tab_inside_a_nav_stack_is_diagnosed() {
    let errs =
        diagnose(r#"View App() { var p = "/" NavStack(path: p) { tab "home" { Text("x") } } }"#);
    assert!(
        errs.iter().any(
            |e| matches!(e, CompileError::MisplacedNavCase { keyword, .. } if keyword == "tab")
        ),
        "{errs:?}"
    );
}

#[test]
fn an_element_inside_a_nav_stack_is_diagnosed() {
    let errs = diagnose(r#"View App() { var p = "/" NavStack(path: p) { Text("stray") } }"#);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            CompileError::NavCaseRequired { container, keyword, .. }
                if container == "NavStack" && keyword == "route"
        )),
        "{errs:?}"
    );
}

#[test]
fn a_malformed_route_pattern_is_diagnosed() {
    for bad in ["/detail/:", "/*/tail", "/a/:id/b/:id"] {
        let src = format!(
            r#"View App() {{ var p = "/" NavStack(path: p) {{ route "{bad}" {{ Text("x") }} }} }}"#
        );
        let errs = diagnose(&src);
        assert!(
            errs.iter()
                .any(|e| matches!(e, CompileError::InvalidRoutePattern { .. })),
            "{bad}: {errs:?}"
        );
    }
}

#[test]
fn an_interpolated_route_pattern_is_rejected_at_parse_time() {
    let parsed =
        parse(r#"View App() { var p = "/" NavStack(path: p) { route "/x/{p}" { Text("x") } } }"#);
    assert!(
        !parsed.errors.is_empty(),
        "a route table is fixed at mount time, so an interpolated pattern is an error"
    );
}

#[test]
fn a_nav_container_without_its_navigation_state_is_an_arity_error() {
    let errs = diagnose(r#"View App() { NavStack { route "/" { Text("x") } } }"#);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            CompileError::ArityMismatch { name, .. } if name == "NavStack"
        )),
        "{errs:?}"
    );
}

#[test]
fn an_unknown_transition_token_is_diagnosed() {
    let errs = diagnose(
        r#"View App() { var p = "/" NavStack(path: p) #[transition: slyde] { route "/" { Text("x") } } }"#,
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::AttributeTypeMismatch { .. })),
        "{errs:?}"
    );
}

// ── the container is an ordinary layout node ─────────────────────────────────

#[test]
fn a_nav_container_paints_its_own_background_and_hosts_siblings() {
    let src = r#"
View App() {
    var navPath = "/"
    Column #[grow: 1] {
        Text("chrome above")
        NavStack(path: navPath) #[grow: 1, bg: 0x101014] {
            route "/" { Text("screen") }
        }
        Text("chrome below")
    }
}
"#;
    let mut app = App::new(src);
    let frame = app.frame();
    let texts: Vec<&str> = frame.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, ["chrome above", "screen", "chrome below"]);
    assert!(
        frame
            .instances()
            .iter()
            .any(|b| (b.color[2] - 20.0 / 255.0).abs() < 0.2 && b.rect[3] > 0.0),
        "the container painted its own surface"
    );
    // The chrome is laid out around the container, not on top of it.
    let above = frame
        .texts()
        .iter()
        .find(|t| t.text == "chrome above")
        .unwrap();
    let screen = frame.texts().iter().find(|t| t.text == "screen").unwrap();
    let below = frame
        .texts()
        .iter()
        .find(|t| t.text == "chrome below")
        .unwrap();
    assert!(above.y < screen.y && screen.y < below.y);
}
