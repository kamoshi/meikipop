use std::error::Error;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use screencapturekit::content_sharing_picker::{
    SCContentSharingPicker, SCContentSharingPickerConfiguration, SCContentSharingPickerMode,
    SCPickerOutcome,
};
use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;

use crate::screenshot::interface::{FrameProvider, Monitor, Screenshot};

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

pub struct ScreenCaptureKitFrameProvider {
    monitors: Vec<Monitor>,
    filter: Option<SCContentFilter>,
    pixel_size: (u32, u32),
    stream: Option<SCStream>,
    shared: Arc<SharedCapture>,
}

impl ScreenCaptureKitFrameProvider {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        log::info!("Using macOS ScreenCaptureKit backend");
        let (filter, monitor, pixel_size) = pick_display()?;

        // Preserve the FrameProvider convention used by the pipeline: index 0
        // is a virtual-desktop entry and index 1 is the selected source. With
        // a system picker both entries intentionally describe the same screen.
        let monitors = vec![monitor.clone(), monitor];

        Ok(Self {
            monitors,
            filter: Some(filter),
            pixel_size,
            stream: None,
            shared: Arc::new(SharedCapture::default()),
        })
    }

    fn ensure_stream(&mut self) -> Result<(), Box<dyn Error>> {
        if self.stream.is_some() {
            return Ok(());
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

        let filter = self
            .filter
            .as_ref()
            .ok_or("The selected ScreenCaptureKit source is no longer available")?;
        let config = SCStreamConfiguration::new()
            .with_width(self.pixel_size.0)
            .with_height(self.pixel_size.1)
            .with_pixel_format(PixelFormat::BGRA)
            .with_shows_cursor(false)
            .with_queue_depth(3)
            .with_fps(10);

        let shared = Arc::clone(&self.shared);
        let mut stream = SCStream::new(filter, &config);
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

impl FrameProvider for ScreenCaptureKitFrameProvider {
    fn monitors(&mut self) -> Result<Vec<Monitor>, Box<dyn Error>> {
        Ok(self.monitors.clone())
    }

    fn frame(&mut self, _monitor: &Monitor) -> Result<Screenshot, Box<dyn Error>> {
        self.ensure_stream()?;
        self.latest_frame()
    }
}

fn pick_display() -> Result<(SCContentFilter, Monitor, (u32, u32)), Box<dyn Error>> {
    let mut configuration = SCContentSharingPickerConfiguration::new();
    configuration.set_allowed_picker_modes(&[SCContentSharingPickerMode::SingleDisplay]);
    configuration.set_allows_changing_selected_content(false);

    let (sender, receiver) = mpsc::sync_channel(1);
    SCContentSharingPicker::show(&configuration, move |outcome| {
        let _ = sender.send(outcome);
    });

    let result = match receiver.recv() {
        Ok(SCPickerOutcome::Picked(result)) => result,
        Ok(SCPickerOutcome::Cancelled) => return Err("Screen selection was cancelled".into()),
        Ok(SCPickerOutcome::Error(error)) => {
            return Err(format!("The macOS screen picker failed: {error}").into());
        }
        Err(_) => return Err("The macOS screen picker closed without a result".into()),
    };

    let display = result
        .displays()
        .into_iter()
        .next()
        .ok_or("The macOS screen picker did not return a display")?;
    let monitor = monitor_from_display(&display)
        .ok_or("The macOS screen picker returned invalid display geometry")?;
    let pixel_size = result.pixel_size();
    if pixel_size.0 == 0 || pixel_size.1 == 0 {
        return Err("The macOS screen picker returned an empty display".into());
    }

    log::info!(
        "Selected display {} through the macOS system picker: left={}, top={}, width={}, height={}, pixels={}x{}",
        display.display_id(),
        monitor.left,
        monitor.top,
        monitor.width,
        monitor.height,
        pixel_size.0,
        pixel_size.1
    );
    Ok((result.filter(), monitor, pixel_size))
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
