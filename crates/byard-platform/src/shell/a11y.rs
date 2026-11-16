//! The accessibility tree of each surface on the way to a screen reader
//! (RFC-0068 §8), through `accesskit_unix` directly: one adapter per surface,
//! each a window under the application's AT-SPI root, which is what
//! `accesskit_winit` does on Linux underneath.
//!
//! The adapter calls its handlers from its own thread, so they only post a
//! message to the event loop; everything that touches the host happens on the
//! main thread, in [`Shell::access_message`]. The mapping from the engine's
//! tree to AccessKit's, and the diff of one tree against the last sent, are
//! the ones the winit host uses (`crate::access`).

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate};
use byard_core::{PerSurface, PlatformHost, SurfaceId};
use smithay_client_toolkit::reexports::calloop::channel::Sender;

use crate::access::{TreeSender, WindowInfo, request};

use super::Shell;

/// What an adapter's thread tells the main thread.
pub(crate) enum Message {
    /// A screen reader connected and asked for the tree.
    Activated(SurfaceId),
    /// It asked a node to act.
    Action(SurfaceId, ActionRequest),
    /// It left.
    Deactivated(SurfaceId),
}

/// The adapter's three handlers: each posts a message.
struct Handlers {
    surface: SurfaceId,
    tx: Sender<Message>,
}

impl ActivationHandler for Handlers {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        // The tree is sent from the main thread, after the next frame.
        let _ = self.tx.send(Message::Activated(self.surface));
        None
    }
}

impl ActionHandler for Handlers {
    fn do_action(&mut self, request: ActionRequest) {
        let _ = self.tx.send(Message::Action(self.surface, request));
    }
}

impl DeactivationHandler for Handlers {
    fn deactivate_accessibility(&mut self) {
        let _ = self.tx.send(Message::Deactivated(self.surface));
    }
}

/// The adapter of one surface, and what it has been told.
pub(crate) struct Bridge {
    adapter: accesskit_unix::Adapter,
    sender: TreeSender,
    active: bool,
}

/// Every surface's bridge.
#[derive(Default)]
pub(crate) struct Bridges {
    bridges: PerSurface<Bridge>,
}

impl Bridges {
    /// Makes the adapter of `surface`.
    pub(crate) fn attach(&mut self, surface: SurfaceId, tx: &Sender<Message>) {
        let handlers = || Handlers {
            surface,
            tx: tx.clone(),
        };
        self.bridges.insert(
            surface,
            Bridge {
                adapter: accesskit_unix::Adapter::new(handlers(), handlers(), handlers()),
                sender: TreeSender::default(),
                active: false,
            },
        );
    }

    /// Drops the adapter of `surface`.
    pub(crate) fn detach(&mut self, surface: SurfaceId) {
        self.bridges.remove(surface);
    }

    /// The surface gained or lost the keyboard.
    pub(crate) fn focus(&mut self, surface: SurfaceId, focused: bool) {
        if let Some(b) = self.bridges.get_mut(surface) {
            b.adapter.update_window_focus_state(focused);
        }
    }

    /// Sends the host's latest tree of `surface` if it differs from the last
    /// one sent. Costs nothing while no assistive technology is connected.
    pub(crate) fn sync(&mut self, surface: SurfaceId, host: &dyn PlatformHost, info: &WindowInfo) {
        let Some(b) = self.bridges.get_mut(surface) else {
            return;
        };
        if !b.active {
            return;
        }
        let Some(frame) = host.access_frame(surface) else {
            return;
        };
        let Some(tree) = frame.access() else {
            return;
        };
        if let Some(update) = b.sender.update(tree, info) {
            b.adapter.update_if_active(|| update);
        }
    }

    fn any_active(&self) -> bool {
        self.bridges.iter().any(|(_, b)| b.active)
    }
}

impl Shell {
    /// What an adapter's thread said.
    pub(crate) fn access_message(&mut self, message: Message) {
        match message {
            Message::Activated(id) => {
                let was_active = self.a11y.any_active();
                if let Some(b) = self.a11y.bridges.get_mut(id) {
                    b.sender.reset();
                    b.active = true;
                }
                if !was_active {
                    self.host.on_access_active(true);
                }
                // The tree is in the next frame the logic thread publishes
                // (it builds one now that a reader is there); draw so it is
                // sent when it is.
                if let Some(s) = self.surfs.get_mut(id) {
                    s.want_draw = true;
                    self.queue(id);
                }
            }
            Message::Action(id, request_from_reader) => {
                if let Some(r) = request(&request_from_reader) {
                    self.host.on_access_request(id, r);
                }
            }
            Message::Deactivated(id) => {
                if let Some(b) = self.a11y.bridges.get_mut(id) {
                    b.active = false;
                    b.sender.reset();
                }
                if !self.a11y.any_active() {
                    self.host.on_access_active(false);
                }
            }
        }
    }

    /// Sends the trees of the surfaces just drawn.
    pub(crate) fn sync_access(&mut self, ids: &[SurfaceId]) {
        for &id in ids {
            let Some(s) = self.surfs.get(id) else {
                continue;
            };
            let info = WindowInfo {
                title: if s.spec.title.is_empty() {
                    self.title.clone()
                } else {
                    s.spec.title.clone()
                },
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a window is far inside f32's integers"
                )]
                size: (s.logical.0 as f32, s.logical.1 as f32),
                scale: s.scale,
            };
            self.a11y.sync(id, &*self.host, &info);
        }
    }
}
