//! What the seat says, in the words the engine uses: buttons, keys and scroll
//! (RFC-0068 §6).
//!
//! Pure functions, so they are tested without a compositor. The names they
//! produce are the ones the winit host produces for the same keys
//! (`key_to_str` in `lib.rs`): a program behaves the same whichever backend
//! runs it.

use byard_core::PointerButton;

// linux/input-event-codes.h: the buttons a mouse reports.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;
const BTN_FORWARD: u32 = 0x115;
const BTN_BACK: u32 = 0x116;

/// The pointer button an evdev code is. `BTN_SIDE` and `BTN_BACK` are both the
/// thumb button that goes back, and `BTN_EXTRA` and `BTN_FORWARD` the one that
/// goes forward, as a toolkit reads them.
pub(crate) fn pointer_button(code: u32) -> PointerButton {
    match code {
        BTN_LEFT => PointerButton::Left,
        BTN_RIGHT => PointerButton::Right,
        BTN_MIDDLE => PointerButton::Middle,
        BTN_SIDE | BTN_BACK => PointerButton::Back,
        BTN_EXTRA | BTN_FORWARD => PointerButton::Forward,
        other => PointerButton::Other(u16::try_from(other).unwrap_or(u16::MAX)),
    }
}

/// What a scroll event is to the engine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Scroll {
    /// A wheel's notches, `(dx, dy)` in lines.
    Wheel(f32, f32),
    /// A touchpad's continuous scroll, `(dx, dy)` in logical pixels.
    Pixels(f32, f32),
}

/// One axis of a `wl_pointer` frame: the pixels scrolled and the notches.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Axis {
    /// Pixels, in the surface's logical coordinates.
    pub absolute: f64,
    /// Notches of a wheel; zero for a touchpad.
    pub discrete: i32,
}

/// The scroll a `wl_pointer` axis frame is, or `None` when nothing moved.
///
/// A mouse reports pixels and notches together, and the notches win, as the
/// winit host has it. Wayland counts the other way round from the engine (a
/// positive axis is down, where the engine's positive delta moves the content
/// down), so both are negated.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "a scroll delta is a handful of pixels or notches"
)]
pub(crate) fn scroll(horizontal: Axis, vertical: Axis) -> Option<Scroll> {
    if horizontal.discrete != 0 || vertical.discrete != 0 {
        return Some(Scroll::Wheel(
            -(horizontal.discrete as f32),
            -(vertical.discrete as f32),
        ));
    }
    if horizontal.absolute == 0.0 && vertical.absolute == 0.0 {
        return None;
    }
    Some(Scroll::Pixels(
        -(horizontal.absolute as f32),
        -(vertical.absolute as f32),
    ))
}

// X11 keysyms (xkbcommon-keysyms.h) of the keys the engine names.
const KEY_BACKSPACE: u32 = 0xff08;
const KEY_TAB: u32 = 0xff09;
const KEY_RETURN: u32 = 0xff0d;
const KEY_ESCAPE: u32 = 0xff1b;
const KEY_HOME: u32 = 0xff50;
const KEY_LEFT: u32 = 0xff51;
const KEY_UP: u32 = 0xff52;
const KEY_RIGHT: u32 = 0xff53;
const KEY_DOWN: u32 = 0xff54;
const KEY_END: u32 = 0xff57;
const KEY_KP_ENTER: u32 = 0xff8d;
const KEY_ISO_LEFT_TAB: u32 = 0xfe20;
const KEY_DELETE: u32 = 0xffff;
const KEY_SPACE: u32 = 0x20;

/// The name the engine gives the key `keysym` is, or the text it types when
/// it is a character, or an empty string for a key the engine does not model
/// (so the caller skips it). Matches the winit host's `key_to_str`.
///
/// `character` is what the keysym is as text (`xkb_keysym_to_utf8`), already
/// empty for a key with no character.
pub(crate) fn key_name(keysym: u32, character: &str) -> String {
    match keysym {
        KEY_BACKSPACE => "Backspace",
        KEY_DELETE => "Delete",
        KEY_RETURN | KEY_KP_ENTER => "Enter",
        KEY_TAB | KEY_ISO_LEFT_TAB => "Tab",
        KEY_SPACE => " ",
        KEY_ESCAPE => "Escape",
        KEY_LEFT => "ArrowLeft",
        KEY_RIGHT => "ArrowRight",
        KEY_UP => "ArrowUp",
        KEY_DOWN => "ArrowDown",
        KEY_HOME => "Home",
        KEY_END => "End",
        _ => {
            return if character.chars().any(char::is_control) {
                String::new()
            } else {
                character.to_string()
            };
        }
    }
    .to_string()
}

/// The text a key press types: `None` for a key that types nothing and for
/// control characters, which are keys and not text (the winit host's
/// `typed_text`).
pub(crate) fn typed_text(text: &str) -> Option<&str> {
    Some(text).filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
}

/// The evdev keycode of a `wl_keyboard` key is xkb's minus 8: xkb reserves the
/// first eight.
pub(crate) const fn xkb_keycode(evdev: u32) -> u32 {
    evdev + 8
}

/// How a held key repeats: `rate` keys a second after `delay` milliseconds, as
/// `wl_keyboard.repeat_info` says. A rate of zero is no repeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Repeat {
    /// Keys per second; 0 turns repeating off.
    pub rate: u32,
    /// Milliseconds before the first repeat.
    pub delay: u32,
}

impl Default for Repeat {
    /// What a compositor that sends no `repeat_info` (version 1 to 3) is
    /// assumed to want: the common desktop default.
    fn default() -> Self {
        Self {
            rate: 25,
            delay: 600,
        }
    }
}

impl Repeat {
    /// The pause before the first repeat, or `None` when repeating is off.
    pub(crate) fn first(self) -> Option<std::time::Duration> {
        (self.rate > 0).then(|| std::time::Duration::from_millis(u64::from(self.delay)))
    }

    /// The pause between repeats.
    pub(crate) fn interval(self) -> Option<std::time::Duration> {
        (self.rate > 0).then(|| std::time::Duration::from_micros(1_000_000 / u64::from(self.rate)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_buttons_are_the_ones_a_toolkit_reads() {
        assert_eq!(pointer_button(0x110), PointerButton::Left);
        assert_eq!(pointer_button(0x111), PointerButton::Right);
        assert_eq!(pointer_button(0x112), PointerButton::Middle);
        assert_eq!(pointer_button(0x113), PointerButton::Back);
        assert_eq!(pointer_button(0x116), PointerButton::Back);
        assert_eq!(pointer_button(0x114), PointerButton::Forward);
        assert_eq!(pointer_button(0x115), PointerButton::Forward);
        assert_eq!(pointer_button(0x117), PointerButton::Other(0x117));
    }

    #[test]
    fn a_wheel_scrolls_in_lines_and_the_notches_win_over_the_pixels() {
        let wheel = Axis {
            absolute: 15.0,
            discrete: 1,
        };
        // Scrolling down on Wayland is positive; the engine's content moves
        // up, which is negative.
        assert_eq!(
            scroll(Axis::default(), wheel),
            Some(Scroll::Wheel(-0.0, -1.0))
        );
    }

    #[test]
    fn a_touchpad_scrolls_in_logical_pixels() {
        let pad = Axis {
            absolute: -7.5,
            discrete: 0,
        };
        let across = Axis {
            absolute: 2.0,
            discrete: 0,
        };
        assert_eq!(scroll(across, pad), Some(Scroll::Pixels(-2.0, 7.5)));
    }

    #[test]
    fn a_frame_with_no_motion_is_no_scroll() {
        assert_eq!(scroll(Axis::default(), Axis::default()), None);
    }

    #[test]
    fn keys_are_named_as_the_winit_host_names_them() {
        assert_eq!(key_name(KEY_BACKSPACE, ""), "Backspace");
        assert_eq!(key_name(KEY_DELETE, ""), "Delete");
        assert_eq!(key_name(KEY_RETURN, "\r"), "Enter");
        assert_eq!(key_name(KEY_KP_ENTER, "\r"), "Enter");
        assert_eq!(key_name(KEY_TAB, "\t"), "Tab");
        assert_eq!(key_name(KEY_ISO_LEFT_TAB, ""), "Tab");
        assert_eq!(key_name(KEY_SPACE, " "), " ");
        assert_eq!(key_name(KEY_ESCAPE, "\u{1b}"), "Escape");
        assert_eq!(key_name(KEY_LEFT, ""), "ArrowLeft");
        assert_eq!(key_name(KEY_RIGHT, ""), "ArrowRight");
        assert_eq!(key_name(KEY_UP, ""), "ArrowUp");
        assert_eq!(key_name(KEY_DOWN, ""), "ArrowDown");
        assert_eq!(key_name(KEY_HOME, ""), "Home");
        assert_eq!(key_name(KEY_END, ""), "End");
    }

    #[test]
    fn a_character_key_is_its_character_and_other_keys_are_nothing() {
        assert_eq!(key_name(0x61, "a"), "a");
        assert_eq!(key_name(0x41, "A"), "A");
        assert_eq!(key_name(0x00e9, "é"), "é");
        // Shift, F5, a key with no character: not modelled.
        assert_eq!(key_name(0xffe1, ""), "");
        assert_eq!(key_name(0xffc2, ""), "");
        // A control character is a key, not a name.
        assert_eq!(key_name(0x1, "\u{1}"), "");
    }

    #[test]
    fn control_characters_are_keys_and_not_typed_text() {
        assert_eq!(typed_text("a"), Some("a"));
        assert_eq!(typed_text("é"), Some("é"));
        assert_eq!(typed_text(""), None);
        assert_eq!(typed_text("\r"), None);
        assert_eq!(typed_text("\u{8}"), None);
    }

    #[test]
    fn xkb_counts_keycodes_eight_above_evdev() {
        assert_eq!(xkb_keycode(30), 38);
    }

    #[test]
    fn a_repeat_rate_of_zero_turns_repeating_off() {
        let r = Repeat {
            rate: 25,
            delay: 600,
        };
        assert_eq!(r.first(), Some(std::time::Duration::from_millis(600)));
        assert_eq!(r.interval(), Some(std::time::Duration::from_millis(40)));
        let off = Repeat {
            rate: 0,
            delay: 600,
        };
        assert_eq!(off.first(), None);
        assert_eq!(off.interval(), None);
        assert_eq!(Repeat::default(), r);
    }
}
