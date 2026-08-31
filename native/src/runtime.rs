use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::ThreadId;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

#[derive(Default)]
struct EventState {
    is_set: bool,
}

#[pyclass(name = "Event")]
pub struct PyEvent {
    state: Mutex<EventState>,
    changed: Condvar,
}

#[pymethods]
impl PyEvent {
    #[new]
    fn new() -> Self {
        Self {
            state: Mutex::new(EventState::default()),
            changed: Condvar::new(),
        }
    }

    fn set(&self) -> PyResult<()> {
        let mut state = lock(&self.state, "event")?;
        state.is_set = true;
        self.changed.notify_all();
        Ok(())
    }

    fn clear(&self) -> PyResult<()> {
        lock(&self.state, "event")?.is_set = false;
        Ok(())
    }

    fn wait(&self, py: Python<'_>) -> PyResult<bool> {
        py.detach(|| {
            let mut state = lock(&self.state, "event")?;
            while !state.is_set {
                state = self
                    .changed
                    .wait(state)
                    .map_err(|_| PyRuntimeError::new_err("event lock was poisoned"))?;
            }
            Ok(true)
        })
    }

    fn is_set(&self) -> PyResult<bool> {
        Ok(lock(&self.state, "event")?.is_set)
    }
}

#[derive(Default)]
struct ReentrantLockState {
    owner: Option<ThreadId>,
    recursion: usize,
}

#[pyclass(name = "RLock")]
pub struct PyRLock {
    state: Mutex<ReentrantLockState>,
    changed: Condvar,
}

#[pymethods]
impl PyRLock {
    #[new]
    fn new() -> Self {
        Self {
            state: Mutex::new(ReentrantLockState::default()),
            changed: Condvar::new(),
        }
    }

    fn acquire(&self, py: Python<'_>) -> PyResult<bool> {
        let thread_id = std::thread::current().id();
        py.detach(|| {
            let mut state = lock(&self.state, "re-entrant lock")?;
            while state.owner.is_some() && state.owner != Some(thread_id) {
                state = self
                    .changed
                    .wait(state)
                    .map_err(|_| PyRuntimeError::new_err("re-entrant lock was poisoned"))?;
            }
            state.owner = Some(thread_id);
            state.recursion += 1;
            Ok(true)
        })
    }

    fn release(&self) -> PyResult<()> {
        let thread_id = std::thread::current().id();
        let mut state = lock(&self.state, "re-entrant lock")?;
        if state.owner != Some(thread_id) || state.recursion == 0 {
            return Err(PyRuntimeError::new_err(
                "cannot release un-acquired re-entrant lock",
            ));
        }
        state.recursion -= 1;
        if state.recursion == 0 {
            state.owner = None;
            self.changed.notify_one();
        }
        Ok(())
    }

    fn __enter__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<PyRef<'py, Self>> {
        slf.acquire(py)?;
        Ok(slf)
    }

    fn __exit__(
        &self,
        _exception_type: Option<&Bound<'_, PyAny>>,
        _exception: Option<&Bound<'_, PyAny>>,
        _traceback: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        self.release()?;
        Ok(false)
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>, name: &str) -> PyResult<MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|_| PyRuntimeError::new_err(format!("{name} lock was poisoned")))
}

pub fn register_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyEvent>()?;
    module.add_class::<PyRLock>()?;
    Ok(())
}
