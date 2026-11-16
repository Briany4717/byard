//! The bookkeeping of a program of several windows (RFC-0068 §2, §5), apart
//! from the windowing library so it can be tested without opening one.
//!
//! [`Windows`] turns what the logic thread asks ([`SurfaceChange`]s) and what
//! the platform reports (a close request, focus, a published frame) into
//! actions against a [`Backend`]: create, configure, destroy, request a
//! redraw, tell the program a window was asked to close, exit. The winit host
//! implements `Backend` with real windows, the Wayland shell backend with
//! windows and layer surfaces (RFC-0068 §8); the tests implement it with a log.

use byard_core::surface::InputRegion;
use byard_core::{SurfaceChange, SurfaceId, SurfaceSpec};

/// What [`Windows`] does to the outside world.
pub(crate) trait Backend {
    /// Opens the window of `id`. An `Err` is fatal to the program.
    fn create(&mut self, id: SurfaceId, spec: &SurfaceSpec) -> Result<(), String>;
    /// The reactive attributes of an open window changed.
    fn configure(&mut self, id: SurfaceId, spec: &SurfaceSpec);
    /// Closes the window of `id`.
    fn destroy(&mut self, id: SurfaceId);
    /// Asks for the window of `id` to be drawn.
    fn request_redraw(&mut self, id: SurfaceId);
    /// Tells the program that the window of `id` was asked to close, which
    /// runs its `close =>`.
    fn send_close(&mut self, id: SurfaceId);
    /// Ends the event loop.
    fn exit(&mut self);
    /// The part of an open panel that takes the pointer changed (RFC-0068
    /// §6). A backend with no panels has nothing to do.
    fn input_region(&mut self, _id: SurfaceId, _region: &InputRegion) {}
}

struct Open {
    id: SurfaceId,
    spec: SurfaceSpec,
    /// The version of the frame last asked to be drawn for it.
    drawn: Option<u64>,
}

/// The windows that are open and the decisions that follow from them.
#[derive(Default)]
pub(crate) struct Windows {
    open: Vec<Open>,
    /// Every id that ever was open: an id is never made again.
    retired: Vec<SurfaceId>,
    /// Whether any window ever opened, so a program that has not opened its
    /// first (a `when` that is still false) is not taken for one that closed
    /// its last.
    ever_opened: bool,
    focused: Option<SurfaceId>,
    exiting: bool,
    /// Whether the program runs when it has no surface open: a shell has no
    /// last window (RFC-0068 §2, Q11), and a screen being unplugged takes its
    /// panels with it without ending the program.
    keeps_running: bool,
}

impl Windows {
    /// The bookkeeping of a program with panels: it ends when it is stopped,
    /// never because its last surface went (RFC-0068 Q11).
    #[cfg_attr(
        not(all(target_os = "linux", feature = "wayland-shell")),
        allow(dead_code, reason = "only the Wayland shell backend keeps panels")
    )]
    pub(crate) fn for_shell() -> Self {
        Self {
            keeps_running: true,
            ..Self::default()
        }
    }

    /// Applies the changes the logic thread queued, in order, as far as order
    /// matters: everything made or reconfigured first, everything destroyed
    /// after, so a `when` that swaps one window for another never has a moment
    /// with none (which would end the program). A window made and destroyed
    /// in the same batch is never opened.
    ///
    /// # Errors
    ///
    /// What the backend said when a window could not be made.
    pub(crate) fn apply<B: Backend>(
        &mut self,
        changes: &[SurfaceChange],
        backend: &mut B,
    ) -> Result<(), String> {
        let destroyed_here = |id: SurfaceId, changes: &[SurfaceChange]| {
            changes
                .iter()
                .any(|c| matches!(c, SurfaceChange::Destroy { id: d } if *d == id))
        };
        let mut created_and_gone = Vec::new();
        for change in changes {
            match change {
                SurfaceChange::Create { id, spec } => {
                    if self.open.iter().any(|o| o.id == *id) || self.retired.contains(id) {
                        continue;
                    }
                    self.retired.push(*id);
                    if destroyed_here(*id, changes) {
                        created_and_gone.push(*id);
                        continue;
                    }
                    backend.create(*id, spec)?;
                    self.ever_opened = true;
                    self.open.push(Open {
                        id: *id,
                        spec: spec.clone(),
                        drawn: None,
                    });
                    // Its first frame was published before it had a window.
                    backend.request_redraw(*id);
                }
                SurfaceChange::Configure { id, spec } => {
                    if let Some(o) = self.open.iter_mut().find(|o| o.id == *id) {
                        if o.spec != *spec {
                            o.spec = spec.clone();
                            backend.configure(*id, spec);
                        }
                    }
                }
                SurfaceChange::InputRegion { id, region } => {
                    if self.open.iter().any(|o| o.id == *id) {
                        backend.input_region(*id, region);
                    }
                }
                // Destroys are applied after every create and configure of
                // the batch, below.
                SurfaceChange::Destroy { .. } => {}
            }
        }
        for change in changes {
            let SurfaceChange::Destroy { id } = change else {
                continue;
            };
            if created_and_gone.contains(id) {
                continue;
            }
            if let Some(i) = self.open.iter().position(|o| o.id == *id) {
                self.open.remove(i);
                if self.focused == Some(*id) {
                    self.focused = None;
                }
                backend.destroy(*id);
            }
        }
        if self.ever_opened && self.open.is_empty() && !self.exiting && !self.keeps_running {
            // The last window closed and there is no panel (RFC-0068 §2).
            self.exiting = true;
            backend.exit();
        }
        Ok(())
    }

    /// The platform took the surface of `id` away by itself: the compositor
    /// closed a layer surface, which is what it does to the panels of a screen
    /// that was unplugged (RFC-0068 §3). The surface is torn down as a destroy
    /// would, and nothing ends: the program learns of the screen going from
    /// `screens`, and its `for` destroys the panel (a `Destroy` for an id that
    /// is already gone is ignored).
    #[cfg_attr(
        not(all(target_os = "linux", feature = "wayland-shell")),
        allow(
            dead_code,
            reason = "only the Wayland shell backend has layer surfaces"
        )
    )]
    pub(crate) fn platform_closed<B: Backend>(&mut self, id: SurfaceId, backend: &mut B) {
        let Some(i) = self.open.iter().position(|o| o.id == id) else {
            return;
        };
        self.open.remove(i);
        if self.focused == Some(id) {
            self.focused = None;
        }
        backend.destroy(id);
    }

    /// The platform asked to close the window of `id`: its `close =>` runs if
    /// it has one, and without one the program ends, as the one window of a
    /// program has always ended it.
    pub(crate) fn close_requested<B: Backend>(&mut self, id: SurfaceId, backend: &mut B) {
        let Some(open) = self.open.iter().find(|o| o.id == id) else {
            return;
        };
        if open.spec.handles_close {
            backend.send_close(id);
        } else if !self.exiting {
            self.exiting = true;
            backend.exit();
        }
    }

    /// The logic thread published frames: asks for a redraw of the windows
    /// whose latest frame is newer than the one last asked for, and of no
    /// other. `version` is the version of a surface's latest frame.
    pub(crate) fn frames_published<B: Backend>(
        &mut self,
        version: impl Fn(SurfaceId) -> Option<u64>,
        backend: &mut B,
    ) {
        for o in &mut self.open {
            let v = version(o.id);
            if v.is_some() && v != o.drawn {
                o.drawn = v;
                backend.request_redraw(o.id);
            }
        }
    }

    /// A window gained or lost the keyboard focus.
    pub(crate) fn focus_changed(&mut self, id: SurfaceId, focused: bool) {
        if focused {
            if self.open.iter().any(|o| o.id == id) {
                self.focused = Some(id);
            }
        } else if self.focused == Some(id) {
            self.focused = None;
        }
    }

    /// The window that has the keyboard, if one does.
    #[allow(dead_code, reason = "what the tests and a host that mirrors focus ask")]
    pub(crate) fn focused(&self) -> Option<SurfaceId> {
        self.focused
    }

    /// Whether the window of `id` is open.
    #[allow(dead_code, reason = "what the tests and a host that mirrors focus ask")]
    pub(crate) fn is_open(&self, id: SurfaceId) -> bool {
        self.open.iter().any(|o| o.id == id)
    }

    /// The ids of the open windows, in the order they opened.
    pub(crate) fn ids(&self) -> impl Iterator<Item = SurfaceId> + '_ {
        self.open.iter().map(|o| o.id)
    }

    /// The spec a window was last given.
    #[allow(dead_code, reason = "what the tests and a host that mirrors focus ask")]
    pub(crate) fn spec(&self, id: SurfaceId) -> Option<&SurfaceSpec> {
        self.open.iter().find(|o| o.id == id).map(|o| &o.spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq, Clone)]
    enum Act {
        Create(u32),
        Configure(u32),
        Destroy(u32),
        Redraw(u32),
        Close(u32),
        Exit,
        Region(u32, InputRegion),
    }

    #[derive(Default)]
    struct Log(Vec<Act>);

    impl Backend for Log {
        fn create(&mut self, id: SurfaceId, _: &SurfaceSpec) -> Result<(), String> {
            self.0.push(Act::Create(id.index()));
            Ok(())
        }
        fn configure(&mut self, id: SurfaceId, _: &SurfaceSpec) {
            self.0.push(Act::Configure(id.index()));
        }
        fn destroy(&mut self, id: SurfaceId) {
            self.0.push(Act::Destroy(id.index()));
        }
        fn request_redraw(&mut self, id: SurfaceId) {
            self.0.push(Act::Redraw(id.index()));
        }
        fn send_close(&mut self, id: SurfaceId) {
            self.0.push(Act::Close(id.index()));
        }
        fn exit(&mut self) {
            self.0.push(Act::Exit);
        }
        fn input_region(&mut self, id: SurfaceId, region: &InputRegion) {
            self.0.push(Act::Region(id.index(), region.clone()));
        }
    }

    fn id(n: u32) -> SurfaceId {
        SurfaceId::new(n)
    }
    fn spec(title: &str, handles_close: bool) -> SurfaceSpec {
        let mut s = SurfaceSpec::window(title);
        s.handles_close = handles_close;
        s
    }
    fn create(n: u32, close: bool) -> SurfaceChange {
        SurfaceChange::Create {
            id: id(n),
            spec: spec(&format!("w{n}"), close),
        }
    }
    fn take(log: &mut Log) -> Vec<Act> {
        std::mem::take(&mut log.0)
    }

    #[test]
    fn windows_are_made_in_order_and_asked_to_draw_their_first_frame() {
        let (mut w, mut log) = (Windows::default(), Log::default());
        w.apply(&[create(1, false), create(2, false)], &mut log)
            .unwrap();
        assert_eq!(
            take(&mut log),
            [
                Act::Create(1),
                Act::Redraw(1),
                Act::Create(2),
                Act::Redraw(2)
            ]
        );
        assert_eq!(w.ids().collect::<Vec<_>>(), [id(1), id(2)]);
    }

    #[test]
    fn a_configure_reaches_an_open_window_only_when_its_spec_changed() {
        let (mut w, mut log) = (Windows::default(), Log::default());
        w.apply(&[create(1, false)], &mut log).unwrap();
        take(&mut log);
        let same = SurfaceChange::Configure {
            id: id(1),
            spec: spec("w1", false),
        };
        let renamed = SurfaceChange::Configure {
            id: id(1),
            spec: spec("new", false),
        };
        let unknown = SurfaceChange::Configure {
            id: id(9),
            spec: spec("x", false),
        };
        w.apply(&[same, unknown, renamed], &mut log).unwrap();
        assert_eq!(take(&mut log), [Act::Configure(1)]);
        assert_eq!(w.spec(id(1)).unwrap().title, "new");
    }

    #[test]
    fn a_window_is_destroyed_and_its_id_is_never_made_again() {
        let (mut w, mut log) = (Windows::default(), Log::default());
        w.apply(&[create(1, false), create(2, false)], &mut log)
            .unwrap();
        take(&mut log);
        w.apply(&[SurfaceChange::Destroy { id: id(2) }], &mut log)
            .unwrap();
        assert_eq!(take(&mut log), [Act::Destroy(2)]);
        // The same id again, even once it is gone, opens nothing.
        w.apply(&[create(2, false)], &mut log).unwrap();
        assert_eq!(take(&mut log), []);
        w.apply(&[create(3, false)], &mut log).unwrap();
        assert_eq!(take(&mut log), [Act::Create(3), Act::Redraw(3)]);
    }

    #[test]
    fn a_batch_swapping_the_only_window_makes_the_new_one_before_destroying_the_old() {
        let (mut w, mut log) = (Windows::default(), Log::default());
        w.apply(&[create(1, false)], &mut log).unwrap();
        take(&mut log);
        w.apply(
            &[SurfaceChange::Destroy { id: id(1) }, create(2, false)],
            &mut log,
        )
        .unwrap();
        assert_eq!(
            take(&mut log),
            [Act::Create(2), Act::Redraw(2), Act::Destroy(1)],
            "no moment with no window, so no exit"
        );
    }

    #[test]
    fn a_window_made_and_destroyed_in_one_batch_is_never_opened() {
        let (mut w, mut log) = (Windows::default(), Log::default());
        w.apply(
            &[create(1, false), SurfaceChange::Destroy { id: id(1) }],
            &mut log,
        )
        .unwrap();
        assert_eq!(take(&mut log), []);
        assert!(!w.is_open(id(1)));
    }

    #[test]
    fn the_program_ends_when_its_last_window_is_destroyed_and_only_then() {
        let (mut w, mut log) = (Windows::default(), Log::default());
        // Nothing opened yet (a `when` still false): no exit.
        w.apply(&[], &mut log).unwrap();
        assert_eq!(take(&mut log), []);
        w.apply(&[create(1, false), create(2, false)], &mut log)
            .unwrap();
        take(&mut log);
        w.apply(&[SurfaceChange::Destroy { id: id(1) }], &mut log)
            .unwrap();
        assert_eq!(take(&mut log), [Act::Destroy(1)]);
        w.apply(&[SurfaceChange::Destroy { id: id(2) }], &mut log)
            .unwrap();
        assert_eq!(take(&mut log), [Act::Destroy(2), Act::Exit]);
        // Said once.
        w.apply(&[], &mut log).unwrap();
        assert_eq!(take(&mut log), []);
    }

    #[test]
    fn closing_a_window_with_a_close_handler_tells_the_program_and_keeps_it_open() {
        let (mut w, mut log) = (Windows::default(), Log::default());
        w.apply(&[create(1, true), create(2, false)], &mut log)
            .unwrap();
        take(&mut log);
        w.close_requested(id(1), &mut log);
        assert_eq!(take(&mut log), [Act::Close(1)]);
        assert!(w.is_open(id(1)), "the program decides, through its `when`");
    }

    #[test]
    fn closing_a_window_without_a_close_handler_ends_the_program() {
        let (mut w, mut log) = (Windows::default(), Log::default());
        w.apply(&[create(1, true), create(2, false)], &mut log)
            .unwrap();
        take(&mut log);
        w.close_requested(id(2), &mut log);
        assert_eq!(take(&mut log), [Act::Exit]);
        // A request for a window that is not there is nothing.
        w.close_requested(id(7), &mut log);
        assert_eq!(take(&mut log), []);
    }

    #[test]
    fn a_redraw_is_asked_only_for_the_windows_whose_frame_changed() {
        let (mut w, mut log) = (Windows::default(), Log::default());
        w.apply(&[create(1, false), create(2, false)], &mut log)
            .unwrap();
        take(&mut log);
        let versions = |v1: u64, v2: u64| {
            move |s: SurfaceId| match s.index() {
                1 => Some(v1),
                2 => Some(v2),
                _ => None,
            }
        };
        w.frames_published(versions(5, 6), &mut log);
        assert_eq!(take(&mut log), [Act::Redraw(1), Act::Redraw(2)]);
        // Nothing new: nothing asked.
        w.frames_published(versions(5, 6), &mut log);
        assert_eq!(take(&mut log), []);
        // Only the second moved.
        w.frames_published(versions(5, 9), &mut log);
        assert_eq!(take(&mut log), [Act::Redraw(2)]);
        // A window with no frame yet is not asked.
        w.frames_published(|_| None, &mut log);
        assert_eq!(take(&mut log), []);
    }

    #[test]
    fn focus_moves_between_windows_and_goes_with_a_destroyed_one() {
        let (mut w, mut log) = (Windows::default(), Log::default());
        w.apply(&[create(1, false), create(2, false)], &mut log)
            .unwrap();
        assert_eq!(w.focused(), None);
        w.focus_changed(id(1), true);
        assert_eq!(w.focused(), Some(id(1)));
        // The platform reports the old window losing it after, or before, the
        // new one gaining it.
        w.focus_changed(id(2), true);
        w.focus_changed(id(1), false);
        assert_eq!(
            w.focused(),
            Some(id(2)),
            "a late loss does not steal it back"
        );
        w.apply(&[SurfaceChange::Destroy { id: id(2) }], &mut log)
            .unwrap();
        assert_eq!(w.focused(), None);
        w.focus_changed(id(9), true);
        assert_eq!(w.focused(), None, "a window that is not open has no focus");
    }

    #[test]
    fn a_window_that_cannot_be_made_is_an_error() {
        struct Refuses;
        impl Backend for Refuses {
            fn create(&mut self, _: SurfaceId, _: &SurfaceSpec) -> Result<(), String> {
                Err("no display".to_string())
            }
            fn configure(&mut self, _: SurfaceId, _: &SurfaceSpec) {}
            fn destroy(&mut self, _: SurfaceId) {}
            fn request_redraw(&mut self, _: SurfaceId) {}
            fn send_close(&mut self, _: SurfaceId) {}
            fn exit(&mut self) {}
        }
        let mut w = Windows::default();
        assert_eq!(
            w.apply(&[create(1, false)], &mut Refuses),
            Err("no display".to_string())
        );
    }

    fn panel(n: u32) -> SurfaceChange {
        SurfaceChange::Create {
            id: id(n),
            spec: SurfaceSpec::panel(format!("bar{n}")),
        }
    }

    #[test]
    fn a_shell_does_not_end_when_its_last_surface_goes() {
        let (mut w, mut log) = (Windows::for_shell(), Log::default());
        w.apply(&[panel(1), create(2, true)], &mut log).unwrap();
        take(&mut log);
        // The window closes, then the panel: a shell has no last window.
        w.apply(
            &[
                SurfaceChange::Destroy { id: id(2) },
                SurfaceChange::Destroy { id: id(1) },
            ],
            &mut log,
        )
        .unwrap();
        assert_eq!(take(&mut log), [Act::Destroy(2), Act::Destroy(1)]);
        // The same batch on the bookkeeping of a program of windows ends it.
        let (mut plain, mut log) = (Windows::default(), Log::default());
        plain.apply(&[create(1, true)], &mut log).unwrap();
        take(&mut log);
        plain
            .apply(&[SurfaceChange::Destroy { id: id(1) }], &mut log)
            .unwrap();
        assert_eq!(take(&mut log), [Act::Destroy(1), Act::Exit]);
    }

    #[test]
    fn a_panel_the_compositor_closed_is_torn_down_once_and_ends_nothing() {
        let (mut w, mut log) = (Windows::for_shell(), Log::default());
        w.apply(&[panel(1), panel(2)], &mut log).unwrap();
        take(&mut log);
        w.platform_closed(id(1), &mut log);
        assert_eq!(take(&mut log), [Act::Destroy(1)]);
        assert!(!w.is_open(id(1)));
        assert!(w.is_open(id(2)));
        // Closed again, and then destroyed by the program's `for`: nothing
        // more happens, and the id is never made again.
        w.platform_closed(id(1), &mut log);
        w.apply(&[SurfaceChange::Destroy { id: id(1) }, panel(1)], &mut log)
            .unwrap();
        assert_eq!(take(&mut log), []);
    }

    #[test]
    fn an_input_region_reaches_an_open_panel_and_no_other() {
        let (mut w, mut log) = (Windows::for_shell(), Log::default());
        w.apply(&[panel(1)], &mut log).unwrap();
        take(&mut log);
        let region = InputRegion::Rects(vec![byard_core::surface::Rect::new(0, 0, 40, 40)]);
        w.apply(
            &[
                SurfaceChange::InputRegion {
                    id: id(1),
                    region: region.clone(),
                },
                SurfaceChange::InputRegion {
                    id: id(9),
                    region: InputRegion::None,
                },
            ],
            &mut log,
        )
        .unwrap();
        assert_eq!(take(&mut log), [Act::Region(1, region)]);
    }

    #[test]
    fn a_panel_is_asked_to_draw_when_its_frame_is_new_and_not_again() {
        let (mut w, mut log) = (Windows::for_shell(), Log::default());
        w.apply(&[panel(1), panel(2)], &mut log).unwrap();
        take(&mut log);
        // Only the first has a newer frame.
        w.frames_published(|i| if i == id(1) { Some(7) } else { Some(3) }, &mut log);
        let first = take(&mut log);
        assert_eq!(first, [Act::Redraw(1), Act::Redraw(2)]);
        w.frames_published(|i| if i == id(1) { Some(8) } else { Some(3) }, &mut log);
        assert_eq!(take(&mut log), [Act::Redraw(1)]);
    }
}
