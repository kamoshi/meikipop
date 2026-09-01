use std::error::Error;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;

use crate::screenshot::interface::{DisplayDescriptor, FrameProvider, Monitor, Screenshot};

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct CaptureState {
    frame: Option<Screenshot>,
    error: Option<String>,
}

#[derive(Default)]
struct SharedCapture {
    state: Mutex<CaptureState>,
    changed: Condvar,
}

struct DisplayInfo {
    display: SCDisplay,
    monitor: Monitor,
}

pub struct ScreenCaptureKitFrameProvider {
    displays: Vec<DisplayInfo>,
    monitors: Vec<Monitor>,
    active_display_id: Option<u32>,
    stream: Option<SCStream>,
    shared: Arc<SharedCapture>,
}

impl ScreenCaptureKitFrameProvider {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        log::info!("Using macOS ScreenCaptureKit backend");
        let content = SCShareableContent::get()?;
        let displays = content
            .displays()
            .into_iter()
            .filter_map(|display| {
                monitor_from_display(&display).map(|monitor| DisplayInfo { display, monitor })
            })
            .collect::<Vec<_>>();

        if displays.is_empty() {
            return Err("ScreenCaptureKit returned no displays".into());
        }

        // Match mss and the Linux backend: index 0 is the virtual desktop,
        // while physical displays begin at index 1.
        let mut monitors = Vec::with_capacity(displays.len() + 1);
        monitors.push(virtual_monitor(&displays));
        monitors.extend(displays.iter().map(|display| display.monitor.clone()));

        Ok(Self {
            displays,
            monitors,
            active_display_id: None,
            stream: None,
            shared: Arc::new(SharedCapture::default()),
        })
    }

    fn ensure_stream(&mut self, monitor: &Monitor) -> Result<(), Box<dyn Error>> {
        let display = self
            .displays
            .iter()
            .find(|display| display.monitor == *monitor)
            .ok_or("Capturing the combined macOS virtual desktop is not supported yet")?;
        let display_id = display.display.display_id();

        if self.active_display_id == Some(display_id) && self.stream.is_some() {
            return Ok(());
        }

        if let Some(stream) = self.stream.take() {
            let _ = stream.stop_capture();
        }
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.frame = None;
            state.error = None;
        }

        let filter = SCContentFilter::builder()
            .display(&display.display)
            .exclude_windows(&[])
            .build();
        let config = SCStreamConfiguration::new()
            .with_width(display.display.width())
            .with_height(display.display.height())
            .with_pixel_format(PixelFormat::BGRA)
            .with_shows_cursor(false)
            .with_queue_depth(3)
            .with_fps(10);

        let shared = Arc::clone(&self.shared);
        let mut stream = SCStream::new(&filter, &config);
        let handler_id = stream.add_output_handler(
            move |sample: CMSampleBuffer, output_type: SCStreamOutputType| {
                if output_type != SCStreamOutputType::Screen {
                    return;
                }

                let result = screenshot_from_sample(&sample);
                let mut state = shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                match result {
                    Ok(frame) => {
                        state.frame = Some(frame);
                        state.error = None;
                    }
                    Err(error) => state.error = Some(error),
                }
                shared.changed.notify_all();
            },
            SCStreamOutputType::Screen,
        );
        if handler_id.is_none() {
            return Err("ScreenCaptureKit rejected the screen output handler".into());
        }

        stream.start_capture()?;
        self.active_display_id = Some(display_id);
        self.stream = Some(stream);
        Ok(())
    }

    fn latest_frame(&self) -> Result<Screenshot, Box<dyn Error>> {
        let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        loop {
            if let Some(frame) = &state.frame {
                return Ok(frame.clone());
            }
            if let Some(error) = &state.error {
                return Err(error.clone().into());
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Timed out waiting for the first ScreenCaptureKit frame".into());
            }
            let (next_state, timeout) = self
                .shared
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner());
            state = next_state;
            if timeout.timed_out() && state.frame.is_none() {
                return Err("Timed out waiting for the first ScreenCaptureKit frame".into());
            }
        }
    }
}

pub fn discover_displays() -> Result<Vec<DisplayDescriptor>, Box<dyn Error>> {
    let content = SCShareableContent::get()?;
    Ok(content
        .displays()
        .into_iter()
        .filter_map(|display| {
            monitor_from_display(&display).map(|monitor| DisplayDescriptor {
                id: display.display_id(),
                top: monitor.top,
                left: monitor.left,
                width: monitor.width,
                height: monitor.height,
            })
        })
        .collect())
}

impl FrameProvider for ScreenCaptureKitFrameProvider {
    fn monitors(&mut self) -> Result<Vec<Monitor>, Box<dyn Error>> {
        Ok(self.monitors.clone())
    }

    fn frame(&mut self, monitor: &Monitor) -> Result<Screenshot, Box<dyn Error>> {
        self.ensure_stream(monitor)?;
        self.latest_frame()
    }
}

impl Drop for ScreenCaptureKitFrameProvider {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.stop_capture();
        }
    }
}

fn monitor_from_display(display: &SCDisplay) -> Option<Monitor> {
    let frame = display.frame();
    if !frame.x.is_finite()
        || !frame.y.is_finite()
        || !frame.width.is_finite()
        || !frame.height.is_finite()
        || frame.width <= 0.0
        || frame.height <= 0.0
    {
        return None;
    }

    Some(Monitor {
        left: frame.x.round() as i32,
        top: frame.y.round() as i32,
        width: frame.width.round() as usize,
        height: frame.height.round() as usize,
    })
}

fn virtual_monitor(displays: &[DisplayInfo]) -> Monitor {
    let left = displays
        .iter()
        .map(|display| display.monitor.left)
        .min()
        .unwrap_or_default();
    let top = displays
        .iter()
        .map(|display| display.monitor.top)
        .min()
        .unwrap_or_default();
    let right = displays
        .iter()
        .map(|display| display.monitor.left + display.monitor.width as i32)
        .max()
        .unwrap_or_default();
    let bottom = displays
        .iter()
        .map(|display| display.monitor.top + display.monitor.height as i32)
        .max()
        .unwrap_or_default();

    Monitor {
        left,
        top,
        width: right.saturating_sub(left) as usize,
        height: bottom.saturating_sub(top) as usize,
    }
}

fn screenshot_from_sample(sample: &CMSampleBuffer) -> Result<Screenshot, String> {
    let pixel_buffer = sample
        .image_buffer()
        .ok_or_else(|| "ScreenCaptureKit frame has no image buffer".to_owned())?;
    let guard = pixel_buffer
        .lock(CVPixelBufferLockFlags::READ_ONLY)
        .map_err(|status| format!("Could not lock ScreenCaptureKit pixel buffer: {status}"))?;
    pack_bgra(
        guard.as_slice(),
        guard.width(),
        guard.height(),
        guard.bytes_per_row(),
    )
}

fn pack_bgra(
    data: &[u8],
    width: usize,
    height: usize,
    stride: usize,
) -> Result<Screenshot, String> {
    let row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| "ScreenCaptureKit frame width is too large".to_owned())?;
    if stride < row_bytes {
        return Err(format!(
            "ScreenCaptureKit row stride {stride} is smaller than {row_bytes}"
        ));
    }
    let required = stride
        .checked_mul(height)
        .ok_or_else(|| "ScreenCaptureKit frame dimensions are too large".to_owned())?;
    if data.len() < required {
        return Err(format!(
            "ScreenCaptureKit frame has {} bytes, expected at least {required}",
            data.len()
        ));
    }

    let mut raw = Vec::with_capacity(row_bytes * height);
    for row in data[..required].chunks_exact(stride) {
        raw.extend_from_slice(&row[..row_bytes]);
    }
    Ok(Screenshot { raw, width, height })
}

#[cfg(test)]
mod tests {
    use super::pack_bgra;

    #[test]
    fn removes_screen_capture_row_padding() {
        let data = [
            1, 2, 3, 4, 5, 6, 7, 8, 99, 99, 99, 99, 9, 10, 11, 12, 13, 14, 15, 16, 88, 88, 88, 88,
        ];
        let screenshot = pack_bgra(&data, 2, 2, 12).unwrap();
        assert_eq!((screenshot.width, screenshot.height), (2, 2));
        assert_eq!(
            screenshot.raw,
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
    }
}
