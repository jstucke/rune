//! A portal dialog's `parent_window` (XDG desktop portal, "Window identifiers"): `x11:<hex
//! xid>`, or on Wayland `wayland:<handle>` from an xdg-foreign export of the window's toplevel,
//! which lives as long as the dialog does. Empty (no parent) when neither works.
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

pub(crate) struct ParentWindow {
    handle: String,
    _export: Option<wayland::Export>,
}

impl ParentWindow {
    /// `window` as a portal dialog's parent, exported now on Wayland.
    pub(crate) fn of(window: &winit::window::Window) -> Self {
        let none = || Self { handle: String::new(), _export: None };
        let Ok(handle) = window.window_handle() else { return none() };
        match handle.as_raw() {
            RawWindowHandle::Xlib(w) => Self { handle: x11(u32::try_from(w.window).unwrap_or(0)), _export: None },
            RawWindowHandle::Xcb(w) => Self { handle: x11(w.window.get()), _export: None },
            RawWindowHandle::Wayland(w) => {
                let Ok(display) = window.display_handle() else { return none() };
                let RawDisplayHandle::Wayland(d) = display.as_raw() else { return none() };
                match wayland::export(d.display.as_ptr(), w.surface.as_ptr()) {
                    Some((name, export)) => Self { handle: format!("wayland:{name}"), _export: Some(export) },
                    None => none(),
                }
            }
            _ => none(),
        }
    }

    pub(crate) fn handle(&self) -> &str { &self.handle }
}

/// `x11:<hex xid>`, or empty for no window.
fn x11(xid: u32) -> String { if xid == 0 { String::new() } else { format!("x11:{xid:x}") } }

mod wayland {
    use std::ffi::c_void;
    use wayland_client::backend::{Backend, ObjectId};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_registry::WlRegistry, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
    use wayland_protocols::xdg::foreign::zv2::client::zxdg_exported_v2::{self, ZxdgExportedV2};
    use wayland_protocols::xdg::foreign::zv2::client::zxdg_exporter_v2::ZxdgExporterV2;

    /// The exported toplevel; dropping it withdraws the handle.
    pub(super) struct Export {
        exported: ZxdgExportedV2,
        conn: Connection,
        _queue: EventQueue<State>,
    }

    impl Drop for Export {
        fn drop(&mut self) {
            self.exported.destroy();
            self.conn.flush().ok();
        }
    }

    #[derive(Default)]
    pub(super) struct State { handle: Option<String> }

    impl Dispatch<WlRegistry, GlobalListContents> for State {
        fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<ZxdgExporterV2, ()> for State {
        fn event(_: &mut Self, _: &ZxdgExporterV2, _: <ZxdgExporterV2 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<ZxdgExportedV2, ()> for State {
        fn event(state: &mut Self, _: &ZxdgExportedV2, event: zxdg_exported_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let zxdg_exported_v2::Event::Handle { handle } = event { state.handle = Some(handle); }
        }
    }

    /// Exports `surface` (winit's toplevel on `display`) on a queue of its own, waiting one round
    /// trip for the handle. None when the compositor has no `zxdg_exporter_v2`.
    #[allow(unsafe_code)]
    pub(super) fn export(display: *mut c_void, surface: *mut c_void) -> Option<(String, Export)> {
        // SAFETY: winit's `wl_display` lives as long as its event loop, past every window and dialog.
        let conn = Connection::from_backend(unsafe { Backend::from_foreign_display(display.cast()) });
        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        let exporter: ZxdgExporterV2 = globals.bind(&qh, 1..=1, ()).ok()?;
        // SAFETY: `surface` is the window's live `wl_surface`, which winit keeps while the window is.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }.ok()?;
        let surface = WlSurface::from_id(&conn, id).ok()?;
        let exported = exporter.export_toplevel(&surface, &qh, ());
        exporter.destroy();
        let mut state = State::default();
        queue.roundtrip(&mut state).ok();
        let Some(handle) = state.handle else {
            exported.destroy();
            conn.flush().ok();
            return None;
        };
        Some((handle, Export { exported, conn, _queue: queue }))
    }
}

#[cfg(test)]
#[path = "parent_tests.rs"]
mod tests;
