//! A `TextField`'s editing, wired to the interpreter (RFC-0041).
//!
//! `interp::edit` is the model: a caret, a selection, undo, and what a key
//! does to them. This is the other half, the part that needs the interpreter:
//! the handlers a field registers on the router (they hold the model, the
//! clipboard and the text measurer, because an `Action` can reach nothing
//! else), the caret's blink, and the clipboard the host provides.

use std::cell::RefCell;
use std::rc::Rc;

use byard_core::{Clipboard, KeyModifiers, MemoryClipboard};

use super::{Interpreter, controller::EngineTimer};
use crate::interp::edit::{Edge, EditStore, FieldFont, Keymap, Outcome};
use crate::interp::env::{SignalId, Value};
use crate::interp::events::{Action, CURRENT_EVENT_MODS, CURRENT_EVENT_POS, CURRENT_EVENT_TIME};
use crate::interp::reactive::ReactiveCtx;

/// The caret shows for this long, then hides for as long, and starts again
/// from showing on every edit and caret move (RFC-0041 §6).
pub(super) const BLINK_PHASE_MS: u64 = 530;

/// The text measurer, shared with the handlers that map a pointer to a caret.
/// Created on first use, as it always was, so nothing that never measures text
/// ever loads a font.
pub(super) type SharedMeasurer = Rc<RefCell<Option<byard_core::text::TextMeasurer>>>;

/// The host's clipboard, shared with the handlers that copy and paste.
#[derive(Clone)]
pub(super) struct SharedClipboard(Rc<RefCell<Box<dyn Clipboard>>>);

impl Default for SharedClipboard {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(Box::new(MemoryClipboard::new()))))
    }
}

/// What the caret's blink is conditioned on, besides there being a focused
/// field (RFC-0041 §6).
#[derive(Default)]
pub(super) struct CaretFlags {
    /// Whether the window has lost focus. `false` until the host says
    /// otherwise, so a headless run is a window with focus.
    pub window_blurred: bool,
    /// Whether the caret is held showing instead of blinking, as a picture of
    /// the app (`byard shot`) wants it.
    pub still: bool,
    /// Whether this frame drew a focused field's caret, so a blink whose field
    /// is gone (a `when` branch that closed under focus) is stopped rather
    /// than left waking the logic thread for nothing.
    pub seen: bool,
}

/// The caret's blink: the timer, and the field it blinks for.
pub(super) struct Blink {
    /// The field whose caret this is.
    owner: u32,
    /// Dropped to stop it.
    timer: EngineTimer,
}

/// Where a field's text sits, so a handler can turn the pointer into a place
/// in it. Captured when the field is rendered.
#[derive(Clone)]
pub(super) struct FieldGeom {
    /// The font the text is measured in.
    pub font: FieldFont,
    /// Where the text starts, in the field's own space.
    pub text_x: f32,
    /// How wide the content box is.
    pub view_w: f32,
    /// The ancestors' paint transform, which the pointer is mapped back
    /// through (RFC-0011).
    pub frame: Option<byard_core::frame::Transform>,
}

/// What a field's handlers share: the model of every field, the clipboard, the
/// measurer, and which field and `var` these handlers are for.
#[derive(Clone)]
pub(super) struct FieldHandle {
    store: Rc<RefCell<EditStore>>,
    clipboard: SharedClipboard,
    measurer: SharedMeasurer,
    sig: SignalId,
    elem: u32,
    geom: FieldGeom,
}

/// The value a `var` holds, as text; anything else is an empty field.
fn text_of(ctx: &mut ReactiveCtx, sig: SignalId) -> String {
    match ctx.peek_signal(sig) {
        Value::Str(s) => s,
        _ => String::new(),
    }
}

impl FieldHandle {
    /// Measures `text` in the field's font.
    fn width(&self, text: &str) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let f = &self.geom.font;
        self.measurer
            .borrow_mut()
            .get_or_insert_with(byard_core::text::TextMeasurer::new)
            .measure_wrapped(text, f.size, None, f.weight, f.family.as_deref())
            .0
    }

    /// The pointer of the event being dispatched, in the field's own space.
    fn pointer(&self) -> (f32, f32) {
        let pos = CURRENT_EVENT_POS.with(std::cell::Cell::get);
        crate::interp::events::local_point(self.geom.frame, pos)
    }

    /// Writes an edit's result to the bound `var`.
    fn write(&self, ctx: &mut ReactiveCtx, outcome: &Outcome) {
        if let Outcome::Value(new) = outcome {
            ctx.write_signal(self.sig, Value::Str(new.clone()));
        }
    }

    /// Runs `f` on this field's editing state with the `var`'s current value,
    /// brought in line with it first (a value written from outside moved the
    /// caret to the end), and notes what it did for the blink.
    fn edit(
        &self,
        ctx: &mut ReactiveCtx,
        f: impl FnOnce(&mut crate::interp::edit::EditState, &str) -> Outcome,
    ) -> Outcome {
        let value = text_of(ctx, self.sig);
        let mut store = self.store.borrow_mut();
        let state = store.state(self.elem);
        state.sync(&value);
        let outcome = f(state, &value);
        store.note(&outcome);
        outcome
    }

    /// The `TextInput` handler: typed text, or an input method's commit,
    /// replaces the selection or goes in at the caret.
    pub(super) fn text_input(self, mut typed: Vec<Action>, mut edited: Vec<Action>) -> Action {
        Box::new(move |ctx, payload| {
            let Some(Value::Str(text)) = payload else {
                return;
            };
            let mods = CURRENT_EVENT_MODS.with(std::cell::Cell::get);
            // A shortcut's character is not text (`Ctrl+V` types no `v`).
            if Keymap::HERE.is_shortcut(mods) {
                return;
            }
            let now = CURRENT_EVENT_TIME.with(std::cell::Cell::get);
            let outcome = self.edit(ctx, |state, value| state.insert(value, text, now));
            self.write(ctx, &outcome);
            for action in &mut typed {
                action(ctx, payload);
            }
            if matches!(outcome, Outcome::Value(_)) {
                let now = Value::Str(text_of(ctx, self.sig));
                for action in &mut edited {
                    action(ctx, Some(&now));
                }
            }
        })
    }

    /// The `KeyDown` handler: movement, selection, deletion, the clipboard and
    /// undo (RFC-0041's key table), then the handlers the field's author wrote.
    /// Enter submits.
    pub(super) fn key_down(
        self,
        mut keys: Vec<Action>,
        mut edited: Vec<Action>,
        mut submit: Vec<Action>,
    ) -> Action {
        Box::new(move |ctx, payload| {
            let Some(Value::Str(key)) = payload else {
                return;
            };
            let mods = CURRENT_EVENT_MODS.with(std::cell::Cell::get);
            let before = text_of(ctx, self.sig);
            let outcome = self.edit(ctx, |state, value| {
                let mut clipboard = self.clipboard.0.borrow_mut();
                state.on_key(value, key, mods, Keymap::HERE, &mut **clipboard)
            });
            self.write(ctx, &outcome);
            for action in &mut keys {
                action(ctx, payload);
            }
            let now = Value::Str(text_of(ctx, self.sig));
            // An edit that changed nothing fires nothing: a Backspace at the
            // start, a Delete at the end, a paste of nothing.
            if now != Value::Str(before) {
                for action in &mut edited {
                    action(ctx, Some(&now));
                }
            }
            if key.as_str() == "Enter" {
                for action in &mut submit {
                    action(ctx, Some(&now));
                }
            }
        })
    }

    /// The `PointerDown` handler: the caret goes where the press landed, and
    /// Shift extends the selection from where it was. The press that focuses
    /// the field does this too: focus is taken after the handler runs.
    pub(super) fn pointer_down(self, mut user: Vec<Action>) -> Action {
        Box::new(move |ctx, payload| {
            let mods = CURRENT_EVENT_MODS.with(std::cell::Cell::get);
            let (x, _) = self.pointer();
            let geom = &self.geom;
            self.edit(ctx, |state, value| {
                let at = x - geom.text_x + state.scroll;
                state.press(value, &geom.font, at, mods.shift, &mut |t| self.width(t))
            });
            for action in &mut user {
                action(ctx, payload);
            }
        })
    }

    /// The `PointerDrag` handler: the caret follows the pointer and the anchor
    /// stays where the press was. Past either edge of a field whose text
    /// overflows, the caret steps one grapheme that way per move instead, and
    /// the text scrolls after it.
    pub(super) fn pointer_drag(self) -> Action {
        Box::new(move |ctx, _| {
            let (x, _) = self.pointer();
            let geom = &self.geom;
            self.edit(ctx, |state, value| {
                if !state.dragging {
                    return Outcome::None;
                }
                let overflows = state.x_of(value, &geom.font, value.len(), &mut |t| self.width(t))
                    > geom.view_w;
                let edge = if !overflows {
                    None
                } else if x < geom.text_x {
                    Some(Edge::Start)
                } else if x > geom.text_x + geom.view_w {
                    Some(Edge::End)
                } else {
                    None
                };
                let at = x - geom.text_x + state.scroll;
                state.drag(value, &geom.font, at, edge, &mut |t| self.width(t))
            });
        })
    }

    /// The `PointerUp` handler: the selection by dragging is over.
    pub(super) fn pointer_up(self, mut user: Vec<Action>) -> Action {
        Box::new(move |ctx, payload| {
            if let Some(state) = self.store.borrow_mut().get_mut(self.elem) {
                state.dragging = false;
            }
            for action in &mut user {
                action(ctx, payload);
            }
        })
    }

    /// The `DoubleTap` handler: selects the word under the pointer.
    pub(super) fn double_tap(self, mut user: Vec<Action>) -> Action {
        Box::new(move |ctx, payload| {
            let (x, _) = self.pointer();
            let geom = &self.geom;
            self.edit(ctx, |state, value| {
                let at = x - geom.text_x + state.scroll;
                state.select_word(value, &geom.font, at, &mut |t| self.width(t))
            });
            for action in &mut user {
                action(ctx, payload);
            }
        })
    }
}

impl Interpreter {
    /// Gives the interpreter the clipboard `Copy`, `Cut` and `Paste` in a
    /// `TextField` use (RFC-0041 §3). A windowed host passes the system's;
    /// without one, an interpreter has an in-memory clipboard, which is what a
    /// headless run and every test want: a copy can be pasted without
    /// touching the clipboard of the machine.
    pub fn set_clipboard(&mut self, clipboard: Box<dyn Clipboard>) {
        *self.clipboard.0.borrow_mut() = clipboard;
    }

    /// Holds the caret showing (`false`) or lets it blink (`true`, the
    /// default). A picture of the app wants it held: whether the caret is in
    /// the picture should not depend on which half of a blink the clock was in.
    pub fn set_caret_blink(&mut self, blinking: bool) {
        self.caret.still = !blinking;
        if !blinking {
            self.stop_blink();
        }
    }

    /// The handlers' view of the field `elem`, whose `var` is `sig`.
    pub(super) fn field_handle(&self, sig: SignalId, elem: u32, geom: FieldGeom) -> FieldHandle {
        FieldHandle {
            store: Rc::clone(&self.edit),
            clipboard: self.clipboard.clone(),
            measurer: Rc::clone(&self.text_measurer),
            sig,
            elem,
            geom,
        }
    }

    /// Measures `text` in `font` through the shared measurer.
    pub(super) fn measure_in(&self, text: &str, font: &FieldFont) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        self.text_measurer
            .borrow_mut()
            .get_or_insert_with(byard_core::text::TextMeasurer::new)
            .measure_wrapped(text, font.size, None, font.weight, font.family.as_deref())
            .0
    }

    /// Whether the caret of the focused field is showing this frame: not in
    /// the off half of its blink, and in a window that has focus.
    pub(super) fn caret_shown(&self) -> bool {
        !self.caret.window_blurred && !self.caret_off.get()
    }

    /// Starts the caret blinking for `owner`, if it is not already, and no
    /// window-less or timer-less embedding is asking for it (RFC-0041 §6).
    ///
    /// One repeating timer through the controller boundary's timer path: one
    /// wake of the logic thread per phase, two a second, and nothing in
    /// between. It is never an animation (`any_active` is not touched), so a
    /// focused, idle field does not keep the logic thread ticking at frame
    /// rate.
    pub(super) fn start_blink(&mut self, owner: u32) {
        self.caret.seen = true;
        if self.caret.window_blurred
            || self.caret.still
            || self.blink.as_ref().is_some_and(|b| b.owner == owner)
        {
            return;
        }
        self.stop_blink();
        let off = Rc::clone(&self.caret_off);
        let tick: Action = Box::new(move |_, _| off.set(!off.get()));
        if let Some(timer) = self.arm_engine_timer(BLINK_PHASE_MS, tick) {
            self.blink = Some(Blink { owner, timer });
        }
    }

    /// Stops the blink and leaves the caret showing.
    pub(super) fn stop_blink(&mut self) {
        self.caret_off.set(false);
        if let Some(blink) = self.blink.take() {
            self.cancel_engine_timer(blink.timer);
        }
    }

    /// Settles the blink with what this tick's events did: stops it when its
    /// field has lost focus or the window has, and starts it over, caret
    /// showing, when the caret moved or the value was edited.
    pub(super) fn settle_blink(&mut self) {
        let reset = std::mem::take(&mut self.edit.borrow_mut().blink_reset);
        let Some(blink) = self.blink.as_ref() else {
            // The caret shows again after a restart; arming waits for the
            // render that draws it.
            if reset {
                self.caret_off.set(false);
            }
            return;
        };
        let owner = blink.owner;
        if self.caret.window_blurred || !self.router.is_focused(owner) {
            self.stop_blink();
        } else if reset {
            self.stop_blink();
            self.start_blink(owner);
        }
    }

    /// What a rendered frame leaves to settle about text editing: the state of
    /// a field that was not drawn is dropped with its element (RFC-0041 §1),
    /// and a blink whose field was not drawn is stopped, so a branch that
    /// closed under focus does not leave the logic thread waking twice a
    /// second for a caret nobody can see.
    pub(super) fn settle_text_editing(&mut self) {
        {
            let mut store = self.edit.borrow_mut();
            if !store.is_empty() {
                store.sweep(self.frame_seq);
            }
        }
        if self.blink.is_some() && !self.caret.seen {
            self.stop_blink();
        }
        self.caret.seen = false;
    }

    /// The selection of the focused field as `(anchor, caret)` byte offsets, or
    /// `None` when no field with editing state has focus. Test-facing.
    #[must_use]
    pub fn focused_selection(&self) -> Option<(usize, usize)> {
        let focused = self.router.focused()?;
        let store = self.edit.borrow();
        let state = store.get(focused)?;
        Some((state.anchor, state.caret))
    }

    /// Whether the caret's blink timer is running. Test-facing: the witness
    /// that an unfocused field arms nothing and a focused one arms one.
    #[must_use]
    pub fn caret_is_blinking(&self) -> bool {
        self.blink.is_some()
    }

    /// How many fields hold editing state. Test-facing: zero for an app that
    /// has not put focus in a `TextField`.
    #[must_use]
    pub fn edit_state_count(&self) -> usize {
        self.edit.borrow().len()
    }

    /// The modifiers a `TextField` handler would see for an event: none.
    /// Used by the composition path, which fires a handler without a platform
    /// event behind it and must not inherit the last key's modifiers.
    pub(super) fn clear_event_context(time_ms: u64) {
        CURRENT_EVENT_MODS.with(|c| c.set(KeyModifiers::default()));
        CURRENT_EVENT_TIME.with(|c| c.set(time_ms));
    }
}
