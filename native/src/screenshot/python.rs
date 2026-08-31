use std::collections::HashMap;
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use pyo3::prelude::*;

use crate::ocr::ocr::PyOcrImageQueue;
use crate::screenshot::interface::{Monitor, RgbImage, ScreenshotBackend};
use crate::screenshot::screenmanager::{ScreenManager, ScreenManagerConfig, ScreenManagerRuntime};
use crate::screenshot::wayland_mss_shim::MssWaylandShim;

struct PythonScreenManagerRuntime {
    shared_state: Py<PyAny>,
    input_loop: Py<PyAny>,
    config: Py<PyAny>,
    ocr_queue: Arc<crate::utils::latest_queue::LatestValueQueue<Option<RgbImage>>>,
}

impl PythonScreenManagerRuntime {
    fn call_shared_method(&self, owner: &str, method: &str) {
        Python::attach(|py| {
            if let Err(error) = self
                .shared_state
                .getattr(py, owner)
                .and_then(|value| value.call_method0(py, method))
            {
                log::error!("Could not call shared_state.{owner}.{method}(): {error}");
            }
        });
    }
}

impl ScreenManagerRuntime for PythonScreenManagerRuntime {
    fn running(&self) -> bool {
        Python::attach(|py| {
            self.shared_state
                .getattr(py, "running")
                .and_then(|value| value.extract(py))
                .unwrap_or_else(|error| {
                    log::error!("Could not read shared_state.running: {error}");
                    false
                })
        })
    }

    fn config(&self) -> ScreenManagerConfig {
        Python::attach(|py| {
            let read_bool = |name| {
                self.config
                    .getattr(py, name)
                    .and_then(|value| value.extract(py))
                    .unwrap_or_else(|error| {
                        log::error!("Could not read config.{name}: {error}");
                        false
                    })
            };
            let auto_scan_interval_seconds = self
                .config
                .getattr(py, "auto_scan_interval_seconds")
                .and_then(|value| value.extract(py))
                .unwrap_or_else(|error| {
                    log::error!("Could not read config.auto_scan_interval_seconds: {error}");
                    0.0
                });

            ScreenManagerConfig {
                auto_scan_mode: read_bool("auto_scan_mode"),
                is_enabled: read_bool("is_enabled"),
                auto_scan_interval_seconds,
                auto_scan_on_mouse_move: read_bool("auto_scan_on_mouse_move"),
            }
        })
    }

    fn wait_for_screenshot_trigger(&mut self) {
        self.call_shared_method("screenshot_trigger_event", "wait");
    }

    fn clear_screenshot_trigger(&mut self) {
        self.call_shared_method("screenshot_trigger_event", "clear");
    }

    fn get_mouse_pos(&mut self) -> (i32, i32) {
        Python::attach(|py| {
            self.input_loop
                .call_method0(py, "get_mouse_pos")
                .and_then(|value| value.extract(py))
                .unwrap_or_else(|error| {
                    log::error!("Could not call input_loop.get_mouse_pos(): {error}");
                    (0, 0)
                })
        })
    }

    fn screen_lock_acquire(&mut self) {
        self.call_shared_method("screen_lock", "acquire");
    }

    fn screen_lock_release(&mut self) {
        self.call_shared_method("screen_lock", "release");
    }

    fn put_ocr(&mut self, image: RgbImage) {
        self.ocr_queue.put(Some(image));
    }

    fn sleep(&mut self, interval: Duration) {
        thread::sleep(interval);
    }

    fn set_screenshot_trigger(&mut self) {
        self.call_shared_method("screenshot_trigger_event", "set");
    }

    fn trigger_hit_scan(&mut self) {
        self.call_shared_method("hit_scan_queue", "trigger");
    }

    fn log_error(&mut self, error: &dyn Error) {
        log::error!("An unexpected error occurred in the screenshot loop. Continuing: {error}");
    }
}

// todo doesnt work when monitors change
#[pyclass(name = "ScreenManager")]
pub struct PyScreenManager {
    backend: Mutex<Option<MssWaylandShim>>,
    monitors: Vec<Monitor>,
    monitor: Arc<Mutex<Option<Monitor>>>,
    force_screenshot: Arc<AtomicBool>,
    started: AtomicBool,
    shared_state: Py<PyAny>,
    input_loop: Py<PyAny>,
    config: Py<PyAny>,
}

#[pymethods]
impl PyScreenManager {
    #[new]
    fn new(
        py: Python<'_>,
        shared_state: Py<PyAny>,
        input_loop: Py<PyAny>,
        config: Py<PyAny>,
        token_path: String,
    ) -> PyResult<Self> {
        let mut backend = py
            .detach(|| MssWaylandShim::new(token_path).map_err(|error| error.to_string()))
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        let monitors = backend
            .monitors()
            .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;

        Ok(Self {
            backend: Mutex::new(Some(backend)),
            monitors,
            monitor: Arc::new(Mutex::new(None)),
            force_screenshot: Arc::new(AtomicBool::new(false)),
            started: AtomicBool::new(false),
            shared_state,
            input_loop,
            config,
        })
    }

    fn start(&self, py: Python<'_>) -> PyResult<()> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "threads can only be started once",
            ));
        }

        let backend = self
            .backend
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err("ScreenManager already started")
            })?;
        let monitor = Arc::clone(&self.monitor);
        let force_screenshot = Arc::clone(&self.force_screenshot);
        let mut runtime = PythonScreenManagerRuntime {
            shared_state: self.shared_state.clone_ref(py),
            input_loop: self.input_loop.clone_ref(py),
            config: self.config.clone_ref(py),
            ocr_queue: {
                let queue = self.shared_state.getattr(py, "ocr_queue")?;
                let queue: PyRef<'_, PyOcrImageQueue> = queue.extract(py)?;
                Arc::clone(&queue.inner)
            },
        };

        thread::Builder::new()
            .name("ScreenManager".to_owned())
            .spawn(move || {
                let mut screen_manager = ScreenManager::new(backend);
                log::debug!("Screenshot thread started.");
                while runtime.running() {
                    screen_manager.monitor = monitor
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone();
                    if force_screenshot.swap(false, Ordering::AcqRel) {
                        screen_manager.force_screenshot_trigger();
                    }

                    if let Err(error) = screen_manager.run_once(&mut runtime) {
                        runtime.log_error(error.as_ref());
                        screen_manager
                            ._sleep_and_handle_loop_exit(&mut runtime, Duration::from_secs(1));
                    }
                }
                log::debug!("Screenshot thread stopped.");
            })
            .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;
        Ok(())
    }

    fn set_scan_region(&self, top: i32, left: i32, width: usize, height: usize) {
        *self
            .monitor
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Monitor {
            top,
            left,
            width,
            height,
        });
    }

    fn set_scan_screen(&self, screen_index: usize) {
        log::info!("Set scan area to screen {screen_index}");
        if let Some(monitor) = self.monitors.get(screen_index) {
            log::info!("Set scan area to screen {screen_index}");
            *self
                .monitor
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(monitor.clone());
        } else {
            log::error!("Cannot set scan screen: index {screen_index} is out of bounds.");
        }
    }

    fn get_scan_geometry(&self) -> (i32, i32, usize, usize) {
        let monitor = self
            .monitor
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(monitor) = monitor.as_ref() else {
            return (0, 0, 0, 0);
        };
        (monitor.left, monitor.top, monitor.width, monitor.height)
    }

    fn force_screenshot_trigger(&self) {
        self.force_screenshot.store(true, Ordering::Release);
    }

    fn get_screens(&self) -> Vec<HashMap<&'static str, i64>> {
        self.monitors
            .iter()
            .map(|monitor| {
                HashMap::from([
                    ("top", i64::from(monitor.top)),
                    ("left", i64::from(monitor.left)),
                    ("width", monitor.width as i64),
                    ("height", monitor.height as i64),
                ])
            })
            .collect()
    }
}

pub fn register_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyScreenManager>()?;
    Ok(())
}
