//! Wiring elements to the outside: input events and native views
//! (RFC-0012, RFC-0039).
//!
//! At paint, an element's `tap`, `pointer_*`, `key_*`, `focus` and the other
//! event attributes are lowered to actions and registered on the router
//! against the element's rect, together with the focus sugar that rides a
//! `focused:` signal. `dispatch_events` is the other direction: it takes the
//! platform's input events, lets the IME and the native views see them first,
//! and routes the rest.
//!
//! A package element is drawn by a native view the host provides. The
//! interpreter owns the instance for as long as the lowered node lives
//! (mounting it at lower time, so a re-lower is its unmount), hands it the
//! props the element wrote, offers it the pointer events that land on it, and
//! places the controller requests it issued through the same queue and
//! dispatcher an action's call uses.

use super::scroll::ScrollDrag;
use super::{Interpreter, PerfWarning};
use crate::diagnostics::CompileError;
use crate::interp::env::Value;
use crate::parser::ast::{Attr, AttrKind};

/// Stands in for a view whose registration went missing between checking and
/// lowering (RFC-0039).
///
/// It draws nothing, and it exists so the slot table stays index-aligned with
/// the lowered tree: the diagnostic beside it is what tells the author, and a
/// missing entry here would silently shift every later view's slot onto the
/// wrong instance.
struct MissingNativeView;

impl byard_core::render::NativeProps for MissingNativeView {}

impl byard_core::render::NativeView for MissingNativeView {
    fn render(
        &mut self,
        _layout: byard_core::render::Layout,
        _cx: &mut byard_core::render::RenderCtx<'_>,
    ) {
    }
}

/// One outstanding controller request a native view issued (RFC-0039).
#[derive(Clone, Copy)]
pub(super) struct NativeRequest {
    /// Which view asked.
    pub(super) slot: usize,
    /// The view table's generation when it asked.
    pub(super) generation: u64,
    /// The key the view chose, handed back with the answer.
    pub(super) key: byard_core::render::RequestKey,
}

/// A native view laid out in the last render (RFC-0039), and where.
///
/// The event half of the ABI: an input event is offered to the views under the
/// pointer, innermost first, and a view that says [`Handled::Yes`] stops it, so
/// a package widget takes part in routing under the rules an intrinsic follows
/// rather than beside them.
///
/// [`Handled::Yes`]: byard_core::render::Handled::Yes
#[derive(Clone, Copy)]
pub(super) struct NativeTarget {
    /// Which live view, by slot.
    pub(super) slot: usize,
    /// Its on-screen rect in logical pixels.
    pub(super) rect: crate::interp::intrinsics::Rect,
    /// How many event handlers had been registered when the view was drawn.
    /// Handlers are registered in paint order, so any registered after this
    /// point belong to elements drawn over the view.
    pub(super) drawn_at: usize,
}

impl Interpreter {
    /// Registers handlers for all event-kind attrs (`#[tap => …]`, etc.).
    /// Places this frame's native-view controller requests (RFC-0039).
    ///
    /// A view named a controller and a method; from here the request is
    /// indistinguishable from one an action raised, because it *is* one: the
    /// same queue, the same dispatcher, the same reply channel. What is extra
    /// is the note of who to hand the answer to, which is the whole of the
    /// "async across the boundary" surface.
    pub(super) fn dispatch_native_calls(&mut self, frame: &mut byard_core::frame::RenderFrame) {
        let owners = std::mem::take(&mut self.native_call_owners);
        if owners.is_empty() {
            // Nothing asked for anything, which is the steady state of every
            // widget that is not fetching something.
            frame.take_native_calls();
            return;
        }
        let calls = frame.take_native_calls();
        for (slot, range) in owners {
            for call in calls.get(range).into_iter().flatten() {
                let Some(controller) = self.controller_id_by_name(&call.controller) else {
                    // A view asking for a controller the app never provided is
                    // an assembly mistake, and a silent no-answer is the worst
                    // possible way to learn about it.
                    self.perf_warnings.push(PerfWarning::UnprovidedNativeCall {
                        controller: call.controller.clone(),
                        method: call.method.clone(),
                    });
                    continue;
                };
                let continuation = self.open_native_continuation(slot, call.key);
                self.queue_native_call(controller, &call.method, call.args.clone(), continuation);
            }
        }
        self.drain_calls();
    }

    /// Offers each event to the native views under the pointer, innermost
    /// first, and reports which events a view took (RFC-0039 × RFC-0003).
    ///
    /// An element drawn over a view that takes presses where the pointer is
    /// (a button over a map) gets the event instead: the view is not offered
    /// it at all, as an intrinsic underneath would not be.
    ///
    /// Innermost first is "last recorded first": the render walk is
    /// depth-first, so a view nested inside another was pushed after it, and
    /// walking the targets in reverse is the same front-to-back order the
    /// router's own hit testing uses.
    ///
    /// A keyboard, text or change event has no position (hosts send it at the
    /// origin), so it is offered to no view here; a view that wants keys gets
    /// them the way an intrinsic does, through the focus path, and inventing a
    /// second rule for package elements is exactly the divergence this ABI
    /// exists to avoid.
    fn dispatch_to_native_views(&mut self, events: &[byard_core::InputEvent]) -> Vec<bool> {
        use byard_core::render::{Event as ViewEvent, Handled, Layout};

        let mut consumed = vec![false; events.len()];
        if self.native_targets.is_empty() {
            return consumed;
        }
        let targets = self.native_targets.clone();
        for (i, ev) in events.iter().enumerate() {
            if !ev.kind.is_positional() {
                continue;
            }
            let (px, py) = ev.pos;
            for target in targets.iter().rev() {
                let r = target.rect;
                if px < r.x || px >= r.x + r.w || py < r.y || py >= r.y + r.h {
                    continue;
                }
                // Something drawn over the view that takes presses here gets
                // this one: what is on top is what the pointer is on.
                if self.router.claims_pointer_from((px, py), target.drawn_at) {
                    break;
                }
                let Some(view) = self.native_views.get_mut(target.slot) else {
                    continue;
                };
                let layout = Layout::new([r.x, r.y, r.w, r.h]);
                let handled = view.on_event(
                    &ViewEvent {
                        kind: ev.kind,
                        local: layout.local((px, py)),
                        delta: ev.delta,
                        payload: ev.payload.clone(),
                    },
                    layout,
                );
                if handled == Handled::Yes {
                    consumed[i] = true;
                    break;
                }
            }
        }
        consumed
    }

    /// Makes this element's native-view instance and returns its slot
    /// (RFC-0039).
    ///
    /// Called at lower time, which is the element's birth: the view exists for
    /// as long as this lowered node does, and a re-lower (a hot reload, a
    /// structural change) drops it, which is its unmount. There is no separate
    /// bookkeeping to keep in step, and therefore nothing to get out of step
    /// (an extension never keeps a resource alive past its owning scope).
    pub(super) fn mount_native_view(
        &mut self,
        name: &str,
        span: crate::diagnostics::Span,
    ) -> usize {
        let slot = self.native_views.len();
        if let Some(view) = byard_core::render::registry::create(name) {
            self.native_views.push(view);
        } else {
            // The catalog answered for this name a moment ago, so failing here
            // means the registry changed underneath the lowering, which is not
            // something an app can do by accident. Said with a span rather
            // than papered over with a blank element (no silent failures).
            self.errors.push(CompileError::UnknownView {
                span,
                name: name.to_string(),
                hint: None,
            });
            self.native_views.push(Box::new(MissingNativeView));
        }
        slot
    }

    /// Evaluates this element's declared props and hands them to its view
    /// (RFC-0039).
    ///
    /// Only the props the view declared, and only the ones the element wrote:
    /// an unwritten prop is the view's own initial value, not a `None` it has
    /// to interpret, and an unknown one was already a compile error with a
    /// span before lowering ever reached here.
    pub(super) fn apply_native_props(&mut self, name: &str, slot: usize, attrs: &[Attr]) {
        let Some(info) = byard_core::render::registry::info(name) else {
            return;
        };
        for prop in info.props {
            let Some(attr) = attrs.iter().find(|a| a.name.as_str() == prop.name) else {
                continue;
            };
            let AttrKind::Prop { value } = &attr.kind else {
                continue;
            };
            let evaluated = self.eval_pure(value);
            // A signal, a memo or a callback has no data form, and a view is
            // data-only by construction (only `Send` data crosses the boundary,
            // never a signal, callback or view handle): the same rule the controller
            // boundary follows, for the same reason.
            let Some(host) = super::bridge::value_to_host(&evaluated) else {
                self.errors.push(CompileError::NonDataViewProp {
                    span: attr.span,
                    prop: prop.name.to_string(),
                    view: name.to_string(),
                });
                continue;
            };
            if let Some(view) = self.native_views.get_mut(slot) {
                view.set_prop(prop.name, &host);
            }
        }
    }

    /// The actions written on a field for the events in `names`, lowered.
    /// The handler's own payload name (`input(text) => …`) is honoured.
    pub(super) fn field_actions(
        &mut self,
        attrs: &[Attr],
        names: &[&str],
    ) -> Vec<super::events::Action> {
        let mut out = Vec::new();
        for attr in attrs {
            if let AttrKind::Event { payload, action } = &attr.kind {
                if names.contains(&attr.name.as_str()) {
                    if let Ok(lowered) = self.lower_action(action, payload.clone()) {
                        out.push(lowered);
                    }
                }
            }
        }
        out
    }

    pub(super) fn register_event_attrs(
        &mut self,
        attrs: &[Attr],
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
    ) {
        for attr in attrs {
            if let AttrKind::Event { payload, action } = &attr.kind {
                let event_kind = match attr.name.as_str() {
                    "tap" | "click" => super::events::EventKind::Tap, // "click" is an alias (RFC-0012 §A)
                    "pointer_down" => super::events::EventKind::PointerDown,
                    "pointer_up" => super::events::EventKind::PointerUp,
                    "pointer_move" => super::events::EventKind::PointerMove,
                    "scroll" => super::events::EventKind::Scroll,
                    "wheel" => super::events::EventKind::Wheel,
                    // RFC-0021 advanced scroll behaviours (engine-fired).
                    "end_reached" => super::events::EventKind::EndReached,
                    "page_change" => super::events::EventKind::PageChange,
                    "scroll_end" => super::events::EventKind::ScrollEnd,
                    "refresh" => super::events::EventKind::Refresh,
                    // RFC-0026: fired when a navigation settles.
                    "route_change" => super::events::EventKind::RouteChange,
                    "change" => super::events::EventKind::Change,
                    "key_down" => super::events::EventKind::KeyDown,
                    "key_up" => super::events::EventKind::KeyUp,
                    "text_input" => super::events::EventKind::TextInput,
                    // RFC-0012 §A: the six modeled-but-previously-unexposed events.
                    "hover" => super::events::EventKind::Hover,
                    "pointer_enter" => super::events::EventKind::PointerEnter,
                    "pointer_exit" => super::events::EventKind::PointerExit,
                    "long_press" => super::events::EventKind::LongPress,
                    "double_tap" => super::events::EventKind::DoubleTap,
                    "secondary" => super::events::EventKind::Secondary,
                    // RFC-0012 S2: `focus =>`/`blur =>` sugar over `focused_sig`'s
                    // edges, registered as ordinary handlers here; `steal_focus`
                    // fires them directly (see `interp::events::EventKind::Focus`).
                    "focus" => super::events::EventKind::Focus,
                    "blur" => super::events::EventKind::Blur,
                    _ => continue,
                };
                if let Ok(closure) = self.lower_action(action, payload.clone()) {
                    if let Some(idx) = elem_idx {
                        self.router.on(idx, hit_rect, event_kind, closure);
                    }
                }
            }
        }
    }

    /// Registers an element as focusable if it has a `focused:` prop attr
    /// **or** a `focus =>`/`blur =>` handler (RFC-0012 S2), the
    /// sugar rides `focused_sig`'s edges, so an element that only wants the
    /// one-shot event (no bound `var`) still needs a signal for
    /// `steal_focus` to flip. That signal is a fresh internal one when
    /// `focused:` wasn't given, mirroring `render_text_field`'s same
    /// bind-or-create pattern for its own `focused_sig`.
    pub(super) fn register_focusable(
        &mut self,
        attrs: &[Attr],
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
    ) {
        // Without an index there is nowhere to register the focusable, so a
        // freshly created internal signal below would just be dropped,
        // bail out first rather than allocate one for nothing.
        let Some(idx) = elem_idx else {
            return;
        };
        let has_focus_sugar = attrs.iter().any(|a| {
            matches!(a.kind, AttrKind::Event { .. }) && matches!(a.name.as_str(), "focus" | "blur")
        });
        let sig = self
            .resolve_focused_sig(attrs)
            .or_else(|| has_focus_sugar.then(|| self.ctx.create_signal(Value::Bool(false))));
        if let Some(sig) = sig {
            self.router.focusable(idx, hit_rect, sig);
        }
    }

    /// Converts winit-sourced input events to interpreter event payloads and dispatches them to the `EventRouter`.
    pub fn dispatch_events(&mut self, events: &[byard_core::InputEvent]) {
        use crate::interp::events::{EventKind as CompKind, InputEvent as CompEvent};
        use byard_core::platform::{EventKind as CoreKind, InputPayload};

        /// Logical pixels a `ScrollView` scrolls per wheel line (RFC-0005).
        const WHEEL_LINE_PX: f32 = 40.0;

        // RFC-0030 §I1: hit-testing, gesture recognition and handler
        // invocation, all interpreter work, all gone in an AOT build.
        byard_core::profile_scope!(
            "interp.dispatch_events",
            byard_core::telemetry::ScopeKind::Interpreter
        );

        // RFC-0039: offer each event to the native views under the pointer,
        // innermost first, before anything else looks at it. A view that
        // handles one stops it there, which is the rule an intrinsic's handler
        // follows; a view that declines is invisible to the rest of routing,
        // which is the rule an element with no listener follows (RFC-0003).
        let mut consumed = self.dispatch_to_native_views(events);
        // RFC-0040: composition is engine state, applied here rather than
        // routed to an app handler, and never offered to anything else.
        self.apply_composition(events, &mut consumed);
        // The system's appearance is the theme's to follow, when it was told
        // to (`scheme = "system"`), and nothing for any element to route.
        // An assistive technology's requests to move focus or to scroll
        // (RFC-0042 §5) are engine state in the same way: applied here,
        // never offered to a handler.
        for (i, ev) in events.iter().enumerate() {
            match ev.kind {
                CoreKind::WindowFocus => {
                    consumed[i] = true;
                    if let Some(InputPayload::Bool(focused)) = ev.payload.as_ref() {
                        self.caret.window_blurred = !*focused;
                    }
                }
                CoreKind::SystemScheme => {
                    consumed[i] = true;
                    if let (true, Some(InputPayload::Bool(dark))) =
                        (self.theme.follows_system, ev.payload.as_ref())
                    {
                        if self.theme_is_dark() != *dark {
                            self.set_theme_dark(*dark);
                        }
                    }
                }
                CoreKind::AccessFocus => {
                    consumed[i] = true;
                    if let Some(InputPayload::Element(elem)) = ev.payload {
                        self.router.focus_element(&mut self.ctx, elem);
                    }
                }
                CoreKind::AccessScrollIntoView => {
                    consumed[i] = true;
                    self.scroll_into_view(ev.pos, ev.delta);
                }
                _ => {}
            }
        }

        let comp_events: Vec<CompEvent> = events
            .iter()
            .enumerate()
            .filter(|(i, _)| !consumed[*i])
            .map(|(_, ev)| {
                let kind = match ev.kind {
                    CoreKind::PointerDown => CompKind::PointerDown,
                    CoreKind::PointerUp => CompKind::PointerUp,
                    CoreKind::Tap => CompKind::Tap,
                    CoreKind::PointerMove => CompKind::PointerMove,
                    CoreKind::Scroll => CompKind::Scroll,
                    CoreKind::Wheel => CompKind::Wheel,
                    CoreKind::Change => CompKind::Change,
                    CoreKind::KeyDown => CompKind::KeyDown,
                    CoreKind::KeyUp => CompKind::KeyUp,
                    CoreKind::TextInput => CompKind::TextInput,
                    CoreKind::PointerEnter => CompKind::PointerEnter,
                    CoreKind::PointerExit => CompKind::PointerExit,
                    CoreKind::Hover => CompKind::Hover,
                    CoreKind::LongPress => CompKind::LongPress,
                    CoreKind::DoubleTap => CompKind::DoubleTap,
                    CoreKind::Secondary => CompKind::Secondary,
                    // Consumed by `apply_composition` above, never routed.
                    CoreKind::Composition
                    | CoreKind::CompositionCommit
                    | CoreKind::CompositionEnd
                    | CoreKind::SystemScheme
                    | CoreKind::WindowFocus
                    | CoreKind::AccessFocus
                    | CoreKind::AccessScrollIntoView => {
                        unreachable!(
                            "composition, window focus, scheme and accessibility events are consumed before routing"
                        )
                    }
                };
                let value = ev.payload.as_ref().map(|p| match p {
                    InputPayload::Str(s) => Value::Str(s.clone()),
                    InputPayload::Bool(b) => Value::Bool(*b),
                    InputPayload::Float(f) => Value::Float(f64::from(*f)),
                    InputPayload::Number(n) => Value::Float(*n),
                    InputPayload::Element(e) => Value::Int(i64::from(*e)),
                    InputPayload::Key(k) => Value::Str(k.clone()),
                    // Only a `Composition` carries one, and those are consumed.
                    InputPayload::Preedit { text, .. } => Value::Str(text.clone()),
                });
                CompEvent {
                    kind,
                    pos: ev.pos,
                    delta: ev.delta,
                    value,
                    time_ms: ev.time_ms,
                    mods: ev.mods,
                }
            })
            .collect();

        // RFC-0005 `ScrollView` wheel: a wheel/scroll over a recorded scroll
        // target nudges whichever of `offset.x`/`offset.y` is writable, each
        // clamped to `[0, content − viewport]`. Wheel deltas are line-based (× a
        // per-line step); trackpad `Scroll` deltas are already pixels. Done here,
        // before the render, so the same tick paints the new offset (paint-time
        // translate, no relayout).
        for (i, ev) in events.iter().enumerate() {
            if consumed[i] {
                // A native view took this wheel event (a chart panning its own
                // axis, say), so the ScrollView underneath must not also
                // scroll on it.
                continue;
            }
            let step = match ev.kind {
                CoreKind::Wheel => WHEEL_LINE_PX,
                CoreKind::Scroll => 1.0,
                _ => continue,
            };
            let (px, py) = ev.pos;
            let Some(t) = self
                .scroll_targets
                .iter()
                .rev()
                .find(|t| {
                    px >= t.rect.x
                        && px < t.rect.x + t.rect.w
                        && py >= t.rect.y
                        && py < t.rect.y + t.rect.h
                })
                .copied()
            else {
                continue;
            };
            // Wheel forward (delta > 0) reveals earlier content → offset shrinks.
            if let Some(axis) = t.x {
                self.nudge_scroll(axis, ev.delta.0 * step);
            }
            if let Some(axis) = t.y {
                self.nudge_scroll(axis, ev.delta.1 * step);
            }
            // RFC-0021: mark this elem freshly scrolled and cancel any in-flight
            // snap glide, the user is driving again, so `settle_snaps` restarts
            // its quiet countdown and only snaps once the fling truly ends.
            if let Some(elem) = t.elem {
                self.scroll_quiet.insert(elem, self.frame_seq);
                self.snap_anims.remove(&elem);
                if let Some((axis, _)) = Self::scrollable_axis(&t) {
                    let off = self.peek_scroll(axis.sig);
                    self.record_scroll_velocity(elem, off, ev.time_ms);
                }
            }
        }

        // RFC-0005 `ScrollView` drag-to-scroll: a pointer press on inert scroll
        // content starts a drag; each move slides the offset (on every writable
        // axis) so the content tracks the pointer, a pure function of the
        // press-relative travel, no accumulated drift; release ends it.
        // The press defers to interactive children via `claims_pointer`, so a
        // button or slider inside the list still wins its own gesture.
        for ev in events {
            match ev.kind {
                CoreKind::PointerDown => {
                    let (px, py) = ev.pos;
                    // RFC-0026: an edge swipe outranks everything under it,
                    // that narrow strip is the platform's back gesture, and a
                    // scrollable or tappable child there must not steal it.
                    if self.begin_nav_swipe(ev.pos) {
                        continue;
                    }
                    let target = if self.router.claims_pointer(ev.pos) {
                        None
                    } else {
                        self.scroll_targets
                            .iter()
                            .rev()
                            .find(|t| {
                                px >= t.rect.x
                                    && px < t.rect.x + t.rect.w
                                    && py >= t.rect.y
                                    && py < t.rect.y + t.rect.h
                            })
                            .copied()
                    };
                    // A press reclaims the view, cancel any in-flight snap glide
                    // so the finger, not the spring, owns the offset (RFC-0021).
                    if let Some(elem) = target.and_then(|t| t.elem) {
                        self.snap_anims.remove(&elem);
                    }
                    self.scroll_drag = target.map(|t| ScrollDrag {
                        start_pos: (px, py),
                        x: t.x.map(|a| self.capture_drag_axis(a)),
                        y: t.y.map(|a| self.capture_drag_axis(a)),
                        elem: t.elem,
                        pull_refresh: t.pull_refresh,
                        refreshing_sig: t.refreshing_sig,
                    });
                    // A fresh press cancels a pull view's retract spring and resets
                    // fling-velocity tracking so this gesture starts clean (RFC-0021).
                    if let Some(elem) = target.and_then(|t| t.elem) {
                        self.pull_anims.remove(&elem);
                        self.scroll_vel.remove(&elem);
                        self.scroll_vel_last.remove(&elem);
                    }
                }
                CoreKind::PointerMove => {
                    if self.nav_swipe.is_some() {
                        self.drive_nav_swipe(ev.pos);
                        continue;
                    }
                    if let Some(d) = self.scroll_drag {
                        if let Some(a) = d.x {
                            let travel = ev.pos.0 - d.start_pos.0;
                            self.write_drag_axis(a, travel);
                        }
                        if let Some(a) = d.y {
                            let travel = ev.pos.1 - d.start_pos.1;
                            self.write_drag_axis(a, travel);
                        }
                        if d.pull_refresh {
                            self.drive_pull_drag(d, ev.pos.1);
                        }
                        // RFC-0021 fling projection: track the drag's velocity on
                        // its scrollable axis (vertical preferred) for the release.
                        if let Some(elem) = d.elem {
                            let axis = d.y.filter(|a| a.max > 0.0).or(d.x);
                            if let Some(a) = axis {
                                let off = self.peek_scroll(a.sig);
                                self.record_scroll_velocity(elem, off, ev.time_ms);
                            }
                        }
                    }
                }
                CoreKind::PointerUp | CoreKind::Tap => {
                    // RFC-0026: a released edge swipe either completes its pop
                    // or springs back, the finger's progress hands straight
                    // over to the spring.
                    self.release_nav_swipe();
                    // RFC-0021 snap: on release, settle the offset to the nearest
                    // page for a `snap: page` ScrollView (before clearing the drag).
                    if let Some(d) = self.scroll_drag {
                        self.snap_scroll_on_release(d.start_pos);
                        if d.pull_refresh {
                            self.release_pull(d);
                        }
                    }
                    self.scroll_drag = None;
                }
                _ => {}
            }
        }

        // RFC-0021: after this tick's scroll writes, reflect `page` continuously
        // (so pagination tracks wheel/trackpad scrolling, not just snap settles)
        // and fire `on_end_reached` for anything past its `end_threshold`.
        self.reflect_pages();
        self.fire_end_reached();
        // RFC-0026: a navigation that settled during this frame's render.
        self.fire_route_changes();

        self.router
            .dispatch_tick(&mut self.ctx, Some(&self.atlas), comp_events);
        // RFC-0040 §6: routing may have moved focus away from a composition.
        self.drop_unfocused_preedit();
        // RFC-0041 §6: and from the field whose caret was blinking, or an edit
        // may have started the blink over.
        self.settle_blink();
    }
}
