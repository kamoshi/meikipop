use std::cell::RefCell;
use std::rc::Rc;

use meikipop_native::pipeline::{Pipeline, PipelineRuntimeConfig};
use slint::ComponentHandle;

use crate::settings::{AppSettings, SettingsController};
use crate::{MeikiPopTray, SettingsWindow};

pub fn setup_settings(
    settings: &SettingsWindow,
    tray: slint::Weak<MeikiPopTray>,
    controller: Rc<SettingsController>,
    pipeline: Rc<RefCell<Pipeline>>,
    runtime_config: Rc<RefCell<PipelineRuntimeConfig>>,
) {
    load_draft(settings, &controller.current());
    let controller_for_save = Rc::clone(&controller);
    let pipeline_for_save = Rc::clone(&pipeline);
    let settings_weak = settings.as_weak();
    settings.on_save(move || {
        if let Some(settings) = settings_weak.upgrade() {
            let mut draft = controller_for_save.current();
            read_draft(&settings, &mut draft);
            match controller_for_save.apply(draft) {
                Ok(()) => {
                    let config = controller_for_save.pipeline_config();
                    match pipeline_for_save.borrow().update_config(config.clone()) {
                        Ok(()) => *runtime_config.borrow_mut() = config,
                        Err(error) => {
                            tracing::warn!(%error, "Could not apply OCR provider setting")
                        }
                    }
                    if let Some(tray) = tray.upgrade() {
                        tray.set_selected_scan_mode(
                            if controller_for_save.current().auto_scan {
                                "auto"
                            } else {
                                "hotkey"
                            }
                            .into(),
                        );
                    }
                    let _ = settings.hide();
                }
                Err(error) => tracing::warn!(%error, "Could not save settings"),
            }
        }
    });
    let controller_for_cancel = Rc::clone(&controller);
    let settings_weak = settings.as_weak();
    settings.on_cancel(move || {
        if let Some(settings) = settings_weak.upgrade() {
            load_draft(&settings, &controller_for_cancel.current());
            let _ = settings.hide();
        }
    });

    let settings_weak = settings.as_weak();
    settings.on_change_hotkey(move || {
        #[cfg(target_os = "linux")]
        crate::hotkey::linux::choose(settings_weak.clone());
        #[cfg(not(target_os = "linux"))]
        {
            let _ = &settings_weak;
            tracing::info!("GlobalShortcuts portal is only supported on Linux");
        }
    });

    let pipeline_for_hotkey = Rc::clone(&pipeline);
    settings.on_hotkey_held(move |held| {
        pipeline_for_hotkey.borrow().set_hotkey_held(held);
    });

    #[cfg(target_os = "linux")]
    crate::hotkey::linux::initialize(settings.as_weak());
}

pub fn load_draft(settings: &SettingsWindow, draft: &AppSettings) {
    settings.set_selected_provider(draft.ocr_provider.clone().into());
    settings.set_max_lookup_length(draft.max_lookup_length);
    settings.set_auto_scan(draft.auto_scan);
    settings.set_auto_scan_on_mouse_move(draft.auto_scan_on_mouse_move);
    settings.set_auto_scan_interval_milliseconds(
        (draft.auto_scan_cooldown_seconds * 1_000.0).round() as i32,
    );
    settings.set_show_popup_without_hotkey(draft.show_popup_without_hotkey);
}

fn read_draft(settings: &SettingsWindow, draft: &mut AppSettings) {
    draft.ocr_provider = settings.get_selected_provider().to_string();
    draft.max_lookup_length = settings.get_max_lookup_length();
    draft.auto_scan = settings.get_auto_scan();
    draft.auto_scan_on_mouse_move = settings.get_auto_scan_on_mouse_move();
    draft.auto_scan_cooldown_seconds =
        settings.get_auto_scan_interval_milliseconds() as f32 / 1_000.0;
    draft.show_popup_without_hotkey = settings.get_show_popup_without_hotkey();
}
