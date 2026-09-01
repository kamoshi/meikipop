// meikipop/ocr/ocr.rs

use std::error::Error;

use crate::ocr::hit_scan;
use crate::ocr::interface::{Mat, OcrProvider, Paragraph};
use crate::ocr::providers::meikiocr::provider::MeikiOcrProvider;
use crate::screenshot::interface::RgbImage;

const DEFAULT_PROVIDER_NAME: &str = "meikiocr (local)";

pub struct OcrProcessor {
    pub ocr_backend: Option<Box<dyn OcrProvider>>,
    pub available_providers: Vec<&'static str>,
    pub last_ocr_result: Option<Vec<Paragraph>>,
}

impl OcrProcessor {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let available_providers = Self::_discover_providers();
        if available_providers.is_empty() {
            return Err("No OCR providers found! The application cannot continue.".into());
        }

        let mut processor = Self {
            ocr_backend: None,
            available_providers,
            last_ocr_result: None,
        };
        processor._load_provider_from_config()?;
        Ok(processor)
    }

    pub fn scan(&mut self, image: &Mat) -> Result<usize, Box<dyn Error>> {
        let Some(ocr_backend) = self.ocr_backend.as_mut() else {
            return Err("OCR provider was not initialized".into());
        };

        let ocr_result = ocr_backend.scan(image)?;
        let paragraph_count = ocr_result.len();
        // todo keep last ocr result?
        self.last_ocr_result = Some(ocr_result);
        Ok(paragraph_count)
    }

    pub fn scan_rgb(&mut self, image: &RgbImage) -> Result<usize, Box<dyn Error>> {
        self.scan(image)
    }

    pub fn hit_scan(&self, norm_x: f64, norm_y: f64) -> Option<String> {
        let paragraphs = self.last_ocr_result.as_deref()?;
        hit_scan::hit_scan(paragraphs, norm_x, norm_y)
    }

    // todo combine methods?
    pub fn switch_provider(&mut self, provider_name: &str) -> Result<(), Box<dyn Error>> {
        if self
            .ocr_backend
            .as_ref()
            .is_some_and(|ocr_backend| provider_name == ocr_backend.name())
        {
            return Ok(());
        }

        if self.available_providers.contains(&provider_name) {
            log::info!("Switching OCR provider to '{provider_name}'...");
            let previous_backend = self.ocr_backend.take();
            match Self::_create_provider(provider_name) {
                Ok(provider) => {
                    log::info!(
                        "Successfully switched OCR provider to '{}'",
                        provider.name()
                    );
                    self.ocr_backend = Some(provider);
                    self.last_ocr_result = None;
                    Ok(())
                }
                Err(error) => {
                    log::error!("Failed to instantiate provider '{provider_name}': {error}");
                    if let Some(previous_backend) = previous_backend {
                        log::info!(
                            "Reverting to previous provider '{}'.",
                            previous_backend.name()
                        );
                        self.ocr_backend = Some(previous_backend);
                    }
                    Err(error)
                }
            }
        } else {
            Err(format!("Attempted to switch to an unknown provider: '{provider_name}'").into())
        }
    }

    fn _load_provider_from_config(&mut self) -> Result<(), Box<dyn Error>> {
        let env_provider = std::env::var("MEIKIPOP_OCR_PROVIDER").ok();
        let configured_provider_name = env_provider.as_deref().unwrap_or(DEFAULT_PROVIDER_NAME);
        let default_provider_name = DEFAULT_PROVIDER_NAME;

        let mut provider_to_load_name = configured_provider_name;

        if !self.available_providers.contains(&configured_provider_name) {
            log::warn!(
                "Configured OCR provider '{configured_provider_name}' not found. Falling back to default provider '{default_provider_name}'."
            );
            provider_to_load_name = default_provider_name;
        }

        if !self.available_providers.contains(&provider_to_load_name) {
            let fallback_provider_name = self.available_providers[0];
            log::warn!(
                "Default OCR provider '{provider_to_load_name}' not found. Falling back to first available provider: '{fallback_provider_name}'."
            );
            provider_to_load_name = fallback_provider_name;
        }

        match Self::_create_provider(provider_to_load_name) {
            Ok(provider) => {
                log::info!("Initialized OCR with '{}' provider.", provider.name());
                self.ocr_backend = Some(provider);
                Ok(())
            }
            Err(error) => {
                log::error!(
                    "Failed to instantiate provider '{provider_to_load_name}' on startup: {error}"
                );
                self.switch_provider(default_provider_name)
            }
        }
    }

    fn _discover_providers() -> Vec<&'static str> {
        let mut providers = Vec::new();

        // Rust providers are registered statically instead of discovered by
        // scanning Python packages.
        providers.push(DEFAULT_PROVIDER_NAME);
        #[cfg(target_os = "macos")]
        providers.push("apple_vision (macOS)");

        providers
    }

    fn _create_provider(provider_name: &str) -> Result<Box<dyn OcrProvider>, Box<dyn Error>> {
        match provider_name {
            DEFAULT_PROVIDER_NAME => Ok(Box::new(MeikiOcrProvider::new()?)),
            #[cfg(target_os = "macos")]
            "apple_vision (macOS)" => Ok(Box::new(
                crate::ocr::providers::apple_vision::AppleVisionOcrProvider::new()?,
            )),
            _ => Err(format!("Unknown OCR provider: '{provider_name}'").into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::providers::dummy::provider::DummyProvider;

    #[test]
    fn discovers_the_native_meikiocr_provider() {
        assert!(
            OcrProcessor::_discover_providers().contains(&DEFAULT_PROVIDER_NAME),
            "the local OCR provider should be available on every platform"
        );
    }

    #[test]
    fn keeps_paragraphs_in_rust_for_later_hit_scans() {
        let mut processor = OcrProcessor {
            ocr_backend: Some(Box::new(DummyProvider)),
            available_providers: vec![DummyProvider::NAME],
            last_ocr_result: None,
        };
        let image = Mat::new(800, 600);

        assert_eq!(processor.scan(&image).unwrap(), 2);
        assert_eq!(processor.last_ocr_result.as_ref().unwrap().len(), 2);
        assert!(processor.hit_scan(0.15, 0.28).is_some());
    }
}
