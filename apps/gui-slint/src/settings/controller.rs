use std::cell::RefCell;

use meikipop_native::pipeline::PipelineRuntimeConfig;

use super::{AppSettings, SettingsStore};

pub struct SettingsController {
    applied: RefCell<AppSettings>,
    store: SettingsStore,
}

impl SettingsController {
    pub fn load() -> Self {
        let store = SettingsStore::new();
        let applied = store.load();
        Self {
            applied: RefCell::new(applied),
            store,
        }
    }

    pub fn current(&self) -> AppSettings {
        self.applied.borrow().clone()
    }

    pub fn apply(&self, settings: AppSettings) -> Result<(), String> {
        if settings.ocr_provider.trim().is_empty() {
            return Err("OCR provider must not be empty".into());
        }
        if !(5..=100).contains(&settings.max_lookup_length) {
            return Err("Max lookup length must be between 5 and 100".into());
        }
        if !settings.auto_scan_cooldown_seconds.is_finite()
            || !(0.1..=60.0).contains(&settings.auto_scan_cooldown_seconds)
        {
            return Err("Scan interval must be between 100 ms and 60 seconds".into());
        }
        self.store
            .save(&settings)
            .map_err(|error| format!("Could not save settings: {error}"))?;
        *self.applied.borrow_mut() = settings;
        Ok(())
    }

    pub fn pipeline_config(&self) -> PipelineRuntimeConfig {
        let settings = self.applied.borrow();
        PipelineRuntimeConfig {
            ocr_provider: settings.ocr_provider.clone(),
            max_lookup_length: settings.max_lookup_length as usize,
            auto_scan: settings.auto_scan,
            auto_scan_on_mouse_move: settings.auto_scan_on_mouse_move,
            auto_scan_cooldown: std::time::Duration::from_secs_f32(
                settings.auto_scan_cooldown_seconds,
            ),
            show_popup_without_hotkey: settings.show_popup_without_hotkey,
        }
    }
}
