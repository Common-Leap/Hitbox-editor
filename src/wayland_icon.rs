//! Native Wayland toplevel icon support for the stable winit used by eframe.
//!
//! winit 0.30 deliberately ignores per-window icons on Wayland. Compositors that implement
//! `xdg-toplevel-icon-v1` can still receive the embedded icon through this small bridge. The
//! existing application id remains the fallback for compositors without that protocol.

use std::io::Write;
use std::os::fd::AsFd;

use anyhow::{Context as _, Result};
use wayland_client::{
    backend::{Backend, ObjectId},
    globals::{registry_queue_init, BindError, GlobalListContents},
    protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::xdg::{
    shell::client::xdg_toplevel::XdgToplevel,
    toplevel_icon::v1::client::{
        xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1,
        xdg_toplevel_icon_v1::XdgToplevelIconV1,
    },
};
use winit::{
    platform::wayland::WindowExtWayland,
    raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle},
    window::Window,
};

/// Resources the compositor is allowed to read for the lifetime of the window icon.
pub(crate) struct WaylandIcon {
    _connection: Connection,
    _event_queue: EventQueue<IconState>,
    _storage: std::fs::File,
    _pool: wl_shm_pool::WlShmPool,
    _buffer: wl_buffer::WlBuffer,
}

#[derive(Default)]
struct IconState;

impl WaylandIcon {
    /// Attach `icon` to a Wayland toplevel. `Ok(None)` means either X11 is active or the
    /// compositor does not advertise the optional icon protocol.
    pub(crate) fn attach(window: &Window, icon: &egui::IconData) -> Result<Option<Self>> {
        let Some(toplevel_ptr) = window.xdg_toplevel() else {
            return Ok(None);
        };

        let RawDisplayHandle::Wayland(display) = window
            .display_handle()
            .context("getting the Wayland display handle")?
            .as_raw()
        else {
            return Ok(None);
        };
        let RawWindowHandle::Wayland(surface) = window
            .window_handle()
            .context("getting the Wayland surface handle")?
            .as_raw()
        else {
            return Ok(None);
        };

        // SAFETY: all three pointers come from the live winit Window borrowed above. The
        // resulting guest Backend is stored in `Self`, so it cannot outlive that Window while
        // the app exists, and it never takes ownership of winit's wl_display.
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let (globals, event_queue) =
            registry_queue_init::<IconState>(&connection).context("reading Wayland globals")?;
        let queue_handle = event_queue.handle();

        let manager: XdgToplevelIconManagerV1 = match globals.bind(&queue_handle, 1..=1, ()) {
            Ok(manager) => manager,
            Err(BindError::NotPresent) => return Ok(None),
            Err(error) => return Err(error).context("binding xdg-toplevel-icon-v1"),
        };
        let shm: wl_shm::WlShm = globals
            .bind(&queue_handle, 1..=1, ())
            .context("binding wl_shm")?;

        // SAFETY: these are live wl_proxy pointers supplied by the same winit Window and the
        // interfaces are fixed by winit/raw-window-handle. The wrappers only borrow the objects;
        // this bridge never sends their destructor requests.
        let toplevel_id =
            unsafe { ObjectId::from_ptr(XdgToplevel::interface(), toplevel_ptr.as_ptr().cast()) }
                .context("wrapping winit's xdg_toplevel")?;
        let toplevel = XdgToplevel::from_id(&connection, toplevel_id)
            .context("creating the xdg_toplevel proxy")?;
        let surface_id = unsafe {
            ObjectId::from_ptr(
                wl_surface::WlSurface::interface(),
                surface.surface.as_ptr().cast(),
            )
        }
        .context("wrapping winit's wl_surface")?;
        let surface = wl_surface::WlSurface::from_id(&connection, surface_id)
            .context("creating the wl_surface proxy")?;

        let width = i32::try_from(icon.width).context("icon width exceeds Wayland limits")?;
        let height = i32::try_from(icon.height).context("icon height exceeds Wayland limits")?;
        if width <= 0 || width != height {
            anyhow::bail!("Wayland toplevel icons must be non-empty and square");
        }
        let stride = width.checked_mul(4).context("icon stride overflow")?;
        let byte_len = stride.checked_mul(height).context("icon size overflow")?;
        let pixels = premultiplied_argb8888(icon)?;

        let mut storage = tempfile::tempfile().context("creating Wayland icon storage")?;
        storage
            .write_all(&pixels)
            .context("writing Wayland icon pixels")?;
        storage.flush().context("flushing Wayland icon pixels")?;

        let pool = shm.create_pool(storage.as_fd(), byte_len, &queue_handle, ());
        let buffer = pool.create_buffer(
            0,
            width,
            height,
            stride,
            wl_shm::Format::Argb8888,
            &queue_handle,
            (),
        );
        let protocol_icon = manager.create_icon(&queue_handle, ());
        protocol_icon.add_buffer(&buffer, 1);
        manager.set_icon(&toplevel, Some(&protocol_icon));

        // Icon assignment is double-buffered with the toplevel's wl_surface state.
        surface.commit();
        protocol_icon.destroy();
        manager.destroy();
        connection.flush().context("sending the Wayland icon")?;

        Ok(Some(Self {
            _connection: connection,
            _event_queue: event_queue,
            _storage: storage,
            _pool: pool,
            _buffer: buffer,
        }))
    }
}

fn premultiplied_argb8888(icon: &egui::IconData) -> Result<Vec<u8>> {
    let pixel_count = usize::try_from(icon.width)
        .ok()
        .and_then(|width| {
            usize::try_from(icon.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .context("icon dimensions overflow")?;
    let expected_len = pixel_count.checked_mul(4).context("icon size overflow")?;
    if icon.rgba.len() != expected_len {
        anyhow::bail!(
            "icon has {} RGBA bytes, expected {expected_len}",
            icon.rgba.len()
        );
    }

    let mut argb = Vec::with_capacity(expected_len);
    for rgba in icon.rgba.chunks_exact(4) {
        let alpha = u32::from(rgba[3]);
        let red = u32::from(rgba[0]) * alpha / 255;
        let green = u32::from(rgba[1]) * alpha / 255;
        let blue = u32::from(rgba[2]) * alpha / 255;
        let color = (alpha << 24) | (red << 16) | (green << 8) | blue;
        argb.extend_from_slice(&color.to_le_bytes());
    }
    Ok(argb)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for IconState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
    }
}

wayland_client::delegate_noop!(IconState: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(IconState: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(IconState: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(IconState: ignore XdgToplevelIconManagerV1);
wayland_client::delegate_noop!(IconState: ignore XdgToplevelIconV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_rgba_to_premultiplied_argb8888() {
        let icon = egui::IconData {
            rgba: vec![255, 64, 16, 128],
            width: 1,
            height: 1,
        };

        let bytes = premultiplied_argb8888(&icon).unwrap();
        assert_eq!(u32::from_le_bytes(bytes.try_into().unwrap()), 0x8080_2008);
    }

    #[test]
    fn rejects_an_invalid_rgba_buffer_length() {
        let icon = egui::IconData {
            rgba: vec![0; 3],
            width: 1,
            height: 1,
        };

        assert!(premultiplied_argb8888(&icon).is_err());
    }
}
