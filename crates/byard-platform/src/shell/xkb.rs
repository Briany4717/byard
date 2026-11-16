//! The compositor's keymap, through libxkbcommon (RFC-0068 §6).
//!
//! smithay-client-toolkit's own keyboard needs the `xkbcommon` crate, which
//! nothing else in the tree uses. winit loads libxkbcommon at run time through
//! `xkbcommon-dl`, which is already linked, so this does the same: a keymap
//! from the file descriptor `wl_keyboard.keymap` sends, a state that follows
//! `wl_keyboard.modifiers`, and what a key is in that state.
//!
//! This is the one place in the shell backend that calls C. Every call is on a
//! pointer this module made and frees, and no pointer leaves it.
#![allow(unsafe_code)]

use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;

use byard_core::KeyModifiers;
use xkbcommon_dl::{
    XKB_MOD_NAME_ALT, XKB_MOD_NAME_CTRL, XKB_MOD_NAME_LOGO, XKB_MOD_NAME_SHIFT, XkbCommon,
    xkb_context, xkb_context_flags, xkb_keymap, xkb_keymap_compile_flags, xkb_keymap_format,
    xkb_state, xkb_state_component, xkbcommon_option,
};

/// The most a keymap of the real world is: they are a few tens of kilobytes,
/// and a compositor that sends megabytes is not to be read.
const MAX_KEYMAP_BYTES: usize = 4 << 20;

/// What a key is, in the state the modifiers are in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyInfo {
    /// The keysym: what the key means with the modifiers held.
    pub keysym: u32,
    /// The text pressing it types, which is a control character with Ctrl held.
    pub text: String,
    /// The keysym as a character, with no modifier changing it: the name of a
    /// key, so Ctrl+A is the key `a`.
    pub character: String,
    /// Whether holding the key repeats it.
    pub repeats: bool,
}

/// A keymap and the modifier state that goes with it.
pub(crate) struct Keymap {
    lib: &'static XkbCommon,
    context: *mut xkb_context,
    compiled: *mut xkb_keymap,
    state: *mut xkb_state,
}

impl Keymap {
    /// Compiles the keymap a `wl_keyboard.keymap` event carried (`size` bytes
    /// at the start of `fd`). `None` if libxkbcommon is not installed or the
    /// keymap does not compile.
    pub(crate) fn from_fd(fd: OwnedFd, size: u32) -> Option<Self> {
        let lib = xkbcommon_option()?;
        let size = usize::try_from(size)
            .ok()
            .filter(|s| *s <= MAX_KEYMAP_BYTES)?;
        let mut bytes = vec![0_u8; size];
        // A positioned read: the descriptor's own position is the sender's.
        std::fs::File::from(fd).read_exact_at(&mut bytes, 0).ok()?;
        // The protocol terminates the text with a NUL, which the buffer form
        // does not want counted.
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        // SAFETY: `lib` is the loaded library, the context is made here and
        // freed in `drop`, and `bytes` is a valid buffer of the length given.
        unsafe {
            let context = (lib.xkb_context_new)(xkb_context_flags::XKB_CONTEXT_NO_FLAGS);
            if context.is_null() {
                return None;
            }
            let keymap = (lib.xkb_keymap_new_from_buffer)(
                context,
                bytes.as_ptr().cast(),
                bytes.len(),
                xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
                xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            );
            if keymap.is_null() {
                (lib.xkb_context_unref)(context);
                return None;
            }
            let state = (lib.xkb_state_new)(keymap);
            if state.is_null() {
                (lib.xkb_keymap_unref)(keymap);
                (lib.xkb_context_unref)(context);
                return None;
            }
            Some(Self {
                lib,
                context,
                compiled: keymap,
                state,
            })
        }
    }

    /// Follows `wl_keyboard.modifiers`.
    pub(crate) fn update_mask(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        // SAFETY: `state` is live until `drop`.
        unsafe {
            (self.lib.xkb_state_update_mask)(self.state, depressed, latched, locked, 0, 0, group);
        }
    }

    /// The modifier keys held now.
    pub(crate) fn modifiers(&self) -> KeyModifiers {
        let active = |name: &[u8]| -> bool {
            // SAFETY: `state` is live, and the names are NUL-terminated
            // constants.
            unsafe {
                (self.lib.xkb_state_mod_name_is_active)(
                    self.state,
                    name.as_ptr().cast(),
                    xkb_state_component::XKB_STATE_MODS_EFFECTIVE,
                ) > 0
            }
        };
        KeyModifiers {
            shift: active(XKB_MOD_NAME_SHIFT),
            ctrl: active(XKB_MOD_NAME_CTRL),
            alt: active(XKB_MOD_NAME_ALT),
            meta: active(XKB_MOD_NAME_LOGO),
        }
    }

    /// What the key with xkb keycode `code` is now.
    pub(crate) fn key(&self, code: u32) -> KeyInfo {
        // SAFETY: `state` and `keymap` are live, and the buffers are as long
        // as the lengths passed.
        unsafe {
            let keysym = (self.lib.xkb_state_key_get_one_sym)(self.state, code);
            let text = {
                let mut buf = [0_u8; 32];
                let n = (self.lib.xkb_state_key_get_utf8)(
                    self.state,
                    code,
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                );
                let n = usize::try_from(n).unwrap_or(0).min(buf.len() - 1);
                String::from_utf8_lossy(&buf[..n]).into_owned()
            };
            let character = {
                let mut buf = [0_u8; 16];
                let n = (self.lib.xkb_keysym_to_utf8)(keysym, buf.as_mut_ptr().cast(), buf.len());
                // The count includes the terminating NUL; 0 and -1 are "no
                // character" and "too long for the buffer".
                let n = usize::try_from(n).unwrap_or(0).saturating_sub(1);
                String::from_utf8_lossy(&buf[..n.min(buf.len())]).into_owned()
            };
            let repeats = (self.lib.xkb_keymap_key_repeats)(self.compiled, code) > 0;
            KeyInfo {
                keysym,
                text,
                character,
                repeats,
            }
        }
    }
}

impl Drop for Keymap {
    fn drop(&mut self) {
        // SAFETY: each pointer was made by `from_fd` and is freed once, in the
        // reverse order it was made.
        unsafe {
            (self.lib.xkb_state_unref)(self.state);
            (self.lib.xkb_keymap_unref)(self.compiled);
            (self.lib.xkb_context_unref)(self.context);
        }
    }
}
