//! The toolkit's handler traits and the protocol objects Byard dispatches
//! itself, for [`Shell`].
//!
//! Each of them only finds the surface an event is about and calls a method
//! of `Shell`, where the behaviour is. `delegate_*!` is how smithay-client-
//! toolkit is told which objects its states answer for.

use byard_core::SurfaceId;
use smithay_client_toolkit::compositor::CompositorHandler;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::protocol::wl_keyboard::WlKeyboard;
use smithay_client_toolkit::reexports::client::protocol::wl_output::{self, WlOutput};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerHandler};
use smithay_client_toolkit::seat::touch::TouchHandler;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_touch, delegate_xdg_shell, delegate_xdg_window, registry_handlers,
};

use super::{Role, Shell, layer};

delegate_compositor!(Shell);
delegate_output!(Shell);
delegate_seat!(Shell);
delegate_pointer!(Shell);
delegate_touch!(Shell);
delegate_layer!(Shell);
delegate_xdg_shell!(Shell);
delegate_xdg_window!(Shell);
delegate_registry!(Shell);

impl Shell {
    /// The surface that has `layer` as its role object.
    fn find_layer(&self, layer: &LayerSurface) -> Option<SurfaceId> {
        self.surfs
            .iter()
            .find(|(_, s)| matches!(&s.role, Role::Layer(l) if l == layer))
            .map(|(id, _)| id)
    }

    /// The surface that has `window` as its role object.
    fn find_window(&self, window: &Window) -> Option<SurfaceId> {
        self.surfs
            .iter()
            .find(|(_, s)| matches!(&s.role, Role::Window(w) if w.xdg_toplevel() == window.xdg_toplevel()))
            .map(|(id, _)| id)
    }
}

impl CompositorHandler for Shell {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &WlSurface,
        new_factor: i32,
    ) {
        // A surface with a fractional scale takes it from the compositor's
        // `preferred_scale`, which is always the more exact.
        if let Some(id) = self.find(surface) {
            if self.surfs.get(id).is_some_and(|s| s.fractional.is_none()) {
                self.set_scale(id, f64::from(new_factor));
            }
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        self.frame_done(surface);
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
}

impl OutputHandler for Shell {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        self.screens_dirty = true;
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        self.screens_dirty = true;
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        self.screens_dirty = true;
    }
}

impl SeatHandler for Shell {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        self.add_capability(&seat, capability);
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        Shell::remove_capability(self, &seat, capability);
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl PointerHandler for Shell {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlPointer,
        events: &[PointerEvent],
    ) {
        self.pointer_events(events);
    }
}

impl TouchHandler for Shell {
    fn down(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlTouch,
        _: u32,
        _: u32,
        surface: WlSurface,
        id: i32,
        position: (f64, f64),
    ) {
        self.touch_down(&surface, id, position);
    }

    fn up(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, _: u32, _: u32, id: i32) {
        self.touch_up(Some(id));
    }

    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlTouch,
        _: u32,
        id: i32,
        position: (f64, f64),
    ) {
        self.touch_motion(id, position);
    }

    fn shape(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlTouch,
        _: i32,
        _: f64,
        _: f64,
    ) {
    }

    fn orientation(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, _: i32, _: f64) {}

    fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch) {
        self.touch_up(None);
    }
}

impl LayerShellHandler for Shell {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        // The compositor took the panel away, as it does the panels of a
        // screen that was unplugged. The program learns of the screen from
        // `screens`, and its `for` drops the panel.
        if let Some(id) = self.find_layer(layer) {
            self.with_windows(|w, b| w.platform_closed(id, b));
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer_surface: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(id) = self.find_layer(layer_surface) else {
            return;
        };
        let asked = self
            .surfs
            .get(id)
            .map_or((0, 0), |s| layer::requested_size(s.spec.size));
        self.configured(id, layer::configured_size(configure.new_size, asked));
    }
}

impl WindowHandler for Shell {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window) {
        if let Some(id) = self.find_window(window) {
            self.with_windows(|w, b| w.close_requested(id, b));
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        window: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        let Some(id) = self.find_window(window) else {
            return;
        };
        let (current, opening) = self
            .surfs
            .get(id)
            .map_or(((0, 0), (0.0, 0.0)), |s| (s.logical, s.spec.size));
        let size = layer::window_size(
            (
                configure.new_size.0.map(std::num::NonZeroU32::get),
                configure.new_size.1.map(std::num::NonZeroU32::get),
            ),
            current,
            opening,
        );
        self.configured(id, size);
    }
}

impl ProvidesRegistryState for Shell {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState, SeatState];
}

// ── the protocols Byard dispatches itself ───────────────────────────────

impl Dispatch<WlKeyboard, ()> for Shell {
    fn event(
        state: &mut Self,
        proxy: &WlKeyboard,
        event: <WlKeyboard as smithay_client_toolkit::reexports::client::Proxy>::Event,
        _unit: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.keyboard_event(proxy, event);
    }
}

impl Dispatch<WpViewporter, ()> for Shell {
    fn event(
        _: &mut Self,
        _: &WpViewporter,
        _: <WpViewporter as smithay_client_toolkit::reexports::client::Proxy>::Event,
        _unit: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewport, ()> for Shell {
    fn event(
        _: &mut Self,
        _: &WpViewport,
        _: <WpViewport as smithay_client_toolkit::reexports::client::Proxy>::Event,
        _unit: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleManagerV1, ()> for Shell {
    fn event(
        _: &mut Self,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as smithay_client_toolkit::reexports::client::Proxy>::Event,
        _unit: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// The fractional scale of a surface, whose user data is the surface's id.
impl Dispatch<WpFractionalScaleV1, SurfaceId> for Shell {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        id: &SurfaceId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.set_scale(*id, layer::fractional_scale(scale));
        }
    }
}
