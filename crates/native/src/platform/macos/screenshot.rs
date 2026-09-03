use std::error::Error;
use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use screencapturekit::SCFrameStatus;
use screencapturekit::cg::CGRect as SCKRect;
use screencapturekit::content_sharing_picker::{
    SCContentSharingPicker, SCContentSharingPickerConfiguration, SCContentSharingPickerMode,
    SCPickerOutcome,
};
use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;

use crate::platform::interface::{CaptureGeometry, CapturedFrame, FrameProvider, Screenshot};

use super::SharedCaptureSource;
use super::window_server::query_window_geometry;

const FRAME_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum StreamState {
    #[default]
    Starting,
    Streaming,
    Suspended,
    Stopped,
}

#[derive(Default)]
struct CaptureState {
    frame: Option<CapturedFrame>,
    frame_sequence: u64,
    stream_state: StreamState,
}

#[derive(Default)]
struct SharedCapture {
    state: Mutex<CaptureState>,
    changed: Condvar,
}

pub struct ScreenCaptureKitFrameProvider {
    geometry: Option<CaptureGeometry>,
    filter: Option<SCContentFilter>,
    pixel_size: Option<(u32, u32)>,
    target: Option<CaptureTarget>,
    stream: Option<SCStream>,
    shared: Arc<SharedCapture>,
    source: SharedCaptureSource,
    source_generation: u64,
    last_geometry_check: Instant,
}

impl ScreenCaptureKitFrameProvider {
    pub(crate) fn new(source: SharedCaptureSource) -> Result<Self, Box<dyn Error>> {
        log::info!("Using macOS ScreenCaptureKit backend");
        let picked = pick_source()?;
        let source_generation = {
            let mut state = source.lock().unwrap_or_else(|error| error.into_inner());
            state.generation = state.generation.wrapping_add(1).max(1);
            if let Some(picked) = &picked {
                state.geometry = Some(picked.geometry.clone());
                state.window_id = picked.target.window_id();
                // Selection and frame availability are separate states. The
                // first content-bearing sample makes the source available.
                state.available = false;
            } else {
                state.geometry = None;
                state.window_id = None;
                state.available = false;
            }
            state.generation
        };

        let (filter, geometry, pixel_size, target) = match picked {
            Some(picked) => (
                Some(picked.filter),
                Some(picked.geometry),
                Some(picked.pixel_size),
                Some(picked.target),
            ),
            None => (None, None, None, None),
        };

        Ok(Self {
            geometry,
            filter,
            pixel_size,
            target,
            stream: None,
            shared: Arc::new(SharedCapture::default()),
            source,
            source_generation,
            last_geometry_check: Instant::now(),
        })
    }

    fn refresh_window_geometry(&mut self) {
        let Some(window_id) = self.target.as_ref().and_then(CaptureTarget::window_id) else {
            return;
        };
        let now = Instant::now();
        if now.duration_since(self.last_geometry_check) < Duration::from_millis(200) {
            return;
        }
        self.last_geometry_check = now;

        let Some(live) = query_window_geometry(window_id) else {
            return;
        };

        // WindowServer is authoritative for desktop position, while the
        // captured frame remains authoritative for logical dimensions.
        if let Some(geometry) = &mut self.geometry {
            geometry.left = live.left;
            geometry.top = live.top;
        }
        self.update_shared_origin(live.left, live.top);

        let Some(filter) = &self.filter else {
            return;
        };
        let Some(pixel_size) = self.pixel_size else {
            return;
        };
        let scale = f64::from(filter.point_pixel_scale());
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            pixel_size.0 as f64 / live.width as f64
        };
        let requested_size = (
            (live.width as f64 * scale).round() as u32,
            (live.height as f64 * scale).round() as u32,
        );
        if requested_size.0 == 0 || requested_size.1 == 0 || Some(requested_size) == self.pixel_size
        {
            return;
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
                return;
            }
        }

        self.pixel_size = Some(requested_size);
    }

    fn update_shared_origin(&self, left: i32, top: i32) {
        let mut source = self
            .source
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if source.generation == self.source_generation
            && let Some(geometry) = &mut source.geometry
        {
            geometry.left = left;
            geometry.top = top;
        }
    }

    fn release_source(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.stop_capture();
        }
        self.filter = None;
        self.geometry = None;
        self.pixel_size = None;
        self.target = None;
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.frame = None;
            state.stream_state = StreamState::Starting;
        }
        let mut source = self
            .source
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if source.generation == self.source_generation {
            source.generation = source.generation.wrapping_add(1).max(1);
            source.geometry = None;
            source.window_id = None;
            source.available = false;
            self.source_generation = source.generation;
        }
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
        }

        let pixel_size = self
            .pixel_size
            .ok_or("No capture source is currently shared")?;
        let filter = self
            .filter
            .as_ref()
            .ok_or("No capture source is currently shared")?;
        let target = self
            .target
            .as_ref()
            .ok_or("No capture source is currently shared")?;
        let fallback_geometry = self
            .geometry
            .as_ref()
            .ok_or("No capture source is currently shared")?;
        let config = stream_configuration(pixel_size);

        let shared = Arc::clone(&self.shared);
        let target = target.clone();
        let fallback_geometry = fallback_geometry.clone();
        let source_generation = self.source_generation;
        let source = Arc::clone(&self.source);
        let mut stream = SCStream::new(filter, &config);
        let handler_id = stream.add_output_handler(
            move |sample: CMSampleBuffer, output_type: SCStreamOutputType| {
                if output_type != SCStreamOutputType::Screen {
                    return;
                }

                let frame_status = sample.frame_status();
                match frame_status {
                    Some(SCFrameStatus::Idle) => {
                        // Static content is still a valid capture source. Keep
                        // the last complete frame available for cursor-driven
                        // OCR rather than interpreting idleness as revocation.
                        return;
                    }
                    Some(
                        SCFrameStatus::Blank | SCFrameStatus::Suspended | SCFrameStatus::Stopped,
                    ) => {
                        let stopped = frame_status == Some(SCFrameStatus::Stopped);
                        let mut state = shared
                            .state
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        state.frame = None;
                        state.stream_state = if stopped {
                            StreamState::Stopped
                        } else {
                            StreamState::Suspended
                        };
                        drop(state);
                        set_source_available(&source, source_generation, false);
                        shared.changed.notify_all();
                        return;
                    }
                    Some(SCFrameStatus::Complete | SCFrameStatus::Started) | None => {}
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
                        // `Stopped` is terminal for this one-shot 1.4 picker
                        // session. Ignore any callback already queued behind
                        // the terminal status rather than resurrecting it.
                        if state.stream_state == StreamState::Stopped {
                            return;
                        }
                        let shared_geometry = geometry.clone();
                        state.frame_sequence = state.frame_sequence.wrapping_add(1);
                        let sequence = state.frame_sequence;
                        state.frame = Some(CapturedFrame {
                            source_generation,
                            sequence,
                            screenshot,
                            geometry,
                        });
                        state.stream_state = StreamState::Streaming;
                        drop(state);
                        update_source_from_frame(&source, source_generation, shared_geometry);
                    }
                    Err(error) => {
                        // One malformed buffer says nothing about the stream's
                        // lifecycle. Preserve the last complete frame and let
                        // the next content-bearing sample recover naturally.
                        drop(state);
                        log::debug!("Ignoring unusable ScreenCaptureKit frame: {error}");
                    }
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

    fn next_frame(
        &self,
        expected_pixel_size: (u32, u32),
    ) -> Result<Option<CapturedFrame>, Box<dyn Error>> {
        let deadline = Instant::now() + FRAME_TIMEOUT;
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        loop {
            if let Some(frame) = &state.frame
                && frame_matches_pixel_size(frame, expected_pixel_size)
            {
                // Pixel storage is reference-counted, so this retains the last
                // complete frame without copying it. Reusing it is essential
                // when ScreenCaptureKit emits only idle samples for unchanged
                // content after the pointer moves.
                return Ok(Some(frame.clone()));
            }
            if matches!(
                state.stream_state,
                StreamState::Suspended | StreamState::Stopped
            ) {
                return Ok(None);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
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
                    .is_none_or(|frame| !frame_matches_pixel_size(frame, expected_pixel_size))
            {
                return Ok(None);
            }
        }
    }
}

impl FrameProvider for ScreenCaptureKitFrameProvider {
    fn capture_geometry(&mut self) -> Result<Option<CaptureGeometry>, Box<dyn Error>> {
        self.refresh_window_geometry();
        Ok(self
            .source
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .geometry
            .clone())
    }

    fn capture_frame(&mut self) -> Result<Option<CapturedFrame>, Box<dyn Error>> {
        let Some(pixel_size) = self.pixel_size else {
            return Ok(None);
        };
        let result = (|| {
            self.ensure_stream()?;
            self.refresh_window_geometry();
            let expected_size = self.pixel_size.unwrap_or(pixel_size);
            self.next_frame(expected_size)
        })();
        match result {
            Ok(Some(frame)) => {
                self.geometry = Some(frame.geometry.clone());
                Ok(Some(frame))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                // A timeout does not mean that the user's selection was
                // revoked. ScreenCaptureKit reports suspension and termination
                // through frame status, so keep this stream recoverable.
                Err(error)
            }
        }
    }
}

fn update_source_from_frame(
    source: &SharedCaptureSource,
    generation: u64,
    geometry: CaptureGeometry,
) {
    let mut state = source.lock().unwrap_or_else(|error| error.into_inner());
    if state.generation == generation {
        state.geometry = Some(geometry);
        state.available = true;
    }
}

fn set_source_available(source: &SharedCaptureSource, generation: u64, available: bool) {
    let mut state = source.lock().unwrap_or_else(|error| error.into_inner());
    if state.generation == generation {
        state.available = available;
    }
}

fn frame_matches_pixel_size(frame: &CapturedFrame, expected_pixel_size: (u32, u32)) -> bool {
    frame.screenshot.width == expected_pixel_size.0 as usize
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

fn pick_source() -> Result<Option<PickedSource>, Box<dyn Error>> {
    let mut configuration = SCContentSharingPickerConfiguration::new();
    configuration.set_allowed_picker_modes(&[
        SCContentSharingPickerMode::SingleDisplay,
        SCContentSharingPickerMode::SingleWindow,
    ]);
    configuration.set_allows_changing_selected_content(false);

    // TODO(screencapturekit-9): Replace this one-shot `show` session with the repeating
    // `SCContentSharingPicker::add_observer` subscription from screencapturekit 9.0. Retaining
    // that subscription will let the app remain visible in Control Center while idle and accept
    // a new source after the user stops sharing, without rebuilding the OCR pipeline.

    let (sender, receiver) = mpsc::sync_channel(1);
    SCContentSharingPicker::show(&configuration, move |outcome| {
        let _ = sender.send(outcome);
    });

    let result = match receiver.recv() {
        Ok(SCPickerOutcome::Picked(result)) => result,
        Ok(SCPickerOutcome::Cancelled) => return Ok(None),
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
    Ok(Some(PickedSource {
        filter: result.filter(),
        geometry,
        pixel_size,
        target,
    }))
}

impl Drop for ScreenCaptureKitFrameProvider {
    fn drop(&mut self) {
        self.release_source();
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
    Ok(Screenshot {
        raw: raw.into(),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use super::super::{CaptureSourceSnapshot, new_shared_capture_source};
    use super::{
        ScreenCaptureKitFrameProvider, SharedCapture, StreamState, frame_matches_pixel_size,
        pack_bgra, set_source_available,
    };
    use crate::platform::interface::{CaptureGeometry, CapturedFrame, Screenshot};

    fn captured_frame(sequence: u64, width: usize, height: usize) -> CapturedFrame {
        CapturedFrame {
            source_generation: 1,
            sequence,
            screenshot: Screenshot {
                raw: vec![0; width * height * 4].into(),
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
    fn rejects_pre_resize_frames() {
        assert!(!frame_matches_pixel_size(
            &captured_frame(5, 100, 50),
            (200, 100)
        ));
        assert!(frame_matches_pixel_size(
            &captured_frame(5, 200, 100),
            (200, 100)
        ));
    }

    #[test]
    fn removes_screen_capture_row_padding() {
        let data = [
            1, 2, 3, 4, 5, 6, 7, 8, 99, 99, 99, 99, 9, 10, 11, 12, 13, 14, 15, 16, 88, 88, 88, 88,
        ];
        let screenshot = pack_bgra(&data, 2, 2, 12).unwrap();
        assert_eq!((screenshot.width, screenshot.height), (2, 2));
        assert_eq!(
            screenshot.raw.as_ref(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
    }

    #[test]
    fn availability_updates_cannot_cross_source_generations() {
        let source = Arc::new(Mutex::new(CaptureSourceSnapshot {
            generation: 2,
            available: false,
            ..CaptureSourceSnapshot::default()
        }));

        set_source_available(&source, 1, true);
        assert!(!source.lock().unwrap().available);

        set_source_available(&source, 2, true);
        assert!(source.lock().unwrap().available);
    }

    #[test]
    fn suspended_capture_returns_idle_without_waiting_for_a_timeout() {
        let provider = provider_with_state(StreamState::Suspended, None);

        assert!(provider.next_frame((10, 10)).unwrap().is_none());
    }

    #[test]
    fn delivering_a_frame_retains_a_cheap_cached_copy() {
        let provider = provider_with_state(StreamState::Streaming, Some(captured_frame(1, 10, 10)));

        let delivered = provider.next_frame((10, 10)).unwrap();
        assert!(delivered.is_some());
        assert!(provider.shared.state.lock().unwrap().frame.is_some());
    }

    fn provider_with_state(
        stream_state: StreamState,
        frame: Option<CapturedFrame>,
    ) -> ScreenCaptureKitFrameProvider {
        let shared = Arc::new(SharedCapture::default());
        {
            let mut state = shared.state.lock().unwrap();
            state.stream_state = stream_state;
            state.frame = frame;
        }
        ScreenCaptureKitFrameProvider {
            geometry: None,
            filter: None,
            pixel_size: None,
            target: None,
            stream: None,
            shared,
            source: new_shared_capture_source(),
            source_generation: 1,
            last_geometry_check: Instant::now(),
        }
    }
}
