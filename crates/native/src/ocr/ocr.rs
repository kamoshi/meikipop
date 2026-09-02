// meikipop/ocr/ocr.rs

use std::error::Error;

use crate::ocr::interface::{Mat, OcrProvider, Paragraph};
use crate::ocr::providers::meikiocr::provider::MeikiOcrProvider;

pub const DEFAULT_PROVIDER_ID: &str = "meikiocr";
#[cfg(target_os = "macos")]
const APPLE_VISION_PROVIDER_ID: &str = "apple_vision";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OcrProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
}

const MEIKIOCR_PROVIDER: OcrProviderInfo = OcrProviderInfo {
    id: DEFAULT_PROVIDER_ID,
    name: "meikiocr (local)",
};

#[cfg(target_os = "macos")]
const APPLE_VISION_PROVIDER: OcrProviderInfo = OcrProviderInfo {
    id: APPLE_VISION_PROVIDER_ID,
    name: "Apple Vision (macOS)",
};

pub struct OcrProcessor {
    ocr_backend: Box<dyn OcrProvider>,
    active_provider_id: &'static str,
}

impl OcrProcessor {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let configured = std::env::var("MEIKIPOP_OCR_PROVIDER").ok();
        Self::new_with_provider(configured.as_deref())
    }

    pub fn new_with_provider(provider_id: Option<&str>) -> Result<Self, Box<dyn Error>> {
        let requested = provider_id.unwrap_or(DEFAULT_PROVIDER_ID);
        let selected = if Self::provider_info(requested).is_some() {
            requested
        } else {
            log::warn!(
                "Configured OCR provider '{requested}' is unavailable; using '{DEFAULT_PROVIDER_ID}'"
            );
            DEFAULT_PROVIDER_ID
        };
        let (active_provider_id, ocr_backend) = Self::create_with_fallback(selected)?;
        log::info!("Initialized OCR with '{}' provider", ocr_backend.name());
        Ok(Self {
            ocr_backend,
            active_provider_id,
        })
    }

    pub fn scan(&mut self, image: &Mat) -> Result<Vec<Paragraph>, Box<dyn Error>> {
        self.ocr_backend.scan(image)
    }

    pub fn active_provider_id(&self) -> &'static str {
        self.active_provider_id
    }

    pub fn available_providers() -> Vec<OcrProviderInfo> {
        let mut providers = vec![MEIKIOCR_PROVIDER];
        #[cfg(target_os = "macos")]
        providers.push(APPLE_VISION_PROVIDER);
        providers
    }

    /// Switches backend and returns whether a replacement actually occurred.
    pub fn switch_provider(&mut self, provider_id: &str) -> Result<bool, Box<dyn Error>> {
        if provider_id == self.active_provider_id {
            return Ok(false);
        }
        let info = Self::provider_info(provider_id)
            .ok_or_else(|| format!("Unknown OCR provider: '{provider_id}'"))?;
        log::info!("Switching OCR provider to '{}'", info.name);
        // Construct first so a failed initialization leaves the active backend
        // untouched and immediately usable.
        let provider = Self::create_provider(provider_id)?;
        self.ocr_backend = provider;
        self.active_provider_id = info.id;
        log::info!("Switched OCR provider to '{}'", info.name);
        Ok(true)
    }

    fn provider_info(provider_id: &str) -> Option<OcrProviderInfo> {
        Self::available_providers()
            .into_iter()
            .find(|provider| provider.id == provider_id)
    }

    fn create_with_fallback(
        provider_id: &str,
    ) -> Result<(&'static str, Box<dyn OcrProvider>), Box<dyn Error>> {
        match Self::create_provider(provider_id) {
            Ok(provider) => Ok((Self::provider_info(provider_id).unwrap().id, provider)),
            Err(error) if provider_id != DEFAULT_PROVIDER_ID => {
                log::warn!(
                    "Failed to initialize OCR provider '{provider_id}': {error}; falling back to '{DEFAULT_PROVIDER_ID}'"
                );
                Ok((
                    DEFAULT_PROVIDER_ID,
                    Self::create_provider(DEFAULT_PROVIDER_ID)?,
                ))
            }
            Err(error) => Err(error),
        }
    }

    fn create_provider(provider_id: &str) -> Result<Box<dyn OcrProvider>, Box<dyn Error>> {
        match provider_id {
            DEFAULT_PROVIDER_ID => Ok(Box::new(MeikiOcrProvider::new()?)),
            #[cfg(target_os = "macos")]
            APPLE_VISION_PROVIDER_ID => Ok(Box::new(
                crate::ocr::providers::apple_vision::AppleVisionOcrProvider::new()?,
            )),
            _ => Err(format!("Unknown OCR provider: '{provider_id}'").into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::ocr::providers::dummy::provider::DummyProvider;

    #[test]
    fn discovers_the_native_meikiocr_provider() {
        assert!(
            OcrProcessor::available_providers()
                .iter()
                .any(|provider| provider.id == DEFAULT_PROVIDER_ID),
            "the local OCR provider should be available on every platform"
        );
    }

    #[test]
    fn provider_ids_are_unique_and_separate_from_display_names() {
        let providers = OcrProcessor::available_providers();
        let ids: HashSet<_> = providers.iter().map(|provider| provider.id).collect();

        assert_eq!(ids.len(), providers.len());
        assert!(
            providers
                .iter()
                .all(|provider| provider.id != provider.name)
        );
    }

    #[test]
    fn an_unknown_provider_does_not_replace_the_active_backend() {
        let mut processor = OcrProcessor {
            ocr_backend: Box::new(DummyProvider),
            active_provider_id: DEFAULT_PROVIDER_ID,
        };

        assert!(processor.switch_provider("not_registered").is_err());
        assert_eq!(processor.active_provider_id(), DEFAULT_PROVIDER_ID);
        assert_eq!(processor.ocr_backend.name(), DummyProvider::NAME);
    }

    #[test]
    fn returns_provider_paragraphs_directly() {
        let mut processor = OcrProcessor {
            ocr_backend: Box::new(DummyProvider),
            active_provider_id: DEFAULT_PROVIDER_ID,
        };
        let image = Mat::new(800, 600);

        assert_eq!(processor.scan(&image).unwrap().len(), 2);
    }
}
