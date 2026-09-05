use std::cell::RefCell;
use std::rc::Rc;

use meikipop_native::pipeline::{Pipeline, PipelineRuntimeConfig};
use slint::{ComponentHandle, Weak};

use crate::settings::SettingsController;
use crate::{MeikiPopTray, SettingsWindow};

pub fn setup_tray(
    tray: &MeikiPopTray,
    settings: Weak<SettingsWindow>,
    controller: Rc<SettingsController>,
    pipeline: Rc<RefCell<Pipeline>>,
    runtime_config: Rc<RefCell<PipelineRuntimeConfig>>,
) {
    tray.set_selected_scan_mode(if controller.current().auto_scan {
        "auto".into()
    } else {
        "hotkey".into()
    });
    let controller_for_settings = Rc::clone(&controller);
    tray.on_show_settings(move || {
        if let Some(settings) = settings.upgrade() {
            crate::setup_settings::load_draft(&settings, &controller_for_settings.current());
            let _ = settings.show();
        }
    });

    let tray_weak = tray.as_weak();
    tray.on_choose_scan_mode(move |mode| {
        let mut updated = controller.current();
        updated.auto_scan = mode == "auto";
        match controller.apply(updated) {
            Ok(()) => {
                let config = controller.pipeline_config();
                match pipeline.borrow().update_config(config.clone()) {
                    Ok(()) => *runtime_config.borrow_mut() = config,
                    Err(error) => {
                        tracing::warn!(%error, "Could not apply scan mode");
                        return;
                    }
                }
                if let Some(tray) = tray_weak.upgrade() {
                    tray.set_selected_scan_mode(mode);
                }
            }
            Err(error) => tracing::warn!(%error, "Could not save scan mode"),
        }
    });

    tray.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
}
