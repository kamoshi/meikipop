// meikipop/ocr/ocr.rs

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

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
    inner: Arc<Mutex<OcrProcessor>>,
    worker_started: AtomicBool,
}

#[pymethods]
impl PyOcrProcessor {
    #[new]
    fn new() -> PyResult<Self> {
        Ok(Self {
            inner: Arc::new(Mutex::new(
                OcrProcessor::new().map_err(|error| PyRuntimeError::new_err(error.to_string()))?,
            )),
            worker_started: AtomicBool::new(false),
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

    fn start_worker(
        &self,
        py: Python<'_>,
        shared_state: Py<PyAny>,
        config: Py<PyAny>,
        logger: Py<PyAny>,
    ) -> PyResult<()> {
        if self.worker_started.swap(true, Ordering::AcqRel) {
            return Err(PyRuntimeError::new_err("threads can only be started once"));
        }

        let inner = Arc::clone(&self.inner);
        let shared_state = shared_state.clone_ref(py);
        let config = config.clone_ref(py);
        let logger = logger.clone_ref(py);
        thread::Builder::new()
            .name("OcrProcessor".to_owned())
            .spawn(move || run_worker(inner, shared_state, config, logger))
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        Ok(())
    }

    fn switch_provider(&self, provider_name: &str) -> PyResult<()> {
        self.lock()?
            .switch_provider(provider_name)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }
}

fn run_worker(
    inner: Arc<Mutex<OcrProcessor>>,
    shared_state: Py<PyAny>,
    config: Py<PyAny>,
    logger: Py<PyAny>,
) {
    python_log(&logger, "debug", "OCR thread started.");
    while python_bool_attribute(&shared_state, "running") {
        let result = Python::attach(|py| -> PyResult<()> {
            let screenshot = shared_state
                .getattr(py, "ocr_queue")?
                .call_method0(py, "get")?;
            if !shared_state.getattr(py, "running")?.extract::<bool>(py)? {
                return Ok(());
            }

            python_log(&logger, "debug", "OCR: Triggered!");

            let start_time = Instant::now();
            let image_rgb = screenshot.call_method1(py, "convert", ("RGB",))?;
            let image_bytes = image_rgb.call_method0(py, "tobytes")?;
            let image_bytes: Vec<u8> = image_bytes.extract(py)?;
            let width = image_rgb.getattr(py, "width")?.extract(py)?;
            let height = image_rgb.getattr(py, "height")?.extract(py)?;
            let image = mat_from_rgb_bytes(&image_bytes, width, height)?;
            let (paragraph_count, provider_name) = py
                .detach(|| {
                    let mut processor = inner
                        .lock()
                        .map_err(|_| "OCR processor lock was poisoned".to_owned())?;
                    let paragraph_count =
                        processor.scan(&image).map_err(|error| error.to_string())?;
                    let provider_name = processor.provider_name().unwrap_or("Unknown OCR provider");
                    Ok::<_, String>((paragraph_count, provider_name))
                })
                .map_err(PyRuntimeError::new_err)?;
            python_log(
                &logger,
                "info",
                &format!(
                    "{provider_name} found {paragraph_count} paragraphs in {:.3}s.",
                    start_time.elapsed().as_secs_f64()
                ),
            );
            // todo keep last ocr result?

            shared_state
                .getattr(py, "hit_scan_queue")?
                .call_method0(py, "trigger")?;
            Ok(())
        });

        if let Err(error) = result {
            python_log(
                &logger,
                "error",
                &format!("An unexpected error occurred in the ocr loop. Continuing: {error}"),
            );
        }

        // This is the equivalent of upstream's `finally` block: auto mode
        // schedules the next screenshot even when OCR raised an error.
        Python::attach(|py| {
            let result = (|| -> PyResult<()> {
                if config.getattr(py, "auto_scan_mode")?.extract::<bool>(py)? {
                    shared_state
                        .getattr(py, "screenshot_trigger_event")?
                        .call_method0(py, "set")?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                python_log(
                    &logger,
                    "error",
                    &format!("Could not schedule the next automatic screenshot: {error}"),
                );
            }
        });
    }
    python_log(&logger, "debug", "OCR thread stopped.");
}

fn python_bool_attribute(object: &Py<PyAny>, name: &str) -> bool {
    Python::attach(|py| {
        object
            .getattr(py, name)
            .and_then(|value| value.extract(py))
            .unwrap_or_else(|error| {
                log::error!("Could not read {name}: {error}");
                false
            })
    })
}

fn python_log(logger: &Py<PyAny>, level: &str, message: &str) {
    Python::attach(|py| {
        if let Err(error) = logger.call_method1(py, level, (message,)) {
            log::error!("Could not forward OCR log message to Python: {error}");
        }
    });
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
