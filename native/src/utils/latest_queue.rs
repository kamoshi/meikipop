// meikipop/utils/latest_queue.rs

use std::sync::{Condvar, Mutex, MutexGuard};

use pyo3::prelude::*;

struct QueueState<T> {
    value: Option<T>,
    event: bool,
}

pub struct LatestValueQueue<T> {
    state: Mutex<QueueState<T>>,
    changed: Condvar,
}

impl<T> Default for LatestValueQueue<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(QueueState {
                value: None,
                event: false,
            }),
            changed: Condvar::new(),
        }
    }
}

impl<T> LatestValueQueue<T> {
    pub fn put(&self, item: T) {
        let mut state = self.lock();
        state.value = Some(item);
        state.event = true;
        self.changed.notify_all();
    }

    pub fn wait(&self) {
        let mut state = self.lock();
        while !state.event {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    pub fn get_with<R>(&self, read: impl FnOnce(Option<&T>) -> R) -> R {
        let mut state = self.lock();
        let result = read(state.value.as_ref());
        state.event = false;
        result
    }

    pub fn trigger(&self) {
        let mut state = self.lock();
        state.event = true;
        self.changed.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, QueueState<T>> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[pyclass(name = "LatestValueQueue")]
pub struct PyLatestValueQueue {
    inner: LatestValueQueue<Py<PyAny>>,
}

#[pymethods]
impl PyLatestValueQueue {
    #[new]
    fn new() -> Self {
        Self {
            inner: LatestValueQueue::default(),
        }
    }

    fn put(&self, item: Py<PyAny>) {
        self.inner.put(item);
    }

    fn get(&self, py: Python<'_>) -> Py<PyAny> {
        py.detach(|| self.inner.wait());
        self.inner
            .get_with(|value| value.map(|value| value.clone_ref(py)))
            .unwrap_or_else(|| py.None())
    }

    fn trigger(&self) {
        self.inner.trigger();
    }
}

pub fn register_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyLatestValueQueue>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_replaces_the_previous_unread_value() {
        let queue = LatestValueQueue::default();
        queue.put(1);
        queue.put(2);
        queue.wait();
        assert_eq!(queue.get_with(|value| value.copied()), Some(2));
    }

    #[test]
    fn trigger_wakes_without_replacing_the_value() {
        let queue = LatestValueQueue::default();
        queue.put("value");
        queue.wait();
        assert_eq!(queue.get_with(|value| value.copied()), Some("value"));

        queue.trigger();
        queue.wait();
        assert_eq!(queue.get_with(|value| value.copied()), Some("value"));
    }
}
