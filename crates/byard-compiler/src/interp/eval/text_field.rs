//! The `TextField` widget in the paint walk (RFC-0040, RFC-0041).
//!
//! `interp::edit` is the model and `eval::editing` the handlers; this is the
//! widget itself: its box, the value or the placeholder, the selection and
//! the caret while it has focus, and the handlers it registers. Also the IME
//! composition (the preedit drawn at the owning field's caret, never part of
//! its value) and the routing of the platform's composition events.

use super::paint::{dim_alpha, push_fill};
use super::{Interpreter, editing, floor_char_boundary};
use crate::interp::env::Value;
use crate::parser::ast::{Attr, AttrKind};

/// An IME composition (RFC-0040 §1): the preedit, drawn at the caret of the
/// field that owns it, and never part of that field's value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Composition {
    /// The element the composition belongs to. Kept after focus leaves it,
    /// with the preedit cleared, so a platform that commits on blur still
    /// commits into the right field (§6).
    owner: u32,
    /// The text being composed; empty once cleared.
    text: String,
    /// The IME's cursor inside `text`, as a byte range; `None` hides it.
    cursor: Option<(usize, usize)>,
}

/// How far a `TextField`'s text sits from its left edge when the field writes
/// no padding.
const FIELD_PAD_X: f32 = 10.0;

/// A placeholder is drawn in the field's `color:` at this share of its alpha.
const PLACEHOLDER_ALPHA: f32 = 0.55;

/// Where the caret's left edge sits relative to the boundary it is at, in
/// logical pixels. A little left of it, so a caret between two letters is in
/// the gap and not on the left side of the second one (RFC-0041 §6).
const CARET_DX: f32 = -0.5;

/// A selection is painted in the theme's `primary` at this share of its alpha
/// when the field writes no `selection:` colour (RFC-0041 §6).
const SELECTION_ALPHA: f32 = 0.3;

impl Interpreter {
    /// Renders a `TextField` widget: background box + text/placeholder, and,
    /// while it has focus, the selection and the caret (RFC-0041 §6). Registers
    /// the handlers that edit it.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    pub(super) fn render_text_field(
        &mut self,
        bound_sig: Option<super::env::SignalId>,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        use super::events::EventKind as Kind;
        let placeholder = self.eval_str_prop(attrs, "placeholder").unwrap_or_default();
        let cur_text = bound_sig
            .map(|s| match self.ctx.peek_signal(s) {
                Value::Str(t) => t,
                _ => String::new(),
            })
            .unwrap_or_default();

        let is_focused = elem_idx.is_some_and(|i| self.router.is_focused(i));
        // RFC-0041: the field being edited, if it is the focused one and is
        // bound to a `var` there is something to edit in.
        let editing = elem_idx.filter(|_| is_focused && bound_sig.is_some());
        // RFC-0040: the preedit this field owns, drawn at the caret and never
        // part of the value.
        let preedit = self
            .composition
            .as_ref()
            .filter(|c| is_focused && elem_idx == Some(c.owner) && !c.text.is_empty())
            .map(|c| (c.text.clone(), c.cursor));

        let (display_text, is_placeholder) = if cur_text.is_empty() && preedit.is_none() {
            (placeholder, true)
        } else {
            (cur_text, false)
        };
        // What the caret and the selection index into: the value, which is
        // empty while a placeholder is showing in its place.
        let value: &str = if is_placeholder { "" } else { &display_text };

        // The field's own `color:` for what is typed, and the same colour
        // fainter for the placeholder. Unwritten, the field keeps the white
        // on grey it always had.
        let written = self
            .eval_color_prop(attrs, "color")
            .map(super::intrinsics::color_rgba_auto);
        let value_rgba =
            written.unwrap_or_else(|| super::intrinsics::color_to_rgba(0x00ff_ffff, false));
        let text_rgba = match (is_placeholder, written) {
            (false, _) => value_rgba,
            (true, Some(c)) => [c[0], c[1], c[2], c[3] * PLACEHOLDER_ALPHA],
            (true, None) => super::intrinsics::color_to_rgba(0x0088_8888, false),
        };
        // `size:` first, then the `typo:` token's size, as a `Text` does.
        let typo_size = self.eval_typo_size(attrs);
        let font_size = self
            .eval_int_prop(attrs, "size")
            .or(typo_size)
            .unwrap_or(16) as f32;

        // Focus underline (Material-style): a thin accent bar along the bottom
        // edge when the field holds focus.
        if is_focused {
            let bar_h = 2.0_f32;
            frame.push_instance(byard_core::BoxInstance {
                rect: [rect.x, rect.y + rect.h - bar_h, rect.w, bar_h],
                color: dim_alpha(
                    super::intrinsics::color_to_rgba(self.theme.primary(), false),
                    opacity,
                ),
                radii: [0.0; 4],
                transform,
                smooth: 0.0,
            });
        }

        // The text starts at the field's left padding when it writes one.
        let pads = attrs.iter().any(|a| {
            matches!(
                a.name.as_str(),
                "p" | "padding" | "pl" | "padding_left" | "padding-left"
            )
        });
        let pad_x = if pads {
            self.eval_container_style("Box", attrs).padding.left
        } else {
            FIELD_PAD_X
        };
        let text_x = rect.x + pad_x;
        let text_y = rect.y + (rect.h - font_size) / 2.0;
        // The content box the text lives in; a longer text scrolls inside it.
        let view_w = (rect.w - pad_x * 2.0).max(1.0);
        // NOTE: `TextLine` carries no `Transform` field (RFC-0011 engine slice:
        // only box primitives were given one), so the field's *text* does not
        // follow `translate`/`scale`/`rotate`, the box visuals below (underline,
        // caret) and its `bg` fill do. Same limitation as the `Text` intrinsic.
        // The field's own type properties, which it accepts from the
        // intrinsic catalogue and, until now, ignored: a field set in a
        // declared family rendered in the system font, which is the shape of
        // defect this whole change exists to remove.
        let weight = self.resolve_weight(attrs);
        let family = self.resolve_family(attrs);
        let font = super::edit::FieldFont {
            size: font_size,
            weight,
            family: family.clone(),
        };

        // RFC-0041: where the caret and the selection are, in pixels from the
        // start of the text, and how far the text has scrolled to keep the
        // caret in view. Measured through the shared measurer, whose cache
        // makes a prefix that was measured last frame free this frame.
        let mut caret_rel = 0.0_f32;
        let mut value_w = 0.0_f32;
        let mut selection: Option<(f32, f32)> = None;
        let mut scroll = 0.0_f32;
        // The preedit's width, and the IME's cursor inside it, as x ranges.
        let mut preedit_w = 0.0_f32;
        let mut preedit_cursor: Option<(f32, f32)> = None;
        if let Some(idx) = editing {
            let store = std::rc::Rc::clone(&self.edit);
            let seq = self.frame_seq;
            let mut measure = |t: &str| self.measure_in(t, &font);
            if let Some((text, cursor)) = &preedit {
                preedit_w = measure(text);
                preedit_cursor = cursor.map(|(b, e)| {
                    let at = |upto: usize| measure(&text[..floor_char_boundary(text, upto)]);
                    (at(b), at(e))
                });
            }
            let mut store = store.borrow_mut();
            let state = store.state(idx);
            state.sync(value);
            state.seen = seq;
            caret_rel = state.x_of(value, &font, state.caret, &mut measure);
            value_w = state.x_of(value, &font, value.len(), &mut measure);
            if state.has_selection() && preedit.is_none() {
                let (lo, hi) = state.range();
                selection = Some((
                    state.x_of(value, &font, lo, &mut measure),
                    state.x_of(value, &font, hi, &mut measure),
                ));
            }
            let cursor_x = caret_rel + preedit_cursor.map_or(preedit_w, |(_, e)| e);
            state.follow_caret(cursor_x, value_w + preedit_w, view_w);
            scroll = state.scroll;
        } else if let Some(idx) = elem_idx {
            // A field that is not focused keeps its state while it is on
            // screen and loses it with its element (§1).
            let seq = self.frame_seq;
            let mut store = self.edit.borrow_mut();
            if !store.is_empty() {
                store.touch(idx, seq);
            }
        }
        let origin_x = text_x - scroll;
        // The text is clipped to the content box when it overflows it, which is
        // the only time it can reach outside.
        let clipped = editing.is_some() && value_w + preedit_w > view_w;
        if clipped {
            frame.begin_clip(byard_core::frame::Rect::new(text_x, rect.y, view_w, rect.h));
        }

        // The selection, behind the text: the theme's `primary` at 30 %, or the
        // field's own `selection:` colour.
        if let Some((x0, x1)) = selection.filter(|(a, b)| b > a) {
            let color = self.eval_color_prop(attrs, "selection").map_or_else(
                || {
                    let c = super::intrinsics::color_to_rgba(self.theme.primary(), false);
                    [c[0], c[1], c[2], c[3] * SELECTION_ALPHA]
                },
                super::intrinsics::color_rgba_auto,
            );
            push_fill(
                frame,
                byard_core::BoxInstance {
                    rect: [origin_x + x0, text_y - 1.0, x1 - x0, font_size + 2.0],
                    color: dim_alpha(color, opacity),
                    radii: [0.0; 4],
                    transform,
                    smooth: 0.0,
                },
            );
        }

        // The value's text. A preedit is drawn at the caret, so the part of the
        // value after the caret moves out of its way (RFC-0040 §1).
        let caret_at = editing
            .and_then(|_| self.edit.borrow().get(elem_idx?).map(|s| s.caret))
            .unwrap_or(value.len());
        let split = preedit.is_some() && caret_at < value.len();
        let push_value = |frame: &mut byard_core::frame::RenderFrame, x: f32, text: &str| {
            if !text.is_empty() {
                frame.push_text(byard_core::TextLine {
                    weight,
                    family: family.clone(),
                    x,
                    y: text_y,
                    text: text.to_string(),
                    font_size,
                    color: dim_alpha(text_rgba, opacity),
                    dirty: true,
                });
            }
        };
        if split {
            let (head, tail) = value.split_at(caret_at);
            push_value(frame, origin_x, head);
            push_value(frame, origin_x + caret_rel + preedit_w, tail);
        } else {
            push_value(frame, origin_x, &display_text);
        }

        // The caret, in the field's own text colour: at the caret's place in
        // the value, or at the IME's cursor inside the preedit while composing
        // (RFC-0040).
        if is_focused {
            let mut caret_x = origin_x + caret_rel + CARET_DX;
            if let Some((text, _)) = &preedit {
                let color = dim_alpha(text_rgba, opacity);
                let start = origin_x + caret_rel;
                frame.push_text(byard_core::TextLine {
                    weight,
                    family: family.clone(),
                    x: start,
                    y: text_y,
                    text: text.clone(),
                    font_size,
                    color,
                    dirty: true,
                });
                // §5: the whole preedit thinly underlined, the IME's cursor
                // range, the active clause on IMEs that use it, thickly.
                let underline_y = text_y + font_size + 1.0;
                let mut bar = |x: f32, w: f32, h: f32| {
                    frame.push_instance(byard_core::BoxInstance {
                        rect: [x, underline_y, w, h],
                        color,
                        radii: [0.0; 4],
                        transform,
                        smooth: 0.0,
                    });
                };
                bar(start, preedit_w, 1.0);
                if let Some((b, e)) = preedit_cursor.filter(|(b, e)| e > b) {
                    bar(start + b, e - b, 2.0);
                }
                caret_x = start + preedit_cursor.map_or(preedit_w, |(_, e)| e) + CARET_DX;
            }
            // `None` from the IME hides its cursor; the caret rectangle is
            // still reported, since the candidate window needs a place.
            let caret_visible = preedit.as_ref().is_none_or(|(_, c)| c.is_some());
            // The blink (RFC-0041 §6): one timer wake per phase, started by the
            // first frame that draws the caret and stopped when it stops being
            // drawn.
            if let Some(idx) = editing {
                self.start_blink(idx);
            }
            if caret_visible && self.caret_shown() {
                frame.push_instance(byard_core::BoxInstance {
                    rect: [caret_x, text_y, 1.5, font_size],
                    color: dim_alpha(value_rgba, opacity),
                    radii: [0.0; 4],
                    transform,
                    smooth: 0.0,
                });
            }
            // RFC-0040 §4: where the platform puts its candidate window, in
            // viewport space, so through the field's transform.
            let a = transform.apply_point([caret_x, text_y]);
            let b = transform.apply_point([caret_x + 1.5, text_y + font_size]);
            frame.set_text_input(byard_core::frame::TextInputState {
                caret: byard_core::frame::Rect::new(
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    (b[0] - a[0]).abs(),
                    (b[1] - a[1]).abs(),
                ),
            });
        }
        if clipped {
            frame.end_clip();
        }

        // The field's own events. The router runs one handler per event kind
        // on the focused element, the last one registered, so the handlers
        // written on the field are registered first (pointer events, focus,
        // blur, key_up reach them as on any element) and the ones that share
        // a kind with the field's own editing are folded into it below.
        self.register_event_attrs(attrs, hit_rect, elem_idx);
        let on_edit = self.field_actions(attrs, &["input", "change"]);
        let on_typed = self.field_actions(attrs, &["text_input"]);
        let on_key = self.field_actions(attrs, &["key_down"]);
        let on_submit = self.field_actions(attrs, &["submit"]);

        // Handlers: typing and the keys edit the value at the caret, the
        // pointer places it and selects (RFC-0041).
        if let (Some(sig), Some(idx)) = (bound_sig, elem_idx) {
            let handle = self.field_handle(
                sig,
                idx,
                editing::FieldGeom {
                    font,
                    text_x,
                    view_w,
                    frame: self.router.current_frame(),
                },
            );
            // `input` and `change` see the value already written.
            let edited = self.field_actions(attrs, &["input", "change"]);
            let events = [
                (Kind::TextInput, handle.clone().text_input(on_typed, edited)),
                (
                    Kind::KeyDown,
                    handle.clone().key_down(on_key, on_edit, on_submit),
                ),
                (
                    Kind::PointerDown,
                    handle
                        .clone()
                        .pointer_down(self.field_actions(attrs, &["pointer_down"])),
                ),
                (Kind::PointerDrag, handle.clone().pointer_drag()),
                (
                    Kind::PointerUp,
                    handle
                        .clone()
                        .pointer_up(self.field_actions(attrs, &["pointer_up"])),
                ),
                (
                    Kind::DoubleTap,
                    handle.double_tap(self.field_actions(attrs, &["double_tap"])),
                ),
            ];
            for (kind, action) in events {
                self.router.on(idx, hit_rect, kind, action);
            }
            // A selection that starts in the field goes on past its edge.
            self.router.capture_drag(idx);

            // Change event: write-back from platform (E1).
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::Change,
                super::events::write_back_action(sig),
            );

            // Register as focusable so Tab and click steal focus.
            // TextField uses its own focused-var if provided via `focused:` attr;
            // otherwise we create a dummy signal just for the focusable registry.
            let focused_sig = self.resolve_focused_sig(attrs);
            let fsig = focused_sig.unwrap_or_else(|| self.ctx.create_signal(Value::Bool(false)));
            self.router.focusable(idx, hit_rect, fsig);
        }
    }

    /// Resolves the `focused:` attribute to a `SignalId`, if present.
    pub(super) fn resolve_focused_sig(&self, attrs: &[Attr]) -> Option<super::env::SignalId> {
        use crate::parser::ast::Expr;
        for attr in attrs {
            if attr.name.as_str() == "focused" {
                if let AttrKind::Prop {
                    value: Expr::Ident(name, _),
                } = &attr.kind
                {
                    if let Some(Value::Signal(sig)) = self.env.lookup(name) {
                        return Some(*sig);
                    }
                }
            }
        }
        None
    }

    /// Applies this tick's IME events to the composition, and marks them, and
    /// any editing key the IME owns, as consumed (RFC-0040 §3, §6).
    ///
    /// - A preedit belongs to the focused field and replaces the last one; an
    ///   empty preedit clears the text but keeps the owner, because winit
    ///   sends one right before every commit.
    /// - A commit goes to the owner, which is not always the focused field,
    ///   through the same `TextInput` handler typing uses: committed text and
    ///   typed text are the same thing once they reach the field.
    /// - The IME turning off ends the composition, with nothing committed.
    /// - Backspace, Delete, Enter and the keys that move the caret are the
    ///   IME's while a preedit is showing, so they never also edit the
    ///   committed value or move the caret out from under the preedit.
    pub(super) fn apply_composition(
        &mut self,
        events: &[byard_core::InputEvent],
        consumed: &mut [bool],
    ) {
        use byard_core::platform::{EventKind as CoreKind, InputPayload};
        for (i, ev) in events.iter().enumerate() {
            match ev.kind {
                CoreKind::Composition => {
                    consumed[i] = true;
                    let Some(InputPayload::Preedit { text, cursor }) = &ev.payload else {
                        continue;
                    };
                    if text.is_empty() {
                        if let Some(c) = self.composition.as_mut() {
                            c.text.clear();
                            c.cursor = None;
                        }
                    } else if let Some(owner) = self.router.focused() {
                        self.composition = Some(Composition {
                            owner,
                            text: text.clone(),
                            cursor: *cursor,
                        });
                    }
                }
                CoreKind::CompositionCommit => {
                    consumed[i] = true;
                    let target = self
                        .composition
                        .take()
                        .map(|c| c.owner)
                        .or_else(|| self.router.focused());
                    if let (Some(target), Some(InputPayload::Key(text))) = (target, &ev.payload) {
                        // No key is down for a commit: it must not inherit the
                        // modifiers of the last one (RFC-0041 §1).
                        Self::clear_event_context(ev.time_ms);
                        self.router.fire_event(
                            &mut self.ctx,
                            target,
                            super::events::EventKind::TextInput,
                            Some(&Value::Str(text.clone())),
                        );
                    }
                }
                CoreKind::CompositionEnd => {
                    consumed[i] = true;
                    self.composition = None;
                }
                CoreKind::KeyDown | CoreKind::KeyUp if self.composing() => {
                    if let Some(InputPayload::Key(key)) = &ev.payload {
                        if matches!(
                            key.as_str(),
                            "Backspace"
                                | "Delete"
                                | "Enter"
                                | "ArrowLeft"
                                | "ArrowRight"
                                | "Home"
                                | "End"
                        ) {
                            consumed[i] = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether a preedit is showing in the focused field.
    fn composing(&self) -> bool {
        self.composition
            .as_ref()
            .is_some_and(|c| !c.text.is_empty() && self.router.is_focused(c.owner))
    }

    /// Discards the preedit of a field that has lost focus (RFC-0040 §6).
    ///
    /// The engine never commits it: it does not know the IME's conversion
    /// state, and a platform that commits on blur would then insert the text
    /// twice. The owner is kept so that platform's commit still lands in the
    /// field the text was typed into.
    pub(super) fn drop_unfocused_preedit(&mut self) {
        if let Some(c) = self.composition.as_mut() {
            if !self.router.is_focused(c.owner) {
                c.text.clear();
                c.cursor = None;
            }
        }
    }
}
