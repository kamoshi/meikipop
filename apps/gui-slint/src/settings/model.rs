#[derive(Clone, Debug, PartialEq)]
pub struct AppSettings {
    pub ocr_provider: String,
    pub max_lookup_length: i32,
    pub auto_scan: bool,
    pub auto_scan_on_mouse_move: bool,
    pub auto_scan_cooldown_seconds: f32,
    pub show_popup_without_hotkey: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            ocr_provider: std::env::var("MEIKIPOP_OCR_PROVIDER")
                .unwrap_or_else(|_| meikipop_native::ocr::ocr::DEFAULT_PROVIDER_ID.to_owned()),
            max_lookup_length: 25,
            auto_scan: true,
            auto_scan_on_mouse_move: true,
            auto_scan_cooldown_seconds: 0.5,
            show_popup_without_hotkey: true,
        }
    }
}
