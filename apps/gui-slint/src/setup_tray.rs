use slint::{ComponentHandle, Weak};

use crate::{MeikiPopTray, SettingsWindow};

pub fn setup_tray(tray: &MeikiPopTray, settings: Weak<SettingsWindow>) {
    tray.on_show_settings(move || {
        if let Some(settings) = settings.upgrade() {
            let _ = settings.show();
        }
    });

    tray.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
}
