use slint::Weak;

use crate::SettingsWindow;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// Platform-neutral bridge from a global-hotkey backend to the Slint UI.
#[derive(Clone)]
pub(crate) struct HotkeyEvents {
    settings: Weak<SettingsWindow>,
}

impl HotkeyEvents {
    fn new(settings: Weak<SettingsWindow>) -> Self {
        Self { settings }
    }

    pub(crate) fn binding_changed(&self, description: String) {
        let settings = self.settings.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(settings) = settings.upgrade() {
                settings.set_hotkey(description.into());
            }
        });
    }

    pub(crate) fn held_changed(&self, held: bool) {
        let settings = self.settings.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(settings) = settings.upgrade() {
                settings.invoke_hotkey_held(held);
            }
        });
    }

    pub(crate) fn show_info(&self, title: &'static str, message: &'static str) {
        let settings = self.settings.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(settings) = settings.upgrade() {
                settings.set_info_dialog_title(title.into());
                settings.set_info_dialog_message(message.into());
                settings.set_show_info_dialog(true);
            }
        });
    }
}

pub fn initialize(settings: Weak<SettingsWindow>) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    platform::initialize(HotkeyEvents::new(settings));

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = settings;
}

pub fn choose(settings: Weak<SettingsWindow>) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    platform::choose(HotkeyEvents::new(settings));

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = settings;
        tracing::info!("Global hotkeys are not supported on this platform");
    }
}

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
