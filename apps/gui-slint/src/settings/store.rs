use std::path::PathBuf;

use super::model::AppSettings;

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new() -> Self {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(std::env::temp_dir);
        Self {
            path: base.join("meikipop").join("settings.conf"),
        }
    }

    pub fn load(&self) -> AppSettings {
        let Ok(contents) = std::fs::read_to_string(&self.path) else {
            return AppSettings::default();
        };
        parse_settings(&contents)
    }

    pub fn save(&self, settings: &AppSettings) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            &self.path,
            format!(
                "ocr_provider={}\nmax_lookup_length={}\nauto_scan={}\nauto_scan_on_mouse_move={}\nauto_scan_cooldown_seconds={}\nshow_popup_without_hotkey={}\n",
                settings.ocr_provider,
                settings.max_lookup_length,
                settings.auto_scan,
                settings.auto_scan_on_mouse_move,
                settings.auto_scan_cooldown_seconds,
                settings.show_popup_without_hotkey,
            ),
        )
    }
}

fn parse_settings(contents: &str) -> AppSettings {
    let mut settings = AppSettings::default();
    for line in contents.lines().filter_map(|line| line.split_once('=')) {
        match line.0 {
            "ocr_provider" => settings.ocr_provider = line.1.to_owned(),
            "max_lookup_length" => {
                if let Ok(value @ 5..=100) = line.1.parse() {
                    settings.max_lookup_length = value;
                }
            }
            "auto_scan" => settings.auto_scan = line.1.parse().unwrap_or(settings.auto_scan),
            "auto_scan_on_mouse_move" => {
                settings.auto_scan_on_mouse_move =
                    line.1.parse().unwrap_or(settings.auto_scan_on_mouse_move)
            }
            "auto_scan_cooldown_seconds" => {
                if let Ok(value) = line.1.parse::<f32>()
                    && value.is_finite()
                    && (0.1..=60.0).contains(&value)
                {
                    settings.auto_scan_cooldown_seconds = value;
                }
            }
            "show_popup_without_hotkey" => {
                settings.show_popup_without_hotkey =
                    line.1.parse().unwrap_or(settings.show_popup_without_hotkey)
            }
            _ => {}
        }
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::parse_settings;

    #[test]
    fn invalid_numeric_values_keep_safe_defaults() {
        let settings = parse_settings(
            "max_lookup_length=1000\nauto_scan_cooldown_seconds=NaN\nauto_scan=false\n",
        );

        assert_eq!(settings.max_lookup_length, 25);
        assert_eq!(settings.auto_scan_cooldown_seconds, 0.5);
        assert!(!settings.auto_scan);
    }

    #[test]
    fn parses_supported_values_and_ignores_unknown_keys() {
        let settings = parse_settings(
            "ocr_provider=dummy\nmax_lookup_length=40\nauto_scan=false\nauto_scan_on_mouse_move=false\nauto_scan_cooldown_seconds=1.5\nshow_popup_without_hotkey=false\nfuture_setting=true\n",
        );

        assert_eq!(settings.ocr_provider, "dummy");
        assert_eq!(settings.max_lookup_length, 40);
        assert!(!settings.auto_scan);
        assert!(!settings.auto_scan_on_mouse_move);
        assert_eq!(settings.auto_scan_cooldown_seconds, 1.5);
        assert!(!settings.show_popup_without_hotkey);
    }
}
