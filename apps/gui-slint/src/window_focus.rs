#[cfg(target_os = "linux")]
pub use linux::PopupFocusControl;

#[cfg(not(target_os = "linux"))]
pub use fallback::PopupFocusControl;

#[cfg(target_os = "linux")]
mod linux {
    use std::cell::Cell;

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::ComponentHandle;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, PropMode};
    use x11rb::rust_connection::RustConnection;
    use x11rb::wrapper::ConnectionExt as _;

    use crate::OcrPopup;

    /// Controls X11 window properties on Linux to prevent the OCR popup
    /// overlay from stealing keyboard or window manager focus from active
    /// applications/games when shown.
    pub struct PopupFocusControl {
        connection: Option<RustConnection>,
        window_id: Cell<Option<u32>>,
        user_time_atom: u32,
        type_atom: Option<u32>,
        tooltip_atom: Option<u32>,
        state_atom: Option<u32>,
        skip_taskbar_atom: Option<u32>,
        skip_pager_atom: Option<u32>,
        above_atom: Option<u32>,
        wm_protocols_atom: Option<u32>,
        wm_delete_window_atom: Option<u32>,
        configured: Cell<bool>,
    }

    impl PopupFocusControl {
        pub fn new(popup: &OcrPopup) -> Self {
            let Ok((conn, _)) = x11rb::connect(None) else {
                tracing::warn!("Could not connect to X11 to configure popup focus properties");
                return Self {
                    connection: None,
                    window_id: Cell::new(None),
                    user_time_atom: 0,
                    type_atom: None,
                    tooltip_atom: None,
                    state_atom: None,
                    skip_taskbar_atom: None,
                    skip_pager_atom: None,
                    above_atom: None,
                    wm_protocols_atom: None,
                    wm_delete_window_atom: None,
                    configured: Cell::new(false),
                };
            };

            let user_time_atom = conn
                .intern_atom(false, b"_NET_WM_USER_TIME")
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.atom)
                .unwrap_or(0);

            let type_atom = conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE")
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.atom);

            let tooltip_atom = conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_TOOLTIP")
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.atom);

            let state_atom = conn
                .intern_atom(false, b"_NET_WM_STATE")
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.atom);

            let skip_taskbar_atom = conn
                .intern_atom(false, b"_NET_WM_STATE_SKIP_TASKBAR")
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.atom);

            let skip_pager_atom = conn
                .intern_atom(false, b"_NET_WM_STATE_SKIP_PAGER")
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.atom);

            let above_atom = conn
                .intern_atom(false, b"_NET_WM_STATE_ABOVE")
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.atom);

            let wm_protocols_atom = conn
                .intern_atom(false, b"WM_PROTOCOLS")
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.atom);

            let wm_delete_window_atom = conn
                .intern_atom(false, b"WM_DELETE_WINDOW")
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.atom);

            let ctrl = Self {
                connection: Some(conn),
                window_id: Cell::new(None),
                user_time_atom,
                type_atom,
                tooltip_atom,
                state_atom,
                skip_taskbar_atom,
                skip_pager_atom,
                above_atom,
                wm_protocols_atom,
                wm_delete_window_atom,
                configured: Cell::new(false),
            };
            ctrl.ensure_configured(popup);
            ctrl
        }

        fn extract_window_id(popup: &OcrPopup) -> Option<u32> {
            let window_handle_binding = popup.window().window_handle();
            let handle_res = window_handle_binding.window_handle();
            match handle_res {
                Ok(handle) => match handle.as_raw() {
                    RawWindowHandle::Xlib(xlib) => {
                        tracing::info!(xlib.window, "Found Xlib raw window handle");
                        Some(xlib.window as u32)
                    }
                    RawWindowHandle::Xcb(xcb) => {
                        tracing::info!(
                            xcb.window = xcb.window.get(),
                            "Found Xcb raw window handle"
                        );
                        Some(xcb.window.get())
                    }
                    other => {
                        tracing::warn!(?other, "Unexpected raw window handle variant");
                        None
                    }
                },
                Err(err) => {
                    tracing::debug!(%err, "Window handle not yet available");
                    None
                }
            }
        }

        pub fn ensure_configured(&self, popup: &OcrPopup) {
            let conn = match &self.connection {
                Some(c) => c,
                None => return,
            };

            if self.window_id.get().is_none()
                && let Some(wid) = Self::extract_window_id(popup)
            {
                self.window_id.set(Some(wid));
            }

            let Some(wid) = self.window_id.get() else {
                return;
            };

            // 1. _NET_WM_WINDOW_TYPE = TOOLTIP
            if let (Some(type_atom), Some(tooltip_atom)) = (self.type_atom, self.tooltip_atom) {
                let _ = conn.change_property32(
                    PropMode::REPLACE,
                    wid,
                    type_atom,
                    AtomEnum::ATOM,
                    &[tooltip_atom],
                );
            }

            // 2. WM_HINTS input = false (flags = 1 [InputHint], input = 0 [false])
            // ICCCM standard: informs the window manager that this window rejects keyboard focus.
            let _ = conn.change_property32(
                PropMode::REPLACE,
                wid,
                AtomEnum::WM_HINTS,
                AtomEnum::WM_HINTS,
                &[1, 0, 0, 0, 0, 0, 0, 0, 0],
            );

            // 3. Remove WM_TAKE_FOCUS from WM_PROTOCOLS:
            // Standard ICCCM No-Input model ensures the WM never attempts to transfer focus.
            if let (Some(protocols_atom), Some(delete_atom)) =
                (self.wm_protocols_atom, self.wm_delete_window_atom)
            {
                let _ = conn.change_property32(
                    PropMode::REPLACE,
                    wid,
                    protocols_atom,
                    AtomEnum::ATOM,
                    &[delete_atom],
                );
            }

            // 4. _NET_WM_STATE = SKIP_TASKBAR, SKIP_PAGER, ABOVE
            if let (Some(state_atom), Some(skip_taskbar), Some(skip_pager), Some(above)) = (
                self.state_atom,
                self.skip_taskbar_atom,
                self.skip_pager_atom,
                self.above_atom,
            ) {
                let _ = conn.change_property32(
                    PropMode::REPLACE,
                    wid,
                    state_atom,
                    AtomEnum::ATOM,
                    &[skip_taskbar, skip_pager, above],
                );
            }

            // 5. override_redirect = 1: bypasses window manager management and focus handling entirely
            let _ = conn.change_window_attributes(
                wid,
                &x11rb::protocol::xproto::ChangeWindowAttributesAux::default().override_redirect(1),
            );

            // 6. _NET_WM_USER_TIME = 0: signals to window manager not to focus on map
            if self.user_time_atom != 0 {
                let _ = conn.change_property32(
                    PropMode::REPLACE,
                    wid,
                    self.user_time_atom,
                    AtomEnum::CARDINAL,
                    &[0],
                );
            }

            let _ = conn.flush();
            if !self.configured.get() {
                self.configured.set(true);
                tracing::info!(
                    wid,
                    "Configured popup window properties (override_redirect=1, tooltip, no-input) to prevent taking focus"
                );
            }
        }

        pub fn prepare_show(&self, popup: &OcrPopup) {
            self.ensure_configured(popup);

            if let (Some(conn), Some(wid)) = (&self.connection, self.window_id.get()) {
                let _ = conn.change_window_attributes(
                    wid,
                    &x11rb::protocol::xproto::ChangeWindowAttributesAux::default()
                        .override_redirect(1),
                );
                if self.user_time_atom != 0 {
                    let _ = conn.change_property32(
                        PropMode::REPLACE,
                        wid,
                        self.user_time_atom,
                        AtomEnum::CARDINAL,
                        &[0],
                    );
                }
                let _ = conn.configure_window(
                    wid,
                    &x11rb::protocol::xproto::ConfigureWindowAux::default()
                        .stack_mode(x11rb::protocol::xproto::StackMode::ABOVE),
                );
                let _ = conn.flush();
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod fallback {
    use crate::OcrPopup;

    pub struct PopupFocusControl;

    impl PopupFocusControl {
        pub fn new(_popup: &OcrPopup) -> Self {
            Self
        }

        pub fn prepare_show(&self, _popup: &OcrPopup) {}
    }
}
