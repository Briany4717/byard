//! Text editing for a `TextField` (RFC-0041): the caret and the selection,
//! the clipboard, undo, and the mapping from a pointer to a caret.
//!
//! This module is the *model*. It knows nothing of the router, the frame or
//! the interpreter: an [`EditState`] is handed the field's current value and
//! an event, and answers with what the value became ([`Outcome`]). That is
//! what makes every row of RFC-0041's key table testable with a string and a
//! key name, and what keeps the interpreter's side down to wiring (see
//! `eval/editing.rs`).
//!
//! # Offsets
//!
//! Every offset is a **byte** offset kept on a grapheme boundary (§Q1):
//! slicing, inserting and measuring a prefix all take byte offsets, so grapheme
//! indices would be converted on every operation. A decomposed accent, an
//! emoji with a skin tone and a regional-indicator flag are one step of the
//! caret each. The invariant is checked in debug builds after every edit.
//!
//! # Cost
//!
//! A field that never has focus never has an [`EditState`]: the store is empty
//! and nothing here runs.

use std::collections::HashMap;
use std::sync::Arc;

use byard_core::{Clipboard, KeyModifiers};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

/// How many undo steps a field keeps; the oldest drops (RFC-0041 §5).
pub(crate) const UNDO_LIMIT: usize = 100;

/// How long a pause ends one run of typed characters and starts the next, as
/// an undo step (RFC-0041 §5).
pub(crate) const COALESCE_MS: u64 = 1000;

/// The margin, in logical pixels, the caret keeps from either edge of the
/// field's content box when the text scrolls (RFC-0041 §6).
pub(crate) const CARET_MARGIN: f32 = 4.0;

/// The state a field was in before an edit, which undo returns to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    value: String,
    caret: usize,
    anchor: usize,
}

/// A run of typed characters that is still one undo step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Typing {
    /// When the last character of the run was typed.
    at_ms: u64,
    /// Where the caret stood after it. A caret that is anywhere else has
    /// moved, which ends the run.
    caret: usize,
}

/// What an event did to a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing changed: not the value, not the caret, not the selection. The
    /// field fires nothing, as Backspace in an empty field never has.
    None,
    /// The caret or the selection moved; the value is what it was.
    Moved,
    /// The value is now this. The caller writes the bound `var`, which is what
    /// fires `on input` and `on change`.
    Value(String),
}

/// The font a field's text is measured in. Part of what a cached width is
/// valid for, with the value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FieldFont {
    /// Logical pixels.
    pub size: f32,
    /// The CSS weight axis.
    pub weight: u16,
    /// The resolved family, `None` for the system font.
    pub family: Option<Arc<str>>,
}

/// The measured width of the value's prefixes at grapheme boundaries, for the
/// value and font they were measured for (RFC-0041 §4).
///
/// Lazily filled: finding the boundary nearest a pointer is a binary search,
/// so it measures `log₂ n` prefixes rather than `n`, and each is measured once
/// until the value, the size, the weight or the family changes. A drag then
/// measures nothing new, and a paint reads the caret's x from here.
#[derive(Clone, Debug, Default)]
struct Widths {
    key: Option<(String, FieldFont)>,
    /// Every grapheme boundary of the value, `0` and its length included.
    bounds: Vec<usize>,
    /// The width of `value[..bounds[i]]`; `NaN` until measured.
    px: Vec<f32>,
}

impl Widths {
    /// Drops what was measured unless it was measured for this value and font.
    fn prepare(&mut self, value: &str, font: &FieldFont) {
        if self
            .key
            .as_ref()
            .is_some_and(|(v, f)| v == value && f == font)
        {
            return;
        }
        self.bounds.clear();
        self.bounds.push(0);
        self.bounds
            .extend(value.grapheme_indices(true).map(|(i, g)| i + g.len()));
        self.px.clear();
        self.px.resize(self.bounds.len(), f32::NAN);
        self.px[0] = 0.0;
        self.key = Some((value.to_string(), font.clone()));
    }

    /// The width of the `i`th boundary's prefix, measured on first use.
    fn nth(&mut self, i: usize, value: &str, measure: &mut dyn FnMut(&str) -> f32) -> f32 {
        if self.px[i].is_nan() {
            self.px[i] = measure(&value[..self.bounds[i]]);
        }
        self.px[i]
    }

    /// The width of `value[..off]`. An `off` that is not a grapheme boundary
    /// (never produced by an edit, but a caller may ask) is measured directly
    /// and not remembered.
    fn at(&mut self, off: usize, value: &str, measure: &mut dyn FnMut(&str) -> f32) -> f32 {
        if off == 0 {
            return 0.0;
        }
        match self.bounds.binary_search(&off) {
            Ok(i) => self.nth(i, value, measure),
            Err(_) => measure(&value[..off.min(value.len())]),
        }
    }

    /// The boundary nearest to `x`, measured from the start of the text: a
    /// press past the middle of a grapheme lands after it.
    fn nearest(&mut self, x: f32, value: &str, measure: &mut dyn FnMut(&str) -> f32) -> usize {
        let last = self.bounds.len() - 1;
        if last == 0 || x <= 0.0 {
            return 0;
        }
        // The first boundary whose prefix reaches `x`.
        let (mut lo, mut hi) = (1, last + 1);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.nth(mid, value, measure) >= x {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        if lo > last {
            return self.bounds[last];
        }
        let after = self.nth(lo, value, measure);
        let before = self.nth(lo - 1, value, measure);
        if x - before < after - x {
            self.bounds[lo - 1]
        } else {
            self.bounds[lo]
        }
    }

    /// The start of the grapheme that `x` is over, which is what a double
    /// press selects a word *around*. Past the end it is the last grapheme.
    fn containing(&mut self, x: f32, value: &str, measure: &mut dyn FnMut(&str) -> f32) -> usize {
        let last = self.bounds.len() - 1;
        if last == 0 || x <= 0.0 {
            return 0;
        }
        // The last boundary at or before `x`, but never the end.
        let (mut lo, mut hi) = (0, last - 1);
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if self.nth(mid, value, measure) <= x {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        self.bounds[lo]
    }
}

/// One field's editing state (RFC-0041 §1): where the caret and the selection
/// are, what undo can return to, and how far the text is scrolled.
#[derive(Clone, Debug, Default)]
pub(crate) struct EditState {
    /// Byte offset of the caret, always on a grapheme boundary.
    pub caret: usize,
    /// The other end of the selection; equal to `caret` when nothing is
    /// selected.
    pub anchor: usize,
    /// How far the text is scrolled left, in logical pixels, so the caret is
    /// inside the field (§6).
    pub scroll: f32,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The value this state was computed against. A different one means the
    /// value was written from somewhere else.
    last_seen: String,
    typing: Option<Typing>,
    /// Whether a press began in this field and has not been released: the
    /// pointer is dragging a selection.
    pub dragging: bool,
    widths: Widths,
    /// The `frame_seq` this state was last rendered on; one that is not
    /// rendered any more is dropped.
    pub seen: u64,
}

/// Which keys are the shortcut, word and line modifiers on this platform
/// (RFC-0041 §Q5). A value rather than a `cfg!` so both platforms' mapping is
/// tested wherever the tests run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Keymap {
    /// macOS: `Cmd` is the shortcut key, `Option` moves by word, and `Cmd`
    /// with Left or Right goes to either end.
    pub mac: bool,
}

impl Keymap {
    /// This build's platform.
    pub const HERE: Self = Self {
        mac: cfg!(target_os = "macos"),
    };

    /// Whether the shortcut modifier is held (`Cmd` on macOS, `Ctrl` elsewhere).
    fn shortcut(self, m: KeyModifiers) -> bool {
        if self.mac { m.meta } else { m.ctrl }
    }

    /// Whether the word modifier is held (`Option` on macOS, `Ctrl` elsewhere).
    fn word(self, m: KeyModifiers) -> bool {
        if self.mac { m.alt } else { m.ctrl }
    }

    /// Whether this press is a shortcut and therefore not text: the shortcut
    /// key is down and `Alt` is not. `Ctrl` and `Alt` together is `AltGr` on
    /// Windows layouts, which types `@` and `€`.
    pub fn is_shortcut(self, m: KeyModifiers) -> bool {
        self.shortcut(m) && (self.mac || !m.alt)
    }
}

/// Whether `c` ends a run of typed characters: a space or punctuation, typed
/// after a letter (RFC-0041 §5).
fn ends_a_word(c: char) -> bool {
    !c.is_alphanumeric()
}

/// A paste as a field takes it: one line (§Q2). Every kind of line break
/// becomes one space, and other control characters, which a field cannot
/// draw, are dropped.
pub(crate) fn single_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // "\r\n" is one break, not two.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(' ');
            }
            '\n' | '\u{2028}' | '\u{2029}' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The grapheme boundary before `at`, or `0`.
pub(crate) fn prev_grapheme(value: &str, at: usize) -> usize {
    value[..at]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}

/// The grapheme boundary after `at`, or the end.
pub(crate) fn next_grapheme(value: &str, at: usize) -> usize {
    value[at..]
        .graphemes(true)
        .next()
        .map_or(value.len(), |g| at + g.len())
}

/// The start of the word before `at`: the greatest word start below it, or `0`.
pub(crate) fn prev_word(value: &str, at: usize) -> usize {
    value
        .unicode_word_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i < at)
        .last()
        .unwrap_or(0)
}

/// The end of the word after `at`: the least word end above it, or the end.
pub(crate) fn next_word(value: &str, at: usize) -> usize {
    value
        .unicode_word_indices()
        .map(|(i, w)| i + w.len())
        .find(|&end| end > at)
        .unwrap_or(value.len())
}

/// The run around `at` that a double press selects: the word it is in, or the
/// spaces and punctuation between two words.
pub(crate) fn word_around(value: &str, at: usize) -> (usize, usize) {
    let mut last = (0, 0);
    for (i, segment) in value.split_word_bound_indices() {
        last = (i, i + segment.len());
        if at < last.1 {
            return last;
        }
    }
    last
}

/// Whether `i` is a grapheme boundary of `value`.
fn is_boundary(value: &str, i: usize) -> bool {
    i <= value.len()
        && value.is_char_boundary(i)
        && GraphemeCursor::new(i, value.len(), true)
            .is_boundary(value, 0)
            .unwrap_or(true)
}

/// The nearest grapheme boundary at or after `i`.
fn snap(value: &str, i: usize) -> usize {
    let mut i = i.min(value.len());
    while !is_boundary(value, i) {
        i += 1;
    }
    i
}

impl EditState {
    /// The selection, as `(start, end)` with `start <= end`.
    pub fn range(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    /// Whether anything is selected.
    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    /// Brings the state in line with the field's value before an event is
    /// applied to it. A value that is not the one this state last saw was
    /// written from outside (an action, a reply): the text the caret pointed
    /// into is gone, so the caret goes to the end, the selection collapses and
    /// undo starts over (RFC-0041 §1).
    pub fn sync(&mut self, value: &str) {
        if self.last_seen == value {
            return;
        }
        self.last_seen.clear();
        self.last_seen.push_str(value);
        self.caret = value.len();
        self.anchor = value.len();
        self.undo.clear();
        self.redo.clear();
        self.typing = None;
    }

    /// Checks the invariant of §Q1 in debug builds: both offsets sit on a
    /// grapheme boundary of the value.
    fn check(&self, value: &str) {
        debug_assert!(
            is_boundary(value, self.caret) && is_boundary(value, self.anchor),
            "caret {} / anchor {} are not grapheme boundaries of {value:?}",
            self.caret,
            self.anchor
        );
    }

    /// Moves the caret, keeping the anchor when `extend`, and says whether
    /// anything moved.
    fn move_to(&mut self, value: &str, to: usize, extend: bool) -> Outcome {
        let before = (self.caret, self.anchor);
        self.caret = snap(value, to);
        if !extend {
            self.anchor = self.caret;
        }
        self.typing = None;
        self.check(value);
        if (self.caret, self.anchor) == before {
            Outcome::None
        } else {
            Outcome::Moved
        }
    }

    /// Pushes the state before an edit, unless the edit continues a step.
    fn begin_step(&mut self, value: &str) {
        self.undo.push(Snapshot {
            value: value.to_string(),
            caret: self.caret,
            anchor: self.anchor,
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }

    /// Replaces `value[lo..hi]` with `with` and puts the caret after it.
    ///
    /// `step` says how it joins undo: `None` is a step of its own; `Some`
    /// is typed text, which continues the run in progress when there is one.
    fn replace(
        &mut self,
        value: &str,
        (lo, hi): (usize, usize),
        with: &str,
        typed_at: Option<u64>,
    ) -> Outcome {
        if value[lo..hi] == *with {
            // The selection is replaced by itself: nothing changed, so
            // nothing is recorded and nothing fires.
            return self.move_to(value, lo + with.len(), false);
        }
        let continues = typed_at.is_some_and(|now| {
            lo == hi
                && self.typing.is_some_and(|t| {
                    t.caret == self.caret && now.saturating_sub(t.at_ms) < COALESCE_MS
                })
        });
        if !continues {
            self.begin_step(value);
        }
        self.redo.clear();
        let mut new = String::with_capacity(value.len() - (hi - lo) + with.len());
        new.push_str(&value[..lo]);
        new.push_str(with);
        new.push_str(&value[hi..]);
        self.caret = snap(&new, lo + with.len());
        self.anchor = self.caret;
        self.typing = typed_at.map(|at_ms| Typing {
            at_ms,
            caret: self.caret,
        });
        self.last_seen.clone_from(&new);
        self.check(&new);
        Outcome::Value(new)
    }

    /// Typed text, or an input method's commit: replaces the selection, or is
    /// inserted at the caret (RFC-0041 §1).
    pub fn insert(&mut self, value: &str, text: &str, now_ms: u64) -> Outcome {
        let text = single_line(text);
        if text.is_empty() {
            return Outcome::None;
        }
        let range = self.range();
        // A single character joins the run being typed; anything longer (a
        // commit, a paste that came as text) is a step of its own.
        let one = {
            let mut chars = text.chars();
            chars.next().is_some() && chars.next().is_none()
        };
        let after_a_letter = value[..range.0]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric);
        let out = self.replace(value, range, &text, one.then_some(now_ms));
        // A space or punctuation typed after a letter ends the run, so the
        // next character starts a new step: "hello world" undoes to "hello ".
        if one && after_a_letter && text.chars().next().is_some_and(ends_a_word) {
            self.typing = None;
        }
        out
    }

    /// Removes the selection, or what `span` says is before or after the
    /// caret. Always a step of its own.
    fn delete(&mut self, value: &str, backwards: bool, by: Span) -> Outcome {
        let range = if self.has_selection() {
            self.range()
        } else {
            let to = match (backwards, by) {
                (true, Span::Grapheme) => prev_grapheme(value, self.caret),
                (true, Span::Word) => prev_word(value, self.caret),
                (false, Span::Grapheme) => next_grapheme(value, self.caret),
                (false, Span::Word) => next_word(value, self.caret),
            };
            (to.min(self.caret), to.max(self.caret))
        };
        if range.0 == range.1 {
            return Outcome::None;
        }
        self.replace(value, range, "", None)
    }

    /// Copies the selection to the clipboard. Nothing selected: nothing
    /// happens, the clipboard keeps what it had.
    fn copy(&self, value: &str, clipboard: &mut dyn Clipboard) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (lo, hi) = self.range();
        clipboard.write_text(&value[lo..hi]);
        true
    }

    /// Replaces the selection with the clipboard's text, as one line. An empty
    /// or unreadable clipboard pastes nothing.
    fn paste(&mut self, value: &str, clipboard: &mut dyn Clipboard) -> Outcome {
        let Some(text) = clipboard.read_text() else {
            return Outcome::None;
        };
        let text = single_line(&text);
        if text.is_empty() {
            return Outcome::None;
        }
        let range = self.range();
        self.replace(value, range, &text, None)
    }

    /// Steps back through undo (RFC-0041 §5).
    fn undo(&mut self, value: &str) -> Outcome {
        let Some(prev) = self.undo.pop() else {
            return Outcome::None;
        };
        self.redo.push(Snapshot {
            value: value.to_string(),
            caret: self.caret,
            anchor: self.anchor,
        });
        self.restore(prev)
    }

    /// Steps forward again through what undo stepped back over.
    fn redo(&mut self, value: &str) -> Outcome {
        let Some(next) = self.redo.pop() else {
            return Outcome::None;
        };
        self.undo.push(Snapshot {
            value: value.to_string(),
            caret: self.caret,
            anchor: self.anchor,
        });
        self.restore(next)
    }

    /// Makes `snapshot` the state of the field.
    fn restore(&mut self, snapshot: Snapshot) -> Outcome {
        let Snapshot {
            value,
            caret,
            anchor,
        } = snapshot;
        self.caret = snap(&value, caret);
        self.anchor = snap(&value, anchor);
        self.typing = None;
        self.last_seen.clone_from(&value);
        self.check(&value);
        Outcome::Value(value)
    }

    /// A key pressed in the field (RFC-0041's key table). `key` is the key's
    /// name as the platform reports it (`"ArrowLeft"`, `"Backspace"`, `"a"`).
    ///
    /// The caller has run [`sync`](Self::sync) with `value`.
    pub fn on_key(
        &mut self,
        value: &str,
        key: &str,
        mods: KeyModifiers,
        keys: Keymap,
        clipboard: &mut dyn Clipboard,
    ) -> Outcome {
        let extend = mods.shift;
        let word = keys.word(mods);
        let line = keys.mac && mods.meta;
        // Offsets are only trusted on a boundary of this value.
        let caret = self.caret;
        match key {
            "ArrowLeft" | "ArrowRight" => {
                let back = key == "ArrowLeft";
                if line {
                    return self.move_to(value, if back { 0 } else { value.len() }, extend);
                }
                if !extend && !word && self.has_selection() {
                    let (lo, hi) = self.range();
                    return self.move_to(value, if back { lo } else { hi }, false);
                }
                let to = match (back, word) {
                    (true, true) => prev_word(value, caret),
                    (true, false) => prev_grapheme(value, caret),
                    (false, true) => next_word(value, caret),
                    (false, false) => next_grapheme(value, caret),
                };
                self.move_to(value, to, extend)
            }
            "Home" => self.move_to(value, 0, extend),
            "End" => self.move_to(value, value.len(), extend),
            "Backspace" | "Delete" => {
                let by = if word { Span::Word } else { Span::Grapheme };
                self.typing = None;
                self.delete(value, key == "Backspace", by)
            }
            _ if keys.is_shortcut(mods) => self.shortcut(value, key, mods, clipboard),
            _ => Outcome::None,
        }
    }

    /// A key pressed with the shortcut modifier held: select all, copy, cut,
    /// paste, undo and redo. Any other is not the field's.
    fn shortcut(
        &mut self,
        value: &str,
        key: &str,
        mods: KeyModifiers,
        clipboard: &mut dyn Clipboard,
    ) -> Outcome {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("a") {
            self.typing = None;
            let before = (self.caret, self.anchor);
            self.anchor = 0;
            self.caret = value.len();
            return if before == (self.caret, self.anchor) {
                Outcome::None
            } else {
                Outcome::Moved
            };
        }
        if is("c") {
            self.copy(value, clipboard);
            return Outcome::None;
        }
        if is("x") {
            self.typing = None;
            if self.copy(value, clipboard) {
                return self.delete(value, true, Span::Grapheme);
            }
            return Outcome::None;
        }
        if is("v") {
            self.typing = None;
            return self.paste(value, clipboard);
        }
        if is("z") {
            self.typing = None;
            return if mods.shift {
                self.redo(value)
            } else {
                self.undo(value)
            };
        }
        if is("y") {
            self.typing = None;
            return self.redo(value);
        }
        Outcome::None
    }

    // ── the pointer ──────────────────────────────────────────────────────

    /// Gets the cached widths ready for `value` in `font`.
    fn prepare(&mut self, value: &str, font: &FieldFont) {
        self.widths.prepare(value, font);
    }

    /// The x of the boundary at `off`, measured from the start of the text.
    pub fn x_of(
        &mut self,
        value: &str,
        font: &FieldFont,
        off: usize,
        measure: &mut dyn FnMut(&str) -> f32,
    ) -> f32 {
        self.prepare(value, font);
        self.widths.at(off, value, measure)
    }

    /// A press at `x` (from the start of the text): the caret goes to the
    /// nearest grapheme boundary, and with `extend` (Shift) the selection
    /// grows from where it was.
    pub fn press(
        &mut self,
        value: &str,
        font: &FieldFont,
        x: f32,
        extend: bool,
        measure: &mut dyn FnMut(&str) -> f32,
    ) -> Outcome {
        self.prepare(value, font);
        let to = self.widths.nearest(x, value, measure);
        self.dragging = true;
        self.move_to(value, to, extend)
    }

    /// The pointer moved with the press held: the caret follows it and the
    /// anchor stays. Past either edge of the field (`edge`) it steps one
    /// grapheme that way instead (§4).
    pub fn drag(
        &mut self,
        value: &str,
        font: &FieldFont,
        x: f32,
        edge: Option<Edge>,
        measure: &mut dyn FnMut(&str) -> f32,
    ) -> Outcome {
        if !self.dragging {
            return Outcome::None;
        }
        let to = match edge {
            Some(Edge::Start) => prev_grapheme(value, self.caret),
            Some(Edge::End) => next_grapheme(value, self.caret),
            None => {
                self.prepare(value, font);
                self.widths.nearest(x, value, measure)
            }
        };
        self.move_to(value, to, true)
    }

    /// A double press at `x`: selects the word under the pointer.
    pub fn select_word(
        &mut self,
        value: &str,
        font: &FieldFont,
        x: f32,
        measure: &mut dyn FnMut(&str) -> f32,
    ) -> Outcome {
        self.prepare(value, font);
        let under = self.widths.containing(x, value, measure);
        let (lo, hi) = word_around(value, under);
        let before = (self.caret, self.anchor);
        self.anchor = lo;
        self.caret = hi;
        self.typing = None;
        self.check(value);
        if before == (self.caret, self.anchor) {
            Outcome::None
        } else {
            Outcome::Moved
        }
    }

    /// Keeps the caret inside the field (§6): scrolls the text so the caret,
    /// `caret_x` from the start of it, is at least [`CARET_MARGIN`] from both
    /// edges of a content box `view_w` wide, and never further than the text
    /// needs. `total` is the text's width.
    pub fn follow_caret(&mut self, caret_x: f32, total: f32, view_w: f32) {
        let margin = CARET_MARGIN.min(view_w / 2.0);
        let furthest = (total + margin - view_w).max(0.0);
        let mut scroll = self.scroll;
        if caret_x - scroll > view_w - margin {
            scroll = caret_x - (view_w - margin);
        }
        if caret_x - scroll < margin {
            scroll = caret_x - margin;
        }
        self.scroll = scroll.clamp(0.0, furthest);
    }
}

/// How much one press of Backspace or Delete removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Span {
    Grapheme,
    Word,
}

/// Which edge of the field a dragging pointer is past.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    /// Before the start of the content box.
    Start,
    /// After the end of it.
    End,
}

/// The editing state of every field that has needed one, by element index
/// (RFC-0041 §1). Empty for an app with no focused `TextField`.
#[derive(Debug, Default)]
pub(crate) struct EditStore {
    fields: HashMap<u32, EditState>,
    /// Set by anything that moved the caret or changed the value, so the
    /// blink starts over with the caret showing; the interpreter clears it.
    pub blink_reset: bool,
}

impl EditStore {
    /// The state of field `elem`, made on first use.
    pub fn state(&mut self, elem: u32) -> &mut EditState {
        self.fields.entry(elem).or_default()
    }

    /// The state of field `elem`, if it has had one.
    pub fn get(&self, elem: u32) -> Option<&EditState> {
        self.fields.get(&elem)
    }

    /// The state of field `elem`, if it has had one, to change.
    pub fn get_mut(&mut self, elem: u32) -> Option<&mut EditState> {
        self.fields.get_mut(&elem)
    }

    /// Marks field `elem`'s state, if it has one, as rendered on `frame_seq`.
    pub fn touch(&mut self, elem: u32, frame_seq: u64) {
        if let Some(state) = self.fields.get_mut(&elem) {
            state.seen = frame_seq;
        }
    }

    /// Whether no field has a state: the steady state of an app that has not
    /// put focus in a `TextField`.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Drops the state of every field that was not rendered on `frame_seq`:
    /// its element was torn down.
    pub fn sweep(&mut self, frame_seq: u64) {
        self.fields.retain(|_, s| s.seen == frame_seq);
    }

    /// How many fields hold a state. Test-facing.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Records the outcome of an event on the blink: anything but nothing
    /// restarts it.
    pub fn note(&mut self, outcome: &Outcome) {
        if !matches!(outcome, Outcome::None) {
            self.blink_reset = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byard_core::MemoryClipboard;

    const LINUX: Keymap = Keymap { mac: false };
    const MAC: Keymap = Keymap { mac: true };

    fn none() -> KeyModifiers {
        KeyModifiers::default()
    }

    fn shift() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            ..none()
        }
    }

    fn ctrl() -> KeyModifiers {
        KeyModifiers {
            ctrl: true,
            ..none()
        }
    }

    /// A field's value and state, driven the way the interpreter drives them.
    struct Field {
        value: String,
        state: EditState,
        clip: MemoryClipboard,
        keys: Keymap,
    }

    impl Field {
        fn new(value: &str) -> Self {
            let mut state = EditState::default();
            state.sync(value);
            Self {
                value: value.to_string(),
                state,
                clip: MemoryClipboard::new(),
                keys: LINUX,
            }
        }

        fn apply(&mut self, out: Outcome) {
            if let Outcome::Value(v) = out {
                self.value = v;
            }
        }

        fn key(&mut self, key: &str, mods: KeyModifiers) {
            let out = self
                .state
                .on_key(&self.value, key, mods, self.keys, &mut self.clip);
            self.apply(out);
        }

        fn type_at(&mut self, text: &str, at_ms: u64) {
            let out = self.state.insert(&self.value, text, at_ms);
            self.apply(out);
        }

        /// `|` at the caret, `[` `]` around a selection: the field as read.
        fn shown(&self) -> String {
            let (lo, hi) = self.state.range();
            let mut s = self.value.clone();
            if lo == hi {
                s.insert(lo, '|');
            } else {
                s.insert(hi, ']');
                s.insert(lo, '[');
            }
            s
        }
    }

    // The graphemes the table is tested with: more than one code point each.
    const E_ACUTE: &str = "e\u{301}"; // 2 code points
    const FLAG: &str = "\u{1F1FA}\u{1F1F8}"; // 2 regional indicators
    const THUMB: &str = "\u{1F44D}\u{1F3FD}"; // an emoji and its skin tone

    #[test]
    fn the_caret_moves_by_grapheme_not_by_code_point() {
        let mut f = Field::new(&format!("a{E_ACUTE}{FLAG}{THUMB}b"));
        assert_eq!(f.state.caret, f.value.len(), "a new field has its end");
        let mut seen = vec![];
        for _ in 0..5 {
            f.key("ArrowLeft", none());
            seen.push(f.state.caret);
        }
        // Each press crosses exactly one grapheme.
        let want = [
            f.value.len() - 1,
            f.value.len() - 1 - THUMB.len(),
            f.value.len() - 1 - THUMB.len() - FLAG.len(),
            1,
            0,
        ];
        assert_eq!(seen, want);
        f.key("ArrowRight", none());
        assert_eq!(f.state.caret, 1);
        f.key("ArrowRight", none());
        assert_eq!(f.state.caret, 1 + E_ACUTE.len());
    }

    #[test]
    fn home_and_end_go_to_either_end_and_shift_extends() {
        let mut f = Field::new("hello");
        f.key("Home", none());
        assert_eq!(f.shown(), "|hello");
        f.key("End", none());
        assert_eq!(f.shown(), "hello|");
        f.key("Home", shift());
        assert_eq!(f.shown(), "[hello]");
        assert_eq!(f.state.caret, 0, "the caret is the end that moved");
        assert_eq!(f.state.anchor, 5, "the anchor stayed");
        f.key("End", shift());
        assert_eq!(f.shown(), "hello|", "and back to nothing selected");
    }

    #[test]
    fn a_plain_arrow_collapses_a_selection_to_its_start_or_end() {
        let mut f = Field::new("hello world");
        f.state.anchor = 2;
        f.state.caret = 7;
        f.key("ArrowLeft", none());
        assert_eq!(f.shown(), "he|llo world", "Left collapses to the start");
        f.state.anchor = 2;
        f.state.caret = 7;
        f.key("ArrowRight", none());
        assert_eq!(f.shown(), "hello w|orld", "Right collapses to the end");
        // A selection made backwards collapses the same way.
        f.state.anchor = 7;
        f.state.caret = 2;
        f.key("ArrowRight", none());
        assert_eq!(f.state.caret, 7);
    }

    #[test]
    fn shift_with_an_arrow_grows_the_selection_a_grapheme_at_a_time() {
        let mut f = Field::new(&format!("x{FLAG}y"));
        f.key("ArrowLeft", shift());
        f.key("ArrowLeft", shift());
        assert_eq!(f.state.range(), (1, f.value.len()), "the flag, whole");
        f.key("ArrowRight", shift());
        assert_eq!(f.state.range(), (f.value.len() - 1, f.value.len()));
    }

    #[test]
    fn the_word_modifier_moves_by_word_in_text_with_accents() {
        let mut f = Field::new("naïve café au lait");
        f.key("ArrowLeft", ctrl());
        assert_eq!(f.shown(), "naïve café au |lait");
        f.key("ArrowLeft", ctrl());
        assert_eq!(f.shown(), "naïve café |au lait");
        f.key("ArrowLeft", ctrl());
        assert_eq!(
            f.shown(),
            "naïve |café au lait",
            "ï and é are in their word"
        );
        f.key("Home", none());
        f.key("ArrowRight", ctrl());
        assert_eq!(f.shown(), "naïve| café au lait", "to the end of the word");
        f.key("ArrowRight", ctrl());
        assert_eq!(f.shown(), "naïve café| au lait");
        f.key("ArrowRight", ctrl());
        f.key("ArrowRight", ctrl());
        f.key("ArrowRight", ctrl());
        assert_eq!(f.shown(), "naïve café au lait|", "and stops at the end");
    }

    #[test]
    fn the_word_modifier_with_shift_extends_by_word() {
        let mut f = Field::new("one two three");
        f.key(
            "ArrowLeft",
            KeyModifiers {
                shift: true,
                ..ctrl()
            },
        );
        assert_eq!(f.shown(), "one two [three]");
    }

    #[test]
    fn on_macos_option_moves_by_word_and_cmd_goes_to_either_end() {
        let mut f = Field::new("one two three");
        f.keys = MAC;
        let option = KeyModifiers {
            alt: true,
            ..none()
        };
        let cmd = KeyModifiers {
            meta: true,
            ..none()
        };
        f.key("ArrowLeft", option);
        assert_eq!(f.shown(), "one two |three");
        f.key("ArrowLeft", cmd);
        assert_eq!(f.shown(), "|one two three", "Cmd+Left is Home");
        f.key("ArrowRight", cmd);
        assert_eq!(f.shown(), "one two three|", "Cmd+Right is End");
        // Ctrl is not the word key there.
        f.key("ArrowLeft", ctrl());
        assert_eq!(f.shown(), "one two thre|e");
    }

    #[test]
    fn backspace_removes_the_selection_or_the_grapheme_before_the_caret() {
        let mut f = Field::new(&format!("a{E_ACUTE}{THUMB}"));
        f.key("Backspace", none());
        assert_eq!(f.value, format!("a{E_ACUTE}"), "the emoji, whole");
        f.key("Backspace", none());
        assert_eq!(f.value, "a", "the accented letter, whole");
        f.state.anchor = 0;
        f.state.caret = 1;
        f.key("Backspace", none());
        assert_eq!(f.value, "", "a selection goes at once");
        assert_eq!(f.state.caret, 0);
    }

    #[test]
    fn delete_removes_the_grapheme_after_the_caret_and_not_the_whole_field() {
        // The defect this RFC fixes: Delete used to write an empty string.
        let mut f = Field::new(&format!("{FLAG}ab"));
        f.key("Home", none());
        f.key("Delete", none());
        assert_eq!(f.value, "ab", "the flag, whole, and nothing else");
        assert_eq!(f.state.caret, 0);
        f.key("Delete", none());
        assert_eq!(f.value, "b");
        f.key("End", none());
        assert_eq!(f.state.caret, 1);
    }

    #[test]
    fn delete_at_the_end_and_backspace_at_the_start_change_nothing() {
        let mut f = Field::new("ab");
        let out = f.state.on_key("ab", "Delete", none(), LINUX, &mut f.clip);
        assert_eq!(out, Outcome::None, "Q3: nothing, so nothing fires");
        f.key("Home", none());
        let out = f
            .state
            .on_key("ab", "Backspace", none(), LINUX, &mut f.clip);
        assert_eq!(out, Outcome::None);
    }

    #[test]
    fn the_word_modifier_deletes_to_the_word_boundary() {
        let mut f = Field::new("one two three");
        f.key("Backspace", ctrl());
        assert_eq!(f.value, "one two ");
        f.key("Backspace", ctrl());
        assert_eq!(f.value, "one ");
        f.key("Home", none());
        f.key("Delete", ctrl());
        assert_eq!(f.value, " ", "to the end of the word, not past the space");
    }

    #[test]
    fn select_all_then_typing_replaces_everything() {
        let mut f = Field::new("hello");
        f.key("a", ctrl());
        assert_eq!(f.shown(), "[hello]");
        f.type_at("x", 0);
        assert_eq!(f.value, "x");
        // The shortcut is the same with Shift held, which capitalises the key.
        f.key(
            "A",
            KeyModifiers {
                shift: true,
                ..ctrl()
            },
        );
        assert_eq!(f.shown(), "[x]");
    }

    #[test]
    fn copy_and_cut_use_the_selection_and_do_nothing_without_one() {
        let mut f = Field::new("hello world");
        f.key("c", ctrl());
        assert_eq!(f.clip.read_text(), None, "nothing selected: nothing copied");
        f.key("x", ctrl());
        assert_eq!(f.value, "hello world", "and nothing cut");
        f.state.anchor = 0;
        f.state.caret = 5;
        f.key("c", ctrl());
        assert_eq!(f.clip.read_text().as_deref(), Some("hello"));
        assert_eq!(f.value, "hello world", "copy leaves the text");
        f.key("x", ctrl());
        assert_eq!(f.value, " world");
        assert_eq!(f.clip.read_text().as_deref(), Some("hello"));
        assert_eq!(f.state.caret, 0);
    }

    #[test]
    fn paste_replaces_the_selection_and_turns_newlines_into_spaces() {
        let mut f = Field::new("say  now");
        f.clip.write_text("hi\r\nthere\nfriend\rok");
        f.state.anchor = 4;
        f.state.caret = 4;
        f.key("v", ctrl());
        assert_eq!(f.value, "say hi there friend ok now");
        assert_eq!(f.state.caret, "say hi there friend ok".len(), "after it");
        // Over a selection.
        let mut g = Field::new("abcdef");
        g.clip.write_text("X");
        g.state.anchor = 1;
        g.state.caret = 5;
        g.key("v", ctrl());
        assert_eq!(g.value, "aXf");
    }

    #[test]
    fn pasting_nothing_changes_nothing() {
        let mut f = Field::new("abc");
        let out = f.state.on_key("abc", "v", ctrl(), LINUX, &mut f.clip);
        assert_eq!(out, Outcome::None, "an empty clipboard");
        f.clip.write_text("\r\n");
        // A paste of only a line break is one space, which is something.
        f.key("v", ctrl());
        assert_eq!(f.value, "abc ");
        f.clip.write_text("\u{7}\u{1b}");
        let out = f.state.on_key(&f.value, "v", ctrl(), LINUX, &mut f.clip);
        assert_eq!(out, Outcome::None, "only control characters: nothing");
    }

    #[test]
    fn typing_hello_world_then_undo_leaves_hello_and_a_space() {
        let mut f = Field::new("");
        for (i, c) in "hello world".chars().enumerate() {
            f.type_at(&c.to_string(), i as u64 * 50);
        }
        assert_eq!(f.value, "hello world");
        f.key("z", ctrl());
        assert_eq!(f.value, "hello ", "undo removes the last word");
        f.key("z", ctrl());
        assert_eq!(f.value, "", "and the one before it, together");
        f.key("z", ctrl());
        assert_eq!(f.value, "", "and then there is nothing to undo");
        f.key(
            "z",
            KeyModifiers {
                shift: true,
                ..ctrl()
            },
        );
        assert_eq!(f.value, "hello ", "redo, with Shift");
        f.key("y", ctrl());
        assert_eq!(f.value, "hello world", "redo, with Y");
    }

    #[test]
    fn a_pause_of_a_second_ends_the_run_of_typing() {
        let mut f = Field::new("");
        f.type_at("a", 0);
        f.type_at("b", 100);
        f.type_at("c", 1500);
        f.type_at("d", 1550);
        f.key("z", ctrl());
        assert_eq!(f.value, "ab");
        f.key("z", ctrl());
        assert_eq!(f.value, "");
    }

    #[test]
    fn moving_the_caret_ends_the_run_of_typing() {
        let mut f = Field::new("");
        f.type_at("a", 0);
        f.type_at("b", 10);
        f.key("ArrowLeft", none());
        f.key("ArrowRight", none());
        f.type_at("c", 20);
        f.key("z", ctrl());
        assert_eq!(f.value, "ab", "the caret moved between b and c");
    }

    #[test]
    fn a_paste_is_one_step_and_so_is_a_cut_and_a_deleted_selection() {
        let mut f = Field::new("");
        f.type_at("a", 0);
        f.clip.write_text("12345");
        f.key("v", ctrl());
        f.type_at("b", 10);
        assert_eq!(f.value, "a12345b");
        f.key("z", ctrl());
        assert_eq!(f.value, "a12345", "typing after a paste is its own step");
        f.key("z", ctrl());
        assert_eq!(f.value, "a", "the paste, whole");
        f.state.anchor = 0;
        f.key("x", ctrl());
        assert_eq!(f.value, "");
        f.key("z", ctrl());
        assert_eq!(f.value, "a", "a cut is a step");
        assert_eq!(
            (f.state.anchor, f.state.caret),
            (0, 1),
            "with its selection"
        );
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut f = Field::new("");
        f.type_at("a", 0);
        f.key("Backspace", none());
        f.key("z", ctrl());
        assert_eq!(f.value, "a");
        f.type_at("z", 5000);
        assert!(f.state.redo.is_empty(), "redo after a new edit is empty");
        f.key("y", ctrl());
        assert_eq!(f.value, "az", "so redo does nothing");
    }

    #[test]
    fn undo_is_bounded_and_the_oldest_step_drops() {
        let mut f = Field::new("");
        for i in 0..(UNDO_LIMIT + 20) {
            f.type_at("a", i as u64 * 5000); // each its own step
        }
        assert_eq!(f.state.undo.len(), UNDO_LIMIT);
        for _ in 0..(UNDO_LIMIT + 20) {
            f.key("z", ctrl());
        }
        assert_eq!(f.value.len(), 20, "the first twenty could not be undone");
    }

    #[test]
    fn a_value_written_from_outside_moves_the_caret_to_the_end_and_forgets_undo() {
        let mut f = Field::new("");
        f.type_at("a", 0);
        f.key("Home", none());
        f.value = "written elsewhere".into();
        f.state.sync(&f.value);
        assert_eq!(f.state.caret, f.value.len());
        assert_eq!(f.state.anchor, f.value.len(), "the selection collapsed");
        let out = f.state.on_key(&f.value, "z", ctrl(), LINUX, &mut f.clip);
        assert_eq!(out, Outcome::None, "undo is gone");
        // Syncing with the same value is a no-op.
        f.key("Home", none());
        f.state.sync(&f.value);
        assert_eq!(f.state.caret, 0);
    }

    #[test]
    fn typed_text_replaces_the_selection() {
        let mut f = Field::new("hello world");
        f.state.anchor = 6;
        f.state.caret = 11;
        f.type_at("there", 0);
        assert_eq!(f.value, "hello there");
        f.key("z", ctrl());
        assert_eq!(
            f.value, "hello world",
            "typing over a selection undoes whole"
        );
        assert_eq!(
            (f.state.anchor, f.state.caret),
            (6, 11),
            "with its selection"
        );
    }

    #[test]
    fn a_shortcut_is_not_text_and_altgr_is() {
        assert!(LINUX.is_shortcut(ctrl()));
        assert!(
            !LINUX.is_shortcut(KeyModifiers {
                alt: true,
                ..ctrl()
            }),
            "Ctrl+Alt is AltGr: it types"
        );
        assert!(!LINUX.is_shortcut(none()));
        assert!(MAC.is_shortcut(KeyModifiers {
            meta: true,
            ..none()
        }));
        assert!(
            !MAC.is_shortcut(KeyModifiers {
                alt: true,
                ..none()
            }),
            "Option types on macOS (dead keys, symbols)"
        );
    }

    #[test]
    fn an_inserted_line_break_becomes_a_space() {
        assert_eq!(single_line("a\nb\r\nc\rd\u{2028}e"), "a b c d e");
        assert_eq!(single_line("tab\there"), "tabhere", "controls are dropped");
        assert_eq!(single_line("ünï😀"), "ünï😀");
    }

    #[test]
    fn a_double_press_selects_the_word_around_the_grapheme_under_it() {
        assert_eq!(word_around("hello big world", 7), (6, 9));
        assert_eq!(word_around("hello big world", 5), (5, 6), "the space");
        assert_eq!(word_around("hello big world", 0), (0, 5));
        assert_eq!(word_around("hello", 5), (0, 5), "past the end: the last");
        assert_eq!(word_around("", 0), (0, 0));
        assert_eq!(word_around("naïve café", 8), (7, 12), "accents stay in");
    }

    /// A measurer where every character, and so every grapheme's first, is
    /// 10 px: easy to place a pointer against.
    fn ten_wide(text: &str) -> f32 {
        text.chars().count() as f32 * 10.0
    }

    fn font() -> FieldFont {
        FieldFont {
            size: 16.0,
            weight: 400,
            family: None,
        }
    }

    #[test]
    fn a_press_lands_on_the_nearest_boundary() {
        let mut s = EditState::default();
        s.sync("abcd");
        let mut m = ten_wide;
        s.press("abcd", &font(), 14.0, false, &mut m);
        assert_eq!(s.caret, 1, "14 is nearer 10 than 20");
        s.press("abcd", &font(), 16.0, false, &mut m);
        assert_eq!(s.caret, 2, "past the middle of a grapheme: after it");
        s.press("abcd", &font(), -50.0, false, &mut m);
        assert_eq!(s.caret, 0);
        s.press("abcd", &font(), 500.0, false, &mut m);
        assert_eq!(s.caret, 4);
        // Shift keeps the anchor.
        s.press("abcd", &font(), 0.0, false, &mut m);
        s.press("abcd", &font(), 30.0, true, &mut m);
        assert_eq!(s.range(), (0, 3));
    }

    #[test]
    fn a_press_only_ever_lands_between_graphemes() {
        let value = format!("a{FLAG}b");
        let mut s = EditState::default();
        s.sync(&value);
        // Each code point measures 10 px, so the flag is 20 px wide.
        for x in 0..60 {
            let mut m = ten_wide;
            s.press(&value, &font(), x as f32, false, &mut m);
            assert!(
                is_boundary(&value, s.caret),
                "x = {x} landed inside a grapheme at {}",
                s.caret
            );
        }
    }

    #[test]
    fn a_drag_moves_the_caret_and_keeps_the_anchor_and_measures_each_prefix_once() {
        let value = "abcdefghij";
        let mut s = EditState::default();
        s.sync(value);
        let calls = std::cell::Cell::new(0);
        let mut m = |t: &str| {
            calls.set(calls.get() + 1);
            ten_wide(t)
        };
        s.press(value, &font(), 20.0, false, &mut m);
        s.drag(value, &font(), 70.0, None, &mut m);
        assert_eq!(s.range(), (2, 7));
        assert_eq!(s.anchor, 2);
        let after_first = calls.get();
        for _ in 0..20 {
            s.drag(value, &font(), 70.0, None, &mut m);
        }
        assert_eq!(
            calls.get(),
            after_first,
            "a repeated position measures nothing new"
        );
        // A change of font invalidates what was measured.
        let big = FieldFont {
            size: 32.0,
            ..font()
        };
        s.drag(value, &big, 70.0, None, &mut m);
        assert!(calls.get() > after_first);
    }

    #[test]
    fn a_drag_past_an_edge_steps_one_grapheme() {
        let value = "abcdefghij";
        let mut s = EditState::default();
        s.sync(value);
        let mut m = ten_wide;
        s.press(value, &font(), 50.0, false, &mut m);
        s.drag(value, &font(), -30.0, Some(Edge::Start), &mut m);
        assert_eq!(s.range(), (4, 5));
        s.drag(value, &font(), -30.0, Some(Edge::Start), &mut m);
        assert_eq!(s.range(), (3, 5));
        s.drag(value, &font(), 900.0, Some(Edge::End), &mut m);
        assert_eq!(s.range(), (4, 5), "and back toward the anchor");
    }

    #[test]
    fn a_drag_that_did_not_start_in_the_field_does_nothing() {
        let mut s = EditState::default();
        s.sync("abc");
        let mut m = ten_wide;
        assert_eq!(s.drag("abc", &font(), 20.0, None, &mut m), Outcome::None);
    }

    #[test]
    fn a_double_press_selects_the_word_under_the_pointer() {
        let value = "big red dog";
        let mut s = EditState::default();
        s.sync(value);
        let mut m = ten_wide;
        s.select_word(value, &font(), 55.0, &mut m);
        assert_eq!(s.range(), (4, 7), "x = 55 is over the `e` of `red`");
        s.select_word(value, &font(), 5.0, &mut m);
        assert_eq!(s.range(), (0, 3));
        s.select_word(value, &font(), 9999.0, &mut m);
        assert_eq!(s.range(), (8, 11), "past the end: the last word");
    }

    #[test]
    fn the_text_scrolls_to_keep_the_caret_inside_with_a_margin() {
        let mut s = EditState::default();
        // A box 100 wide holding text 300 wide.
        s.follow_caret(300.0, 300.0, 100.0);
        assert_eq!(s.scroll, 204.0, "the end sits 4 px from the right edge");
        s.follow_caret(0.0, 300.0, 100.0);
        assert_eq!(s.scroll, 0.0, "and back to the start");
        s.follow_caret(50.0, 300.0, 100.0);
        assert_eq!(s.scroll, 0.0, "a caret already inside does not scroll");
        s.follow_caret(150.0, 300.0, 100.0);
        assert_eq!(s.scroll, 54.0);
        s.follow_caret(60.0, 300.0, 100.0);
        assert_eq!(
            s.scroll, 54.0,
            "6 px from the left edge is inside the margin"
        );
        s.follow_caret(57.0, 300.0, 100.0);
        assert_eq!(s.scroll, 53.0, "3 px from it is not");
        // Text that fits never scrolls.
        let mut t = EditState::default();
        t.follow_caret(40.0, 40.0, 100.0);
        assert_eq!(t.scroll, 0.0);
    }

    #[test]
    fn the_store_drops_what_was_not_rendered() {
        let mut store = EditStore::default();
        assert!(store.is_empty());
        store.state(1).seen = 7;
        store.state(2).seen = 6;
        store.sweep(7);
        assert_eq!(store.len(), 1);
        assert!(store.get(1).is_some() && store.get(2).is_none());
        store.note(&Outcome::None);
        assert!(!store.blink_reset);
        store.note(&Outcome::Moved);
        assert!(store.blink_reset);
    }
}
