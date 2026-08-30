// meikipop/ocr/ocr.rs

use std::error::Error;
use std::sync::Mutex;

use opencv::core::Mat;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::ocr::hit_scan;
use crate::ocr::interface::{OcrProvider, Paragraph};
use crate::ocr::providers::meikiocr::ocr::mat_from_rgb_bytes;
use crate::ocr::providers::meikiocr::provider::MeikiOcrProvider;

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
        let configured_provider_name = DEFAULT_PROVIDER_NAME;
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
        providers
    }

    fn _create_provider(provider_name: &str) -> Result<Box<dyn OcrProvider>, Box<dyn Error>> {
        match provider_name {
            DEFAULT_PROVIDER_NAME => Ok(Box::new(MeikiOcrProvider::new()?)),
            _ => Err(format!("Unknown OCR provider: '{provider_name}'").into()),
        }
    }

    fn provider_name(&self) -> Option<&'static str> {
        self.ocr_backend.as_ref().map(|provider| provider.name())
    }
}

#[pyclass(name = "OcrProcessor")]
pub struct PyOcrProcessor {
    inner: Mutex<OcrProcessor>,
}

#[pymethods]
impl PyOcrProcessor {
    #[new]
    fn new() -> PyResult<Self> {
        Ok(Self {
            inner: Mutex::new(
                OcrProcessor::new().map_err(|error| PyRuntimeError::new_err(error.to_string()))?,
            ),
        })
    }

    #[getter]
    fn available_providers(&self) -> PyResult<Vec<&'static str>> {
        Ok(self.lock()?.available_providers.clone())
    }

    #[getter]
    #[pyo3(name = "NAME")]
    fn name(&self) -> PyResult<&'static str> {
        self.lock()?
            .provider_name()
            .ok_or_else(|| PyRuntimeError::new_err("OCR provider was not initialized"))
    }

    fn scan(
        &self,
        py: Python<'_>,
        image: &Bound<'_, PyBytes>,
        width: usize,
        height: usize,
    ) -> PyResult<usize> {
        let image = mat_from_rgb_bytes(image.as_bytes(), width, height)?;
        py.detach(|| {
            let mut processor = self
                .inner
                .lock()
                .map_err(|_| "OCR processor lock was poisoned".to_owned())?;
            processor.scan(&image).map_err(|error| error.to_string())
        })
        .map_err(PyRuntimeError::new_err)
    }

    fn hit_scan(&self, norm_x: f64, norm_y: f64) -> PyResult<Option<String>> {
        Ok(self.lock()?.hit_scan(norm_x, norm_y))
    }

    fn switch_provider(&self, provider_name: &str) -> PyResult<()> {
        self.lock()?
            .switch_provider(provider_name)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }
}

impl PyOcrProcessor {
    fn lock(&self) -> PyResult<std::sync::MutexGuard<'_, OcrProcessor>> {
        self.inner
            .lock()
            .map_err(|_| PyRuntimeError::new_err("OCR processor lock was poisoned"))
    }
}

pub fn register_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyOcrProcessor>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::providers::dummy::provider::DummyProvider;
    use opencv::core::{CV_8UC3, Scalar};

    #[test]
    fn discovers_the_native_meikiocr_provider() {
        assert_eq!(OcrProcessor::_discover_providers(), [DEFAULT_PROVIDER_NAME]);
    }

    #[test]
    fn keeps_paragraphs_in_rust_for_later_hit_scans() {
        let mut processor = OcrProcessor {
            ocr_backend: Some(Box::new(DummyProvider)),
            available_providers: vec![DummyProvider::NAME],
            last_ocr_result: None,
        };
        let image = Mat::new_rows_cols_with_default(600, 800, CV_8UC3, Scalar::all(0.0)).unwrap();

        assert_eq!(processor.scan(&image).unwrap(), 2);
        assert_eq!(processor.last_ocr_result.as_ref().unwrap().len(), 2);
        assert!(processor.hit_scan(0.15, 0.28).is_some());
    }
}
