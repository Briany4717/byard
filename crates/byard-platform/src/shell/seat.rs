//! The pointer, the keyboard and touch of the seats, as the engine takes them
//! (RFC-0068 §6).
//!
//! Positional events arrive from smithay-client-toolkit already in the
//! surface's logical coordinates, so they go to the host as they are, with
//! the surface they happened on. The keyboard is handled here: libxkbcommon
//! through [`xkb`](super::xkb), focus from `wl_keyboard.enter`, and key repeat
//! as a calloop timer, since a compositor leaves repeating to the client.

use std::time::Duration;

use byard_core::{KeyModifiers, PointerState, SurfaceId};
use smithay_client_toolkit::reexports::calloop::RegistrationToken;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::client::protocol::wl_keyboard::{
    self, KeyState, KeymapFormat, WlKeyboard,
};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{Proxy, WEnum};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind};
use smithay_client_toolkit::seat::Capability;

use super::{Shell, keys, xkb};

/// Where a pointer that left a surface is told it is, so nothing is under it:
/// the engine has no "left" event of its own, and an element that was hovered
/// has to stop being.
const OFF_SURFACE: f32 = -1.0e6;

/// One seat and the devices taken from it.
struct Seat {
    proxy: WlSeat,
    pointer: Option<Pointer>,
    keyboard: Option<Keyboard>,
    touch: Option<WlTouch>,
}

struct Pointer {
    proxy: WlPointer,
    shape: Option<WpCursorShapeDeviceV1>,
}

struct Keyboard {
    proxy: WlKeyboard,
    keymap: Option<xkb::Keymap>,
    repeat: keys::Repeat,
    /// The key being held that repeats, and the timer that repeats it.
    held: Option<(u32, RegistrationToken)>,
}

/// Every seat's devices and what the program last heard from them.
#[derive(Default)]
pub(crate) struct Devices {
    seats: Vec<Seat>,
    /// The surface the pointer is over.
    pointer_focus: Option<SurfaceId>,
    /// The surface that has the keyboard.
    keyboard_focus: Option<SurfaceId>,
    modifiers: KeyModifiers,
    /// The finger that is acting as the pointer, and the surface it is on.
    touch: Option<(i32, SurfaceId, (f32, f32))>,
}

impl Devices {
    /// A surface went away: nothing points at it or types into it any more.
    pub(crate) fn surface_gone(&mut self, id: SurfaceId) {
        if self.pointer_focus == Some(id) {
            self.pointer_focus = None;
        }
        if self.keyboard_focus == Some(id) {
            self.keyboard_focus = None;
        }
        if self.touch.is_some_and(|(_, s, _)| s == id) {
            self.touch = None;
        }
    }

    fn seat_mut(&mut self, seat: &WlSeat) -> &mut Seat {
        if let Some(i) = self.seats.iter().position(|s| &s.proxy == seat) {
            return &mut self.seats[i];
        }
        self.seats.push(Seat {
            proxy: seat.clone(),
            pointer: None,
            keyboard: None,
            touch: None,
        });
        self.seats.last_mut().expect("just pushed")
    }

    fn keyboard_mut(&mut self, proxy: &WlKeyboard) -> Option<&mut Keyboard> {
        self.seats
            .iter_mut()
            .filter_map(|s| s.keyboard.as_mut())
            .find(|k| &k.proxy == proxy)
    }
}

impl Shell {
    // ── capabilities ────────────────────────────────────────────────────

    /// A seat gained a device.
    pub(crate) fn add_capability(&mut self, seat: &WlSeat, capability: Capability) {
        match capability {
            Capability::Pointer => {
                let Ok(proxy) = self.seats.get_pointer(&self.qh, seat) else {
                    return;
                };
                let shape = self
                    .cursor_shape
                    .as_ref()
                    .map(|m| m.get_shape_device(&proxy, &self.qh));
                self.seat.seat_mut(seat).pointer = Some(Pointer { proxy, shape });
            }
            Capability::Keyboard => {
                let proxy = seat.get_keyboard(&self.qh, ());
                self.seat.seat_mut(seat).keyboard = Some(Keyboard {
                    proxy,
                    keymap: None,
                    repeat: keys::Repeat::default(),
                    held: None,
                });
            }
            Capability::Touch => {
                if let Ok(touch) = self.seats.get_touch(&self.qh, seat) {
                    self.seat.seat_mut(seat).touch = Some(touch);
                }
            }
            _ => {}
        }
    }

    /// A seat lost a device (a keyboard unplugged).
    pub(crate) fn remove_capability(&mut self, seat: &WlSeat, capability: Capability) {
        let Some(s) = self.seat.seats.iter_mut().find(|s| &s.proxy == seat) else {
            return;
        };
        match capability {
            Capability::Pointer => {
                if let Some(p) = s.pointer.take() {
                    if p.proxy.version() >= 3 {
                        p.proxy.release();
                    }
                }
                self.seat.pointer_focus = None;
            }
            Capability::Keyboard => {
                let held = s.keyboard.take().and_then(|k| {
                    if k.proxy.version() >= 3 {
                        k.proxy.release();
                    }
                    k.held
                });
                if let Some((_, token)) = held {
                    self.loop_handle.remove(token);
                }
                self.lose_keyboard_focus();
            }
            Capability::Touch => {
                if let Some(t) = s.touch.take() {
                    if t.version() >= 3 {
                        t.release();
                    }
                }
                self.seat.touch = None;
            }
            _ => {}
        }
    }

    // ── pointer ─────────────────────────────────────────────────────────

    /// A frame of pointer events.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "surface coordinates are far inside f32's range"
    )]
    pub(crate) fn pointer_events(&mut self, events: &[PointerEvent]) {
        for event in events {
            let Some(id) = self.find(&event.surface) else {
                continue;
            };
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.seat.pointer_focus = Some(id);
                    self.set_default_cursor(serial);
                    self.host.on_cursor_moved(id, x, y);
                }
                PointerEventKind::Leave { .. } => {
                    if self.seat.pointer_focus == Some(id) {
                        self.seat.pointer_focus = None;
                    }
                    self.host.on_cursor_moved(id, OFF_SURFACE, OFF_SURFACE);
                }
                PointerEventKind::Motion { .. } => self.host.on_cursor_moved(id, x, y),
                PointerEventKind::Press { button, .. } => {
                    self.host.on_pointer_input(
                        id,
                        keys::pointer_button(button),
                        PointerState::Pressed,
                        x,
                        y,
                    );
                }
                PointerEventKind::Release { button, .. } => {
                    self.host.on_pointer_input(
                        id,
                        keys::pointer_button(button),
                        PointerState::Released,
                        x,
                        y,
                    );
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    let axis = |a: smithay_client_toolkit::seat::pointer::AxisScroll| keys::Axis {
                        absolute: a.absolute,
                        discrete: a.discrete,
                    };
                    match keys::scroll(axis(horizontal), axis(vertical)) {
                        Some(keys::Scroll::Wheel(dx, dy)) => self.host.on_wheel(id, dx, dy, x, y),
                        Some(keys::Scroll::Pixels(dx, dy)) => self.host.on_scroll(id, dx, dy, x, y),
                        None => {}
                    }
                }
            }
        }
    }

    /// The arrow, which a surface that sets no cursor would otherwise show as
    /// whatever the last client left.
    fn set_default_cursor(&self, serial: u32) {
        for s in &self.seat.seats {
            if let Some(shape) = s.pointer.as_ref().and_then(|p| p.shape.as_ref()) {
                shape.set_shape(serial, Shape::Default);
            }
        }
    }

    // ── touch ───────────────────────────────────────────────────────────

    /// A finger went down. The first one acts as the left button of a pointer
    /// at that place; the others are ignored.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "surface coordinates are far inside f32's range"
    )]
    pub(crate) fn touch_down(&mut self, surface: &WlSurface, finger: i32, position: (f64, f64)) {
        if self.seat.touch.is_some() {
            return;
        }
        let Some(id) = self.find(surface) else {
            return;
        };
        let at = (position.0 as f32, position.1 as f32);
        self.seat.touch = Some((finger, id, at));
        self.host.on_cursor_moved(id, at.0, at.1);
        self.host.on_pointer_input(
            id,
            byard_core::PointerButton::Left,
            PointerState::Pressed,
            at.0,
            at.1,
        );
    }

    /// The acting finger moved.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "surface coordinates are far inside f32's range"
    )]
    pub(crate) fn touch_motion(&mut self, finger: i32, position: (f64, f64)) {
        let Some((f, id, _)) = self.seat.touch else {
            return;
        };
        if f != finger {
            return;
        }
        let at = (position.0 as f32, position.1 as f32);
        self.seat.touch = Some((f, id, at));
        self.host.on_cursor_moved(id, at.0, at.1);
    }

    /// The acting finger came up, or the compositor took the touch away.
    pub(crate) fn touch_up(&mut self, finger: Option<i32>) {
        let Some((f, id, at)) = self.seat.touch else {
            return;
        };
        if finger.is_some_and(|v| v != f) {
            return;
        }
        self.seat.touch = None;
        self.host.on_pointer_input(
            id,
            byard_core::PointerButton::Left,
            PointerState::Released,
            at.0,
            at.1,
        );
        self.host.on_cursor_moved(id, OFF_SURFACE, OFF_SURFACE);
    }

    // ── keyboard ────────────────────────────────────────────────────────

    /// One event of a `wl_keyboard`.
    pub(crate) fn keyboard_event(&mut self, proxy: &WlKeyboard, event: wl_keyboard::Event) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if !matches!(format, WEnum::Value(KeymapFormat::XkbV1)) {
                    return;
                }
                let keymap = xkb::Keymap::from_fd(fd, size);
                if keymap.is_none() {
                    eprintln!(
                        "  the keyboard's keymap could not be read (is libxkbcommon installed?); \
                         panels will not see keys"
                    );
                }
                if let Some(k) = self.seat.keyboard_mut(proxy) {
                    k.keymap = keymap;
                }
            }
            wl_keyboard::Event::Enter { surface, .. } => {
                let Some(id) = self.find(&surface) else {
                    return;
                };
                self.seat.keyboard_focus = Some(id);
                #[cfg(feature = "a11y")]
                self.a11y.focus(id, true);
                self.wins.focus_changed(id, true);
                self.host.on_window_focus(id, true);
            }
            wl_keyboard::Event::Leave { surface, .. } => {
                if self.find(&surface) == self.seat.keyboard_focus {
                    self.lose_keyboard_focus();
                }
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                let code = keys::xkb_keycode(key);
                match state {
                    WEnum::Value(KeyState::Pressed) => self.key_pressed(proxy, code),
                    WEnum::Value(KeyState::Released) => self.key_released(proxy, code),
                    _ => {}
                }
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                let Some(k) = self.seat.keyboard_mut(proxy) else {
                    return;
                };
                let Some(map) = k.keymap.as_mut() else {
                    return;
                };
                map.update_mask(mods_depressed, mods_latched, mods_locked, group);
                let mods = map.modifiers();
                if mods != self.seat.modifiers {
                    self.seat.modifiers = mods;
                    // The host stamps them onto the events it creates next.
                    self.host.on_modifiers(mods);
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                if let Some(k) = self.seat.keyboard_mut(proxy) {
                    k.repeat = keys::Repeat {
                        rate: u32::try_from(rate).unwrap_or(0),
                        delay: u32::try_from(delay).unwrap_or(0),
                    };
                }
            }
            _ => {}
        }
    }

    /// The keyboard left its surface: nothing is typed into it, and a held key
    /// stops repeating.
    fn lose_keyboard_focus(&mut self) {
        let held: Vec<RegistrationToken> = self
            .seat
            .seats
            .iter_mut()
            .filter_map(|s| s.keyboard.as_mut())
            .filter_map(|k| k.held.take().map(|(_, t)| t))
            .collect();
        for token in held {
            self.loop_handle.remove(token);
        }
        if let Some(id) = self.seat.keyboard_focus.take() {
            #[cfg(feature = "a11y")]
            self.a11y.focus(id, false);
            self.wins.focus_changed(id, false);
            self.host.on_window_focus(id, false);
        }
    }

    fn key_pressed(&mut self, proxy: &WlKeyboard, code: u32) {
        let Some(focus) = self.seat.keyboard_focus else {
            return;
        };
        let Some(k) = self.seat.keyboard_mut(proxy) else {
            return;
        };
        let Some(info) = k.keymap.as_ref().map(|m| m.key(code)) else {
            return;
        };
        let first = k.repeat.first().filter(|_| info.repeats);
        // A new key press replaces the one that was repeating.
        let replaced = k.held.take();
        if let Some((_, token)) = replaced {
            self.loop_handle.remove(token);
        }
        self.deliver_key(focus, &info, true);
        if let Some(first) = first {
            let kb = proxy.clone();
            let timer = Timer::from_duration(first);
            if let Ok(token) = self
                .loop_handle
                .insert_source(timer, move |_, (), shell: &mut Shell| {
                    shell.repeat_tick(&kb)
                })
            {
                if let Some(k) = self.seat.keyboard_mut(proxy) {
                    k.held = Some((code, token));
                }
            }
        }
    }

    fn key_released(&mut self, proxy: &WlKeyboard, code: u32) {
        let Some(focus) = self.seat.keyboard_focus else {
            return;
        };
        let Some(k) = self.seat.keyboard_mut(proxy) else {
            return;
        };
        let Some(info) = k.keymap.as_ref().map(|m| m.key(code)) else {
            return;
        };
        if k.held.as_ref().is_some_and(|(c, _)| *c == code) {
            if let Some((_, token)) = k.held.take() {
                self.loop_handle.remove(token);
            }
        }
        self.deliver_key(focus, &info, false);
    }

    /// The timer of a held key fired: type it again, and say when next.
    fn repeat_tick(&mut self, proxy: &WlKeyboard) -> TimeoutAction {
        let Some(focus) = self.seat.keyboard_focus else {
            return TimeoutAction::Drop;
        };
        let Some(k) = self.seat.keyboard_mut(proxy) else {
            return TimeoutAction::Drop;
        };
        let (Some((code, _)), Some(interval)) = (k.held.as_ref(), k.repeat.interval()) else {
            return TimeoutAction::Drop;
        };
        let Some(info) = k.keymap.as_ref().map(|m| m.key(*code)) else {
            return TimeoutAction::Drop;
        };
        self.deliver_key(focus, &info, true);
        TimeoutAction::ToDuration(Duration::max(interval, Duration::from_millis(1)))
    }

    /// Hands a key to the host as the winit host does: the chord first, which
    /// a dev runner may claim; then the key; then the text it types.
    fn deliver_key(&mut self, surface: SurfaceId, info: &xkb::KeyInfo, pressed: bool) {
        let name = keys::key_name(info.keysym, &info.character);
        let mods = self.seat.modifiers;
        if !name.is_empty() && self.host.on_chord(surface, &name, pressed, mods) {
            return;
        }
        if !name.is_empty() {
            self.host.on_key(surface, &name, pressed);
        }
        if pressed {
            if let Some(text) = keys::typed_text(&info.text) {
                self.host.on_text(surface, text);
            }
        }
    }
}
