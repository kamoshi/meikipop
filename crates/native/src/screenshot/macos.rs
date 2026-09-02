use std::error::Error;
use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use screencapturekit::cg::CGRect as SCKRect;
use screencapturekit::content_sharing_picker::{
    SCContentSharingPicker, SCContentSharingPickerConfiguration, SCContentSharingPickerMode,
    SCPickerOutcome,
};
use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;

use crate::platform::macos::window_server::query_window_geometry;
use crate::screenshot::interface::{CaptureGeometry, CapturedFrame, FrameProvider, Screenshot};

const FRAME_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Default)]
struct CaptureState {
    frame: Option<CapturedFrame>,
    frame_sequence: u64,
    error: Option<String>,
}

#[derive(Default)]
struct SharedCapture {
    state: Mutex<CaptureState>,
    changed: Condvar,
}

pub struct ScreenCaptureKitFrameProvider {
    geometry: CaptureGeometry,
    filter: SCContentFilter,
    pixel_size: (u32, u32),
    target: CaptureTarget,
    stream: Option<SCStream>,
    shared: Arc<SharedCapture>,
    last_geometry_check: Instant,
    last_delivered_sequence: u64,
}

impl ScreenCaptureKitFrameProvider {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        log::info!("Using macOS ScreenCaptureKit backend");
        let PickedSource {
            filter,
            geometry,
            pixel_size,
            target,
        } = pick_source()?;

        Ok(Self {
            geometry,
            filter,
            pixel_size,
            target,
            stream: None,
            shared: Arc::new(SharedCapture::default()),
            last_geometry_check: Instant::now(),
            last_delivered_sequence: 0,
        })
    }

    pub fn window_id(&self) -> Option<u32> {
        self.target.window_id()
    }

    fn refresh_window_geometry(&mut self) -> Option<u64> {
        let window_id = self.target.window_id()?;
        let now = Instant::now();
        if now.duration_since(self.last_geometry_check) < Duration::from_millis(200) {
            return None;
        }
        self.last_geometry_check = now;

        let live = query_window_geometry(window_id)?;

        // Position changes matter even when the capture resolution does not.
        self.geometry = live.clone();

        let scale = f64::from(self.filter.point_pixel_scale());
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            self.pixel_size.0 as f64 / live.width as f64
        };
        let requested_size = (
            (live.width as f64 * scale).round() as u32,
            (live.height as f64 * scale).round() as u32,
        );
        if requested_size.0 == 0 || requested_size.1 == 0 || requested_size == self.pixel_size {
            return None;
        }

        log::info!(
            "Target window resized to {}x{} (pixels {}x{}), updating capture stream",
            live.width,
            live.height,
            requested_size.0,
            requested_size.1
        );

        if let Some(stream) = &self.stream {
            let config = stream_configuration(requested_size);
            if let Err(error) = stream.update_configuration(&config) {
                // Keep pixel_size unchanged so the next geometry refresh retries.
                log::warn!(
                    "Failed to update ScreenCaptureKit stream configuration on resize: {error}"
                );
                return None;
            }
        }

        self.pixel_size = requested_size;
        Some(self.current_frame_sequence())
    }

    fn current_frame_sequence(&self) -> u64 {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .frame_sequence
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

        let config = stream_configuration(self.pixel_size);

        let shared = Arc::clone(&self.shared);
        let target = self.target.clone();
        let fallback_geometry = self.geometry.clone();
        let mut stream = SCStream::new(&self.filter, &config);
        let handler_id = stream.add_output_handler(
            move |sample: CMSampleBuffer, output_type: SCStreamOutputType| {
                if output_type != SCStreamOutputType::Screen {
                    return;
                }

                let result = screenshot_from_sample(&sample).map(|screenshot| {
                    let geometry =
                        frame_geometry(&target, &fallback_geometry, &sample, &screenshot);
                    (screenshot, geometry)
                });
                let mut state = shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                match result {
                    Ok((screenshot, geometry)) => {
                        state.frame_sequence = state.frame_sequence.wrapping_add(1);
                        let sequence = state.frame_sequence;
                        state.frame = Some(CapturedFrame {
                            sequence,
                            screenshot,
                            geometry,
                        });
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

    fn next_frame_after(
        &self,
        sequence: u64,
        expected_pixel_size: (u32, u32),
    ) -> Result<CapturedFrame, Box<dyn Error>> {
        let deadline = Instant::now() + FRAME_TIMEOUT;
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        loop {
            if let Some(frame) = &state.frame
                && frame_is_usable(frame, sequence, expected_pixel_size)
            {
                return Ok(frame.clone());
            }
            if let Some(error) = &state.error {
                return Err(error.clone().into());
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Timed out waiting for a new ScreenCaptureKit frame".into());
            }
            let (next_state, timeout) = self
                .shared
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner());
            state = next_state;
            if timeout.timed_out()
                && state
                    .frame
                    .as_ref()
                    .is_none_or(|frame| !frame_is_usable(frame, sequence, expected_pixel_size))
            {
                return Err("Timed out waiting for a new ScreenCaptureKit frame".into());
            }
        }
    }
}

impl FrameProvider for ScreenCaptureKitFrameProvider {
    fn capture_geometry(&mut self) -> Result<CaptureGeometry, Box<dyn Error>> {
        self.refresh_window_geometry();
        Ok(self.geometry.clone())
    }

    fn capture_frame(&mut self) -> Result<CapturedFrame, Box<dyn Error>> {
        self.ensure_stream()?;
        let mut minimum_sequence = self.last_delivered_sequence;
        if let Some(sequence_before_reconfiguration) = self.refresh_window_geometry() {
            minimum_sequence = minimum_sequence.max(sequence_before_reconfiguration);
        }
        let frame = self.next_frame_after(minimum_sequence, self.pixel_size)?;
        self.last_delivered_sequence = frame.sequence;
        self.geometry = frame.geometry.clone();
        Ok(frame)
    }
}

fn frame_is_usable(
    frame: &CapturedFrame,
    minimum_sequence: u64,
    expected_pixel_size: (u32, u32),
) -> bool {
    frame.sequence > minimum_sequence
        && frame.screenshot.width == expected_pixel_size.0 as usize
        && frame.screenshot.height == expected_pixel_size.1 as usize
}

fn stream_configuration(pixel_size: (u32, u32)) -> SCStreamConfiguration {
    SCStreamConfiguration::new()
        .with_width(pixel_size.0)
        .with_height(pixel_size.1)
        .with_ignores_shadows_single_window(true)
        .with_ignores_shadows_display(true)
        .with_pixel_format(PixelFormat::BGRA)
        .with_shows_cursor(false)
        .with_queue_depth(3)
        .with_fps(10)
}

#[derive(Clone)]
enum CaptureTarget {
    Display { display_id: u32 },
    Window { window_id: u32, title: String },
    Selection,
}

impl CaptureTarget {
    fn window_id(&self) -> Option<u32> {
        match self {
            Self::Window { window_id, .. } => Some(*window_id),
            Self::Display { .. } | Self::Selection => None,
        }
    }
}

impl fmt::Display for CaptureTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Display { display_id } => write!(formatter, "display {display_id}"),
            Self::Window { window_id, title } => {
                write!(formatter, "window {window_id} ({title})")
            }
            Self::Selection => formatter.write_str("selected content"),
        }
    }
}

struct PickedSource {
    filter: SCContentFilter,
    geometry: CaptureGeometry,
    pixel_size: (u32, u32),
    target: CaptureTarget,
}

fn pick_source() -> Result<PickedSource, Box<dyn Error>> {
    let mut configuration = SCContentSharingPickerConfiguration::new();
    configuration.set_allowed_picker_modes(&[
        SCContentSharingPickerMode::SingleDisplay,
        SCContentSharingPickerMode::SingleWindow,
    ]);
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

    let (geometry, target) = if let Some(display) = result.displays().into_iter().next() {
        let geometry = geometry_from_rect(display.frame())
            .ok_or("The macOS screen picker returned invalid display geometry")?;
        (
            geometry,
            CaptureTarget::Display {
                display_id: display.display_id(),
            },
        )
    } else if let Some(window) = result.windows().into_iter().next() {
        let geometry = geometry_from_rect(window.frame())
            .ok_or("The macOS screen picker returned invalid window geometry")?;
        let title = window.title().unwrap_or_else(|| "Untitled".into());
        (
            geometry,
            CaptureTarget::Window {
                window_id: window.window_id(),
                title,
            },
        )
    } else {
        let (x, y, width, height) = result.rect();
        let frame = SCKRect::new(x, y, width, height);
        let geometry = geometry_from_rect(frame)
            .ok_or("The macOS screen picker returned invalid content geometry")?;
        (geometry, CaptureTarget::Selection)
    };

    let pixel_size = result.pixel_size();
    let pixel_size = if pixel_size.0 == 0 || pixel_size.1 == 0 {
        let (width, height) = (geometry.width as u32, geometry.height as u32);
        if width == 0 || height == 0 {
            return Err("The macOS screen picker returned an empty selection".into());
        }
        (width, height)
    } else {
        pixel_size
    };

    log::info!(
        "Selected {target} through the macOS system picker: left={}, top={}, width={}, height={}, pixels={}x{}",
        geometry.left,
        geometry.top,
        geometry.width,
        geometry.height,
        pixel_size.0,
        pixel_size.1
    );
    Ok(PickedSource {
        filter: result.filter(),
        geometry,
        pixel_size,
        target,
    })
}

impl Drop for ScreenCaptureKitFrameProvider {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.stop_capture();
        }
    }
}

fn geometry_from_rect(frame: SCKRect) -> Option<CaptureGeometry> {
    if !frame.x.is_finite()
        || !frame.y.is_finite()
        || !frame.width.is_finite()
        || !frame.height.is_finite()
        || frame.width <= 0.0
        || frame.height <= 0.0
    {
        return None;
    }

    Some(CaptureGeometry {
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

fn frame_geometry(
    target: &CaptureTarget,
    fallback: &CaptureGeometry,
    sample: &CMSampleBuffer,
    screenshot: &Screenshot,
) -> CaptureGeometry {
    let mut geometry = target
        .window_id()
        .and_then(query_window_geometry)
        .unwrap_or_else(|| fallback.clone());

    // Bind logical dimensions to this sample rather than to a later window
    // observation. The WindowServer query remains authoritative for origin.
    if let Some(scale) = sample
        .scale_factor()
        .filter(|scale| *scale > 0.0 && scale.is_finite())
    {
        let width = (screenshot.width as f64 / scale).round() as usize;
        let height = (screenshot.height as f64 / scale).round() as usize;
        if width > 0 && height > 0 {
            geometry.width = width;
            geometry.height = height;
        }
    }
    geometry
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
    use super::{frame_is_usable, pack_bgra};
    use crate::screenshot::interface::{CaptureGeometry, CapturedFrame, Screenshot};

    fn captured_frame(sequence: u64, width: usize, height: usize) -> CapturedFrame {
        CapturedFrame {
            sequence,
            screenshot: Screenshot {
                raw: vec![0; width * height * 4],
                width,
                height,
            },
            geometry: CaptureGeometry {
                left: 0,
                top: 0,
                width,
                height,
            },
        }
    }

    #[test]
    fn rejects_cached_or_pre_resize_frames() {
        assert!(!frame_is_usable(
            &captured_frame(4, 200, 100),
            4,
            (200, 100)
        ));
        assert!(!frame_is_usable(&captured_frame(5, 100, 50), 4, (200, 100)));
        assert!(frame_is_usable(&captured_frame(5, 200, 100), 4, (200, 100)));
    }

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
