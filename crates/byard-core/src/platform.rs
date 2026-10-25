//! # Platform abstraction
//!
//! Defines [`PlatformHost`], the only point of contact between the engine
//! core and a concrete windowing backend (`winit`, a future mobile host,
//! the headless *Coreolis* embedding, etc.), per RFC-0001 §6.
//!
//! `byard-core` has zero direct references to `winit`, Wayland, Win32, or
//! any other OS primitive. This module does not either: [`WindowSize`] is
//! expressed in plain `u32`/`f64`, and the only non-`std` types in the trait
//! signature ([`wgpu::Instance`], [`wgpu::Surface`]) are already a dependency
//! of `byard-core` for rendering, not for windowing. A concrete host (e.g.
//! `byard-platform`'s `WinitHost`) owns the actual window and event loop,
//! and calls into a [`PlatformHost`] implementation in response to OS events:
//!
//! ```text
//!   Host (WinitHost / etc.)                     Application code
//!   ──────────────────────                      ──────────────────────────────
//!   window + surface created  ──────────────►   PlatformHost::on_resume
//!   resize / DPI change       ──────────────►   PlatformHost::on_resize
//!   RedrawRequested            ──────────────►   PlatformHost::on_redraw
//!   mouse button press/release ──────────────►   PlatformHost::on_pointer_input
//!   close button / Cmd+Q       ──────────────►   PlatformHost::on_close_requested
//! ```
//!
//! The application implements [`PlatformHost`] once and it works unchanged
//! against any host. The host crate is the only place `winit` (or whatever
//! the next backend is) appears in the dependency tree.

use crate::ByardError;

/// Physical window dimensions plus the OS DPI scale factor.
///
/// This is the minimal data every [`PlatformHost`] callback needs about the
/// window, expressed without depending on any windowing crate's types.
/// `width`/`height` are in **physical pixels** (e.g. winit's
/// `window.inner_size()`); `scale_factor` is the OS-reported DPI scale (e.g.
/// winit's `window.scale_factor()`), see [`crate::engine`]'s module docs for
/// why the engine needs both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowSize {
    /// Surface width in physical pixels.
    pub width: u32,
    /// Surface height in physical pixels.
    pub height: u32,
    /// OS DPI scale factor (`1.0` on non-`HiDPI`, `2.0` on Retina, etc.).
    pub scale_factor: f64,
    /// The display's refresh rate in millihertz, when the host knows it.
    ///
    /// This is the frame budget (RFC-0030 §Q3, §V2). It is adaptive rather
    /// than a fixed 16.7 ms on purpose: the budget answers "will this app drop
    /// frames on *this* machine", and on a 120 Hz panel that threshold is
    /// 8.3 ms. A tool that drew its bars against 16.7 ms there would report a
    /// comfortable frame for one that visibly stutters, lying on exactly the
    /// hardware where the frame budget matters most.
    ///
    /// `None` when the platform cannot report it, in which case the consumer
    /// falls back to 60 Hz and says so.
    pub refresh_rate_mhz: Option<u32>,
}

/// Keyboard modifier state accompanying a key or pointer event.
///
/// Carried on every [`InputEvent`] (RFC-0041 §2) so a text field can tell a
/// plain Left from Shift+Left or Ctrl+Left, and also offered to the host by
/// itself through [`PlatformHost::on_chord`], before the router sees the
/// event, so the dev runner can claim its `Mod+Shift` chords (RFC-0030 §Q1).
///
/// The `key_down` payload a `byld` handler sees is still the key name alone:
/// modifiers reach the engine's own widgets, not the language, which is the
/// reason RFC-0030 kept them off the event and still holds.
// Four `bool`s is the shape of the thing being modelled, a keyboard has four
// modifier keys and they are independent. A bitflags type would satisfy the
// lint and make every call site less readable than `mods.shift`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt / Option.
    pub alt: bool,
    /// Command / Super / Windows.
    pub meta: bool,
}

impl KeyModifiers {
    /// Whether the platform's "Mod" key is held: `Cmd` on macOS, `Ctrl`
    /// everywhere else.
    ///
    /// A chord rather than a bare function key because bare F-keys are
    /// unusable as a default, macOS maps them to media controls unless the
    /// user flipped a preference, several are claimed by Linux window
    /// managers, and, critically, a bare key *can* collide with text entry in
    /// the app under test while a `Mod+Shift` chord cannot (§Q1).
    #[must_use]
    pub const fn mod_held(self) -> bool {
        if cfg!(target_os = "macos") {
            self.meta
        } else {
            self.ctrl
        }
    }

    /// Whether the modifier that moves the caret by a word is held: `Option`
    /// on macOS, `Ctrl` everywhere else (RFC-0041 §Q5).
    ///
    /// Not the same key as [`mod_held`](Self::mod_held) on macOS, where `Cmd`
    /// is the shortcut key and `Option` is the word key.
    #[must_use]
    pub const fn word_held(self) -> bool {
        if cfg!(target_os = "macos") {
            self.alt
        } else {
            self.ctrl
        }
    }

    /// Reads a key chord as a person writes it on a command line: modifiers
    /// then a key, joined by `+`, like `shift+Left` or `mod+a`, and returns the
    /// modifiers and the key's name as a platform reports it (`ArrowLeft`).
    ///
    /// The modifiers are `shift`, `ctrl`, `alt` (or `option`), `meta` (or
    /// `cmd`, `super`) and `mod`, the platform's shortcut key (RFC-0041 §Q5).
    /// The key is a single character, or one of `Left`, `Right`, `Up`, `Down`,
    /// `Home`, `End`, `Backspace`, `Delete`, `Enter`, `Tab`, `Escape` and
    /// `Space`; case does not matter for those.
    ///
    /// # Errors
    ///
    /// An unknown modifier or key, in words a person can act on.
    pub fn parse_chord(chord: &str) -> Result<(Self, String), String> {
        let (head, key) = match chord.rfind('+') {
            // "mod++" is Mod and the plus key; a lone "+" is the plus key.
            Some(i) if i + 1 == chord.len() => (chord[..i].strip_suffix('+').unwrap_or(""), "+"),
            Some(i) => (&chord[..i], &chord[i + 1..]),
            None => ("", chord),
        };
        let mut mods = Self::default();
        for part in head.split('+').filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "shift" => mods.shift = true,
                "ctrl" | "control" => mods.ctrl = true,
                "alt" | "option" => mods.alt = true,
                "meta" | "cmd" | "command" | "super" | "win" => mods.meta = true,
                "mod" => {
                    if cfg!(target_os = "macos") {
                        mods.meta = true;
                    } else {
                        mods.ctrl = true;
                    }
                }
                other => {
                    return Err(format!(
                        "`{other}` is not a modifier; use shift, ctrl, alt, meta or mod"
                    ));
                }
            }
        }
        let name = match key.to_ascii_lowercase().as_str() {
            "left" => "ArrowLeft".to_string(),
            "right" => "ArrowRight".to_string(),
            "up" => "ArrowUp".to_string(),
            "down" => "ArrowDown".to_string(),
            "home" => "Home".to_string(),
            "end" => "End".to_string(),
            "backspace" => "Backspace".to_string(),
            "delete" | "del" => "Delete".to_string(),
            "enter" | "return" => "Enter".to_string(),
            "tab" => "Tab".to_string(),
            "escape" | "esc" => "Escape".to_string(),
            "space" => " ".to_string(),
            _ if key.chars().count() == 1 => key.to_string(),
            "" => return Err(format!("`{chord}` names no key")),
            _ => {
                return Err(format!(
                    "`{key}` is not a key I know; try Left, Home, Delete or a single character"
                ));
            }
        };
        Ok((mods, name))
    }

    /// Whether this is the `Mod+Shift` prefix every dev chord uses.
    #[must_use]
    pub const fn is_dev_chord(self) -> bool {
        self.mod_held() && self.shift
    }
}

/// A mouse/pointer button, expressed without depending on any windowing
/// crate's types, mirrors the variants `winit::event::MouseButton` exposes
/// today, since that is the only host that currently exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    /// The primary (usually left) button.
    Left,
    /// The secondary (usually right) button.
    Right,
    /// The middle/wheel button.
    Middle,
    /// A "navigate back" side button, where the device has one.
    Back,
    /// A "navigate forward" side button, where the device has one.
    Forward,
    /// Any other, vendor-specific button, identified by its raw code.
    Other(u16),
}

/// A pointer button's transition state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerState {
    /// The button was just pressed down.
    Pressed,
    /// The button was just released.
    Released,
}

/// The event kinds the Phase-2 router models.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventKind {
    /// A pointer press.
    PointerDown,
    /// A pointer release.
    PointerUp,
    /// A qualifying tap (down+up within thresholds).
    Tap,
    /// Continuous pointer movement.
    PointerMove,
    /// Continuous scroll.
    Scroll,
    /// Continuous wheel.
    Wheel,
    /// A value change from a value-carrying intrinsic.
    Change,
    /// A keyboard key press; key name in `InputEvent.payload` as `InputPayload::Key`.
    KeyDown,
    /// A keyboard key release; key name in `InputEvent.payload` as `InputPayload::Key`.
    KeyUp,
    /// Printable text input (character key or IME commit); text in `InputEvent.payload` as `InputPayload::Key`.
    TextInput,
    // ── Remaining event catalog ─────────────────────────────────────
    /// Cursor entered an element's hit rect (synthesized by the router).
    PointerEnter,
    /// Cursor left an element's hit rect (synthesized by the router).
    PointerExit,
    /// Pointer hovering inside an element (fires on `PointerMove` while inside, no button).
    Hover,
    /// Press held > 500 ms without significant movement.
    LongPress,
    /// Two qualifying taps within `DOUBLE_TAP_MS` ms.
    DoubleTap,
    /// Secondary (right) button tap.
    Secondary,
    /// The IME's preedit changed (RFC-0040); the preedit and its cursor in
    /// `InputEvent.payload` as `InputPayload::Preedit`. An empty preedit
    /// clears it.
    Composition,
    /// The IME committed text (RFC-0040); the text in `InputEvent.payload` as
    /// `InputPayload::Key`. Delivered to the field that owned the
    /// composition, which is not always the one that has focus now.
    CompositionCommit,
    /// The platform turned the IME off (RFC-0040): any preedit is gone and
    /// no commit is coming for it.
    CompositionEnd,
    /// The system's appearance is light or dark, reported by the host when
    /// the window opens and whenever the user changes it; `true` in
    /// `InputEvent.payload` as `InputPayload::Bool` means dark.
    SystemScheme,
    /// An assistive technology asked to move keyboard focus to an element
    /// (RFC-0042 §5); the element in `InputEvent.payload` as
    /// `InputPayload::Element`. Not routed to a handler: the router moves
    /// focus itself, as a Tab press would.
    AccessFocus,
    /// An assistive technology asked to scroll an element into view
    /// (RFC-0042 §5); the element in `InputEvent.payload` as
    /// `InputPayload::Element`, and where it is in viewport space in
    /// `InputEvent.pos` (its top-left) and `InputEvent.delta` (its size).
    AccessScrollIntoView,
    /// The window gained or lost focus, reported by the host; `true` in
    /// `InputEvent.payload` as `InputPayload::Bool` means it has focus. A
    /// text field's caret only blinks in a window that has it (RFC-0041 §6).
    WindowFocus,
}

impl EventKind {
    /// Whether this is a continuous (coalescible) event.
    #[must_use]
    pub fn is_continuous(self) -> bool {
        matches!(self, Self::PointerMove | Self::Scroll | Self::Wheel)
    }

    /// Whether this event happens at a place, so `InputEvent.pos` is where
    /// it happened and hit testing may route it.
    ///
    /// Keyboard, text and value-change events have no position: hosts send
    /// them with `pos: (0.0, 0.0)`, and they go to the focused element. Routing
    /// one by rect would hand it to whatever sits at the window's corner.
    #[must_use]
    pub fn is_positional(self) -> bool {
        match self {
            Self::PointerDown
            | Self::PointerUp
            | Self::Tap
            | Self::PointerMove
            | Self::Scroll
            | Self::Wheel
            | Self::PointerEnter
            | Self::PointerExit
            | Self::Hover
            | Self::LongPress
            | Self::DoubleTap
            | Self::Secondary => true,
            Self::Change
            | Self::KeyDown
            | Self::KeyUp
            | Self::TextInput
            | Self::Composition
            | Self::CompositionCommit
            | Self::CompositionEnd
            | Self::SystemScheme
            | Self::AccessFocus
            | Self::AccessScrollIntoView
            | Self::WindowFocus => false,
        }
    }
}

/// A simple payload for input events.
#[derive(Clone, Debug, PartialEq)]
pub enum InputPayload {
    /// A string payload (e.g. text input value).
    Str(String),
    /// A boolean payload (e.g. toggle state).
    Bool(bool),
    /// A float payload (e.g. slider position).
    Float(f32),
    /// A number at full precision: the value an assistive technology set a
    /// slider to (RFC-0042), which an `f32` would turn from `0.7` into
    /// `0.699999988…`.
    Number(f64),
    /// An element, by its index in the lowered tree (RFC-0042 `AccessId`).
    Element(u32),
    /// A key name or printable text (keyboard events).
    Key(String),
    /// An IME preedit and its cursor, a byte range inside it (RFC-0040).
    Preedit {
        /// The text being composed.
        text: String,
        /// The IME's cursor; `None` hides it.
        cursor: Option<(usize, usize)>,
    },
}

/// An input-method event, independent of any windowing library (RFC-0040
/// §3). A host maps its platform's IME events onto this and hands them to
/// [`PlatformHost::on_ime`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImeEvent {
    /// The platform turned the IME on for this window.
    Enabled,
    /// The preedit is now `text`, with the IME cursor at `cursor` (bytes).
    /// An empty `text` clears it.
    Preedit {
        /// The text being composed.
        text: String,
        /// The IME's cursor; `None` hides it.
        cursor: Option<(usize, usize)>,
    },
    /// Insert `text` into the value at the caret.
    Commit(String),
    /// The platform turned the IME off; any preedit is gone.
    Disabled,
}

impl ImeEvent {
    /// The engine input event this becomes, or `None` for one the engine
    /// has no use for (`Enabled`: the field is already focused, which is
    /// what asked for the IME).
    #[must_use]
    pub fn into_input(self, time_ms: u64) -> Option<InputEvent> {
        let (kind, payload) = match self {
            Self::Enabled => return None,
            Self::Preedit { text, cursor } => (
                EventKind::Composition,
                Some(InputPayload::Preedit { text, cursor }),
            ),
            Self::Commit(text) => (EventKind::CompositionCommit, Some(InputPayload::Key(text))),
            Self::Disabled => (EventKind::CompositionEnd, None),
        };
        Some(InputEvent {
            kind,
            pos: (0.0, 0.0),
            delta: (0.0, 0.0),
            payload,
            time_ms,
            mods: KeyModifiers::default(),
        })
    }
}

/// A normalized, `Send`-able input event produced by the platform thread.
#[derive(Clone, Debug, PartialEq)]
pub struct InputEvent {
    /// The event kind.
    pub kind: EventKind,
    /// Absolute cursor position (logical px).
    pub pos: (f32, f32),
    /// Incremental delta for continuous events.
    pub delta: (f32, f32),
    /// The new value for a `Change` event (write-back payload).
    pub payload: Option<InputPayload>,
    /// Event time in milliseconds (for the tap interval).
    pub time_ms: u64,
    /// The modifier keys held when the event happened (RFC-0041 §2). Default
    /// (none held) for events that have no keyboard to ask, such as a
    /// system-scheme change.
    pub mods: KeyModifiers,
}

/// Application hooks driven by a concrete platform host.
///
/// Implement this trait once per application; a host (e.g. `WinitHost` from
/// `byard-platform`) owns the window and event loop and calls these methods
/// in response to OS events. Neither side needs to know the other's concrete
/// type, which is what keeps `winit` (or any future backend) out of
/// `byard-core`'s dependency tree (RFC-0001 §6).
///
/// ## Scope: this trait drives [`Engine`](crate::engine::Engine)
///
/// `PlatformHost` owns an `Engine` and calls it in response to OS events.
/// [`on_resume`](PlatformHost::on_resume) initialises the engine and calls
/// [`Engine::start_logic`](crate::engine::Engine::start_logic) to spawn the
/// logic thread; [`on_redraw`](PlatformHost::on_redraw) calls
/// [`Engine::render_latest`](crate::engine::Engine::render_latest), which
/// reads the latest [`RenderFrame`](crate::frame::RenderFrame) from the
/// engine's [`Relay`](crate::relay::Relay), the logic thread never blocks
/// the render thread, per RFC-0001 §5.
pub trait PlatformHost {
    /// Called once, after the host has created its window and `wgpu`
    /// surface, before the first redraw. Implementations typically call
    /// [`Engine::init`](crate::engine::Engine::init) here (via
    /// `pollster::block_on` or similar, since this method is synchronous)
    /// and store the resulting `Engine`.
    ///
    /// `instance` is borrowed only for the duration of this call, adapter
    /// and device creation must happen before it returns; `surface` is
    /// moved in because the resulting `Engine` owns it for its lifetime.
    ///
    /// `waker` is a frame-waker tied to the host's event loop (see
    /// [`Engine::set_frame_waker`](crate::engine::Engine::set_frame_waker)).
    /// An event-driven (`Wait`-mode) host should install it on the `Engine` it
    /// creates here, `engine.set_frame_waker(waker)`, so input results are
    /// presented as soon as the logic thread publishes them. A
    /// continuously-redrawing (`Poll`) host may ignore it.
    ///
    /// # Errors
    ///
    /// Returns whatever [`ByardError`] engine initialisation produces (see
    /// [`Engine::init`](crate::engine::Engine::init)'s own error
    /// documentation). The host should treat this as fatal startup failure.
    fn on_resume(
        &mut self,
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        size: WindowSize,
        waker: crate::relay::FrameWaker,
    ) -> Result<(), ByardError>;

    /// Called whenever the window is resized or the OS DPI scale changes.
    ///
    /// Implementations typically forward this directly to
    /// [`Engine::on_resize`](crate::engine::Engine::on_resize).
    fn on_resize(&mut self, size: WindowSize);

    /// Called when the host requests a redraw (e.g. winit's
    /// `WindowEvent::RedrawRequested`).
    ///
    /// Implementations typically call
    /// [`Engine::render_latest`](crate::engine::Engine::render_latest) here.
    ///
    /// # Errors
    ///
    /// Returns whatever [`ByardError`] frame rendering produces. Transient
    /// surface loss is already handled internally and never reaches this
    /// method as an error, only unrecoverable surface errors do.
    fn on_redraw(&mut self) -> Result<(), ByardError>;

    /// Whether the application still has frame-driven work, the render-thread
    /// side of the RFC-0010 active-animation set (extended by RFC-0025's
    /// repeating animations).
    ///
    /// A continuously-redrawing (`Poll`) host asks this between iterations and
    /// spins **only while it is true**: once every animation has settled, and
    /// nothing else is in flight, the loop drops back to blocking on OS events,
    /// so a static scene costs exactly zero frames. Input and the frame waker
    /// both wake it again, so nothing is missed.
    ///
    /// Defaults to `true`, which is the historical always-spin behaviour: a host
    /// that cannot answer keeps redrawing rather than risking a stalled window.
    fn wants_frames(&self) -> bool {
        true
    }

    /// Called when the user requests the window close (close button,
    /// Cmd+Q, etc.). Returning `true` tells the host it is safe to exit its
    /// event loop; returning `false` keeps the window open (e.g. to show an
    /// unsaved-changes prompt in a future application).
    ///
    /// Defaults to always permitting the close, since most applications,
    /// including every example in this crate today, have nothing to guard.
    fn on_close_requested(&mut self) -> bool {
        true
    }

    /// Called when a pointer (mouse) button changes state over the window at coordinates (x, y).
    ///
    /// Defaults to a no-op.
    fn on_pointer_input(&mut self, _button: PointerButton, _state: PointerState, _x: f32, _y: f32) {
    }

    /// Called with the system's appearance (`true` ⇒ dark) when the window
    /// opens and whenever it changes.
    ///
    /// Defaults to a no-op.
    fn on_system_scheme(&mut self, _dark: bool) {}

    /// Called when the cursor moves to coordinates (x, y).
    ///
    /// Defaults to a no-op.
    fn on_cursor_moved(&mut self, _x: f32, _y: f32) {}

    /// Called when a keyboard key is pressed or released.
    ///
    /// `key` is the logical key name (e.g. `"a"`, `"Enter"`, `"Backspace"`,
    /// `"Tab"`). `pressed` is `true` for key-down, `false` for key-up.
    ///
    /// Defaults to a no-op.
    fn on_key(&mut self, _key: &str, _pressed: bool) {}

    /// Called before [`on_key`](Self::on_key) with the modifier state, so a
    /// host can claim a chord for itself.
    ///
    /// Returning `true` **consumes** the event: `on_key` is not called and the
    /// app under test never sees it. That is the whole point, a dev chord
    /// that reached the router would be indistinguishable from the user typing
    /// (RFC-0030 §V3).
    ///
    /// The default ignores every chord, so a host that does not opt in behaves
    /// exactly as before.
    fn on_chord(&mut self, _key: &str, _pressed: bool, _mods: KeyModifiers) -> bool {
        false
    }

    /// Called when the modifier keys change (Shift, Ctrl, Alt, Cmd/Super), so
    /// a host can stamp the state onto the key and pointer events it creates
    /// afterwards ([`InputEvent::mods`], RFC-0041 §2).
    ///
    /// Defaults to a no-op: a host that does not record them sends events with
    /// no modifiers held, and a text field then edits as it always did.
    fn on_modifiers(&mut self, _mods: KeyModifiers) {}

    /// Called when the window gains (`true`) or loses focus, so the engine can
    /// stop a text field's caret blinking in a window nobody is typing into
    /// (RFC-0041 §6). A host usually forwards it as an
    /// [`EventKind::WindowFocus`] input event.
    ///
    /// Defaults to a no-op.
    fn on_window_focus(&mut self, _focused: bool) {}

    /// Called when printable text is committed (character keys, IME commit).
    ///
    /// `text` is the committed string (usually one character but may be more
    /// for IME). Defaults to a no-op.
    fn on_text(&mut self, _text: &str) {}

    /// Called on an input-method event (RFC-0040): a preedit, a commit, or
    /// the IME turning on or off. A host usually forwards
    /// [`ImeEvent::into_input`] to its engine.
    ///
    /// Defaults to a no-op, so a host that does not opt in behaves exactly as
    /// before.
    fn on_ime(&mut self, _event: ImeEvent) {}

    /// The focused text field's caret this frame, in viewport space, or
    /// `None` when no text field has focus (RFC-0040 §4). The platform thread
    /// reads it after each redraw to turn the IME on or off and to place its
    /// candidate window. A host usually answers from
    /// [`Engine::text_input`](crate::engine::Engine::text_input).
    ///
    /// Defaults to `None`: a host that does not opt in never enables the IME.
    fn text_input(&self) -> Option<crate::frame::TextInputState> {
        None
    }

    /// Called with `true` when an assistive technology connects to the window
    /// and `false` when the last one goes (RFC-0042 §5). A host passes it on
    /// to its engine ([`Engine::set_access_active`](crate::engine::Engine::set_access_active)),
    /// which is what makes the logic thread start, or stop, building the
    /// accessibility tree.
    ///
    /// Defaults to a no-op: a host that does not opt in never has a tree.
    fn on_access_active(&mut self, _active: bool) {}

    /// The latest published frame, from which the platform reads the
    /// accessibility tree to send to the assistive technology (RFC-0042 §5).
    /// A host usually answers from
    /// [`Engine::latest_frame`](crate::engine::Engine::latest_frame).
    ///
    /// Defaults to `None`: a host that does not opt in has no tree to send.
    fn access_frame(&self) -> Option<std::sync::Arc<crate::frame::RenderFrame>> {
        None
    }

    /// Called when an assistive technology asks a node to act (RFC-0042 §5).
    /// A host usually passes it on to
    /// [`Engine::push_access_request`](crate::engine::Engine::push_access_request).
    ///
    /// Defaults to a no-op.
    fn on_access_request(&mut self, _request: crate::access::AccessRequest) {}

    /// Called on a trackpad-style scroll gesture at cursor position `(x, y)`,
    /// with `(dx, dy)` the scroll delta in logical pixels (RFC-0012 §A loose
    /// end: `scroll`'s hardware origin).
    ///
    /// Defaults to a no-op.
    fn on_scroll(&mut self, _dx: f32, _dy: f32, _x: f32, _y: f32) {}

    /// Called on a physical mouse-wheel tick at cursor position `(x, y)`,
    /// with `(dx, dy)` the wheel delta (RFC-0012 §A loose end: `wheel`'s
    /// hardware origin).
    ///
    /// Defaults to a no-op.
    fn on_wheel(&mut self, _dx: f32, _dy: f32, _x: f32, _y: f32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `PlatformHost` that exercises only the trait's default
    /// methods, so the default `on_close_requested` body is covered without
    /// needing a real `wgpu`/window environment.
    struct DefaultsOnlyHost;

    impl PlatformHost for DefaultsOnlyHost {
        fn on_resume(
            &mut self,
            _instance: &wgpu::Instance,
            _surface: wgpu::Surface<'static>,
            _size: WindowSize,
            _waker: crate::relay::FrameWaker,
        ) -> Result<(), ByardError> {
            Ok(())
        }

        fn on_resize(&mut self, _size: WindowSize) {}

        fn on_redraw(&mut self) -> Result<(), ByardError> {
            Ok(())
        }
    }

    #[test]
    fn on_close_requested_defaults_to_true() {
        let mut host = DefaultsOnlyHost;
        assert!(host.on_close_requested());
    }

    #[test]
    fn on_pointer_input_default_is_a_no_op() {
        let mut host = DefaultsOnlyHost;
        // Must not panic for any button/state combination.
        host.on_pointer_input(PointerButton::Left, PointerState::Pressed, 0.0, 0.0);
        host.on_pointer_input(PointerButton::Other(7), PointerState::Released, 10.0, 20.0);
        host.on_cursor_moved(15.0, 25.0);
    }

    #[test]
    fn on_scroll_and_on_wheel_defaults_are_no_ops() {
        let mut host = DefaultsOnlyHost;
        // Must not panic for any delta/position combination.
        host.on_scroll(1.0, -2.0, 10.0, 20.0);
        host.on_wheel(0.0, 3.0, 30.0, 40.0);
    }

    #[test]
    fn pointer_button_and_state_are_copy_clone_eq_and_debug() {
        let a = PointerButton::Left;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(PointerButton::Left, PointerButton::Right);
        assert_eq!(PointerButton::Other(3), PointerButton::Other(3));
        assert_ne!(PointerButton::Other(3), PointerButton::Other(4));
        let _ = format!("{a:?}");

        let pressed = PointerState::Pressed;
        assert_ne!(pressed, PointerState::Released);
        let _ = format!("{pressed:?}");
    }

    #[test]
    fn a_dev_chord_is_mod_plus_shift_and_mod_is_platform_specific() {
        // The chord exists so it cannot collide with text entry in the app
        // under test, which a bare function key can.
        let mods = KeyModifiers {
            shift: true,
            meta: cfg!(target_os = "macos"),
            ctrl: !cfg!(target_os = "macos"),
            alt: false,
        };
        assert!(mods.is_dev_chord());
        assert!(
            !KeyModifiers {
                shift: false,
                ..mods
            }
            .is_dev_chord()
        );
        assert!(!KeyModifiers::default().is_dev_chord());
    }

    #[test]
    fn a_chord_is_modifiers_then_a_key_and_names_the_key_as_a_platform_does() {
        let (mods, key) = KeyModifiers::parse_chord("shift+Left").unwrap();
        assert_eq!(
            (mods, key.as_str()),
            (
                KeyModifiers {
                    shift: true,
                    ..KeyModifiers::default()
                },
                "ArrowLeft"
            )
        );
        let (mods, key) = KeyModifiers::parse_chord("mod+a").unwrap();
        assert!(mods.mod_held() && !mods.shift);
        assert_eq!(key, "a");
        let (mods, key) = KeyModifiers::parse_chord("ctrl+Shift+DELETE").unwrap();
        assert!(mods.ctrl && mods.shift && !mods.alt);
        assert_eq!(key, "Delete");
        let (mods, key) = KeyModifiers::parse_chord("Home").unwrap();
        assert_eq!((mods, key.as_str()), (KeyModifiers::default(), "Home"));
        assert_eq!(KeyModifiers::parse_chord("mod++").unwrap().1, "+");
        assert_eq!(KeyModifiers::parse_chord("+").unwrap().1, "+");
        assert_eq!(KeyModifiers::parse_chord("space").unwrap().1, " ");
    }

    #[test]
    fn a_chord_that_names_nothing_it_knows_is_refused_with_the_word_that_failed() {
        let err = KeyModifiers::parse_chord("hyper+a").unwrap_err();
        assert!(err.contains("hyper"), "{err}");
        let err = KeyModifiers::parse_chord("shift+Flibber").unwrap_err();
        assert!(err.contains("Flibber"), "{err}");
        assert!(KeyModifiers::parse_chord("").is_err());
    }

    #[test]
    fn window_size_is_copy_clone_eq_and_debug() {
        let a = WindowSize {
            width: 800,
            height: 600,
            scale_factor: 2.0,
            refresh_rate_mhz: Some(120_000),
        };
        let b = a; // Copy
        assert_eq!(a, b);

        let c = WindowSize { width: 801, ..a };
        assert_ne!(a, c);

        // Debug must not panic.
        let _ = format!("{a:?}");
    }
}
