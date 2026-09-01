// Modified from AuroraWright's OwOCR

use std::error::Error;
use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::crop_bgra_impl;
use crate::screenshot::interface::{FrameProvider, Monitor, Screenshot};
use ashpd::desktop::screencast::{Screencast, SelectSourcesOptions, SourceType, Stream};
use ashpd::desktop::{PersistMode, Session};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::VideoFrameExt;

#[derive(Clone)]
struct Frame {
    data: Arc<Vec<u8>>,
    width: usize,
    height: usize,
    left: i32,
    top: i32,
}

#[derive(Default)]
struct CaptureState {
    frame: Option<Frame>,
    error: Option<String>,
    stopped: bool,
}

#[derive(Default)]
struct SharedCapture {
    state: Mutex<CaptureState>,
    changed: Condvar,
}

impl SharedCapture {
    fn set_frame(&self, frame: Frame) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.frame = Some(frame);
        self.changed.notify_all();
    }

    fn fail(&self, error: String) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.error.is_none() {
            state.error = Some(error);
        }
        state.stopped = true;
        self.changed.notify_all();
    }

    fn mark_stopped(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.stopped = true;
        self.changed.notify_all();
    }

    fn wait_ready(&self, timeout: Duration) -> Result<(), WaitError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());

        loop {
            if state.frame.is_some() {
                return Ok(());
            }
            if let Some(error) = &state.error {
                return Err(WaitError::Failed(error.clone()));
            }
            if state.stopped {
                return Err(WaitError::Failed(
                    "Wayland capture stopped before receiving a frame".to_owned(),
                ));
            }

            let now = Instant::now();
            if now >= deadline {
                return Err(WaitError::TimedOut);
            }

            let remaining = deadline.saturating_duration_since(now);
            let (next_state, result) = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner());
            state = next_state;
            if result.timed_out() && state.frame.is_none() && state.error.is_none() {
                return Err(WaitError::TimedOut);
            }
        }
    }
}

enum WaitError {
    Failed(String),
    TimedOut,
}

struct PortalCapture {
    proxy: Screencast,
    session: Session<Screencast>,
    stream: Stream,
    pipewire_fd: OwnedFd,
}

async fn open_portal(token_path: &Path) -> Result<PortalCapture, String> {
    let proxy = Screencast::new()
        .await
        .map_err(|error| format!("Failed to connect to ScreenCast portal: {error}"))?;
    let session = proxy
        .create_session(Default::default())
        .await
        .map_err(|error| format!("Failed to create ScreenCast session: {error}"))?;

    let restore_token = fs::read_to_string(token_path)
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty());

    let options = SelectSourcesOptions::default()
        .set_sources(ashpd::enumflags2::BitFlags::from_flag(SourceType::Monitor))
        .set_multiple(false)
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(restore_token.as_deref());

    proxy
        .select_sources(&session, options)
        .await
        .and_then(|request| request.response())
        .map_err(|error| format!("Failed to select ScreenCast source: {error}"))?;

    let response = proxy
        .start(&session, None, Default::default())
        .await
        .and_then(|request| request.response())
        .map_err(|error| format!("Failed to start ScreenCast session: {error}"))?;

    let stream = response
        .streams()
        .first()
        .cloned()
        .ok_or_else(|| "ScreenCast portal returned no streams".to_owned())?;

    if let Some(token) = response.restore_token() {
        if let Some(parent) = token_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Failed to create ScreenCast token directory: {error}"))?;
        }
        fs::write(token_path, token)
            .map_err(|error| format!("Failed to save ScreenCast restore token: {error}"))?;
    }

    let pipewire_fd = proxy
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(|error| format!("Failed to open PipeWire remote: {error}"))?;

    Ok(PortalCapture {
        proxy,
        session,
        stream,
        pipewire_fd,
    })
}

fn copy_strided_bgra(
    data: &[u8],
    width: usize,
    height: usize,
    stride: usize,
) -> Result<Vec<u8>, String> {
    let row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| "Captured frame width is too large".to_owned())?;
    if stride < row_bytes {
        return Err(format!(
            "Captured frame stride {stride} is smaller than its {row_bytes}-byte row"
        ));
    }
    let required = stride
        .checked_mul(height)
        .ok_or_else(|| "Captured frame dimensions are too large".to_owned())?;
    if data.len() < required {
        return Err(format!(
            "Captured frame has {} bytes, but its dimensions and stride require {required}",
            data.len()
        ));
    }

    if stride == row_bytes {
        return Ok(data[..row_bytes * height].to_vec());
    }

    let mut packed = Vec::with_capacity(row_bytes * height);
    for row in data[..required].chunks_exact(stride) {
        packed.extend_from_slice(&row[..row_bytes]);
    }
    Ok(packed)
}

fn process_sample(sample: &gst::Sample) -> Result<Frame, String> {
    let caps = sample
        .caps()
        .ok_or_else(|| "GStreamer sample has no caps".to_owned())?;
    let info = gst_video::VideoInfo::from_caps(caps)
        .map_err(|error| format!("Invalid GStreamer video caps: {error}"))?;

    if !matches!(
        info.format(),
        gst_video::VideoFormat::Bgra | gst_video::VideoFormat::Bgrx
    ) {
        return Err(format!(
            "Expected a BGRA or BGRx frame, received {}",
            info.format()
        ));
    }

    let buffer = sample
        .buffer()
        .ok_or_else(|| "GStreamer sample has no buffer".to_owned())?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(|error| format!("Could not map GStreamer video frame: {error}"))?;
    let stride = frame
        .plane_stride()
        .first()
        .copied()
        .ok_or_else(|| "GStreamer video frame has no plane stride".to_owned())?;
    let stride = usize::try_from(stride)
        .map_err(|_| format!("Negative GStreamer video stride is unsupported: {stride}"))?;
    let data = frame
        .plane_data(0)
        .map_err(|error| format!("Could not access GStreamer video plane: {error}"))?;
    let width = frame.width() as usize;
    let height = frame.height() as usize;
    let data = copy_strided_bgra(data, width, height, stride)?;

    Ok(Frame {
        data: Arc::new(data),
        width,
        height,
        left: 0,
        top: 0,
    })
}

fn build_pipeline(
    pipewire_fd: i32,
    node_id: u32,
    position: (i32, i32),
    shared: Arc<SharedCapture>,
) -> Result<gst::Pipeline, String> {
    gst::init().map_err(|error| format!("Failed to initialize GStreamer: {error}"))?;

    let source = gst::ElementFactory::make("pipewiresrc")
        .property("fd", pipewire_fd)
        .property("path", node_id.to_string())
        .build()
        .map_err(|error| format!("Could not create pipewiresrc: {error}"))?;
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|error| format!("Could not create videoconvert: {error}"))?;
    let rate = gst::ElementFactory::make("videorate")
        .property("drop-only", true)
        .build()
        .map_err(|error| format!("Could not create videorate: {error}"))?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", gst::List::new(["BGRA", "BGRx"]))
        .field("max-framerate", gst::Fraction::new(30, 1))
        .build();
    let caps_filter = gst::ElementFactory::make("capsfilter")
        .property("caps", caps)
        .build()
        .map_err(|error| format!("Could not create GStreamer caps filter: {error}"))?;
    let app_sink = gst::ElementFactory::make("appsink")
        .property("max-buffers", 1u32)
        .property("drop", true)
        .property("enable-last-sample", false)
        .property("qos", false)
        .property("sync", false)
        .property("wait-on-eos", false)
        .build()
        .map_err(|error| format!("Could not create GStreamer appsink: {error}"))?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| "Created GStreamer sink is not an appsink".to_owned())?;

    let callback_shared = Arc::clone(&shared);
    app_sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let result = sink
                    .pull_sample()
                    .map_err(|_| "Could not pull GStreamer sample".to_owned())
                    .and_then(|sample| process_sample(&sample));
                match result {
                    Ok(mut frame) => {
                        frame.left = position.0;
                        frame.top = position.1;
                        callback_shared.set_frame(frame);
                        Ok(gst::FlowSuccess::Ok)
                    }
                    Err(error) => {
                        callback_shared.fail(error);
                        Err(gst::FlowError::Error)
                    }
                }
            })
            .build(),
    );

    let pipeline = gst::Pipeline::default();
    pipeline
        .add_many([
            &source,
            &convert,
            &rate,
            &caps_filter,
            app_sink.upcast_ref(),
        ])
        .map_err(|error| format!("Could not assemble GStreamer pipeline: {error}"))?;
    gst::Element::link_many([
        &source,
        &convert,
        &rate,
        &caps_filter,
        app_sink.upcast_ref(),
    ])
    .map_err(|error| format!("Could not link GStreamer pipeline: {error}"))?;

    Ok(pipeline)
}

fn gstreamer_error(message: &gst::MessageRef) -> Option<String> {
    match message.view() {
        gst::MessageView::Error(error) => {
            let source = message
                .src()
                .map(|source| source.path_string().to_string())
                .unwrap_or_else(|| "unknown GStreamer element".to_owned());
            let debug = error.debug().unwrap_or_default();
            Some(format!(
                "GStreamer error from {source}: {} ({debug})",
                error.error()
            ))
        }
        gst::MessageView::Eos(_) => Some("PipeWire stream ended".to_owned()),
        _ => None,
    }
}

fn run_capture(
    shared: Arc<SharedCapture>,
    stop: Arc<AtomicBool>,
    token_path: PathBuf,
) -> Result<(), String> {
    let portal = async_io::block_on(open_portal(&token_path))?;
    log::debug!(
        "Selected PipeWire stream {} at {:?} with size {:?}",
        portal.stream.pipe_wire_node_id(),
        portal.stream.position(),
        portal.stream.size()
    );

    let pipeline = build_pipeline(
        portal.pipewire_fd.as_raw_fd(),
        portal.stream.pipe_wire_node_id(),
        portal.stream.position().unwrap_or_default(),
        Arc::clone(&shared),
    )?;
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|error| format!("Could not start GStreamer pipeline: {error}"))?;

    let bus = pipeline
        .bus()
        .ok_or_else(|| "GStreamer pipeline has no bus".to_owned())?;
    let mut result = Ok(());
    while !stop.load(Ordering::Acquire) {
        if let Some(message) = bus.timed_pop_filtered(
            gst::ClockTime::from_mseconds(100),
            &[gst::MessageType::Error, gst::MessageType::Eos],
        ) {
            if let Some(error) = gstreamer_error(&message) {
                result = Err(error);
                break;
            }
        }
    }

    if let Err(error) = pipeline.set_state(gst::State::Null) {
        if result.is_ok() {
            result = Err(format!("Could not stop GStreamer pipeline: {error}"));
        }
    }
    if let Err(error) = async_io::block_on(portal.session.close()) {
        if result.is_ok() {
            result = Err(format!("Could not close ScreenCast session: {error}"));
        }
    }

    // Keep the portal proxy and its D-Bus connection alive until after closing
    // the session. Dropping the owned descriptor then closes it exactly once.
    drop(portal.proxy);
    result
}

/// A conservative one-stream Wayland ScreenCast capture backend.
pub struct WaylandCapture {
    shared: Arc<SharedCapture>,
    stop: Arc<AtomicBool>,
}

impl WaylandCapture {
    fn start(token_path: String) -> Result<Self, String> {
        if token_path.is_empty() {
            return Err("token_path must not be empty".to_owned());
        }

        let shared = Arc::new(SharedCapture::default());
        let stop = Arc::new(AtomicBool::new(false));
        let worker_shared = Arc::clone(&shared);
        let worker_stop = Arc::clone(&stop);
        let token_path = PathBuf::from(token_path);
        thread::Builder::new()
            .name("meikipop-wayland-capture".to_owned())
            .spawn(
                move || match run_capture(Arc::clone(&worker_shared), worker_stop, token_path) {
                    Ok(()) => worker_shared.mark_stopped(),
                    Err(error) => worker_shared.fail(error),
                },
            )
            .map_err(|error| format!("Could not start Wayland capture thread: {error}"))?;

        Ok(Self { shared, stop })
    }

    fn wait_until_ready(&self, timeout: Duration) -> Result<(), WaitError> {
        self.shared.wait_ready(timeout)
    }

    fn latest_frame(&self) -> Option<Frame> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .frame
            .clone()
    }
}

impl Drop for WaylandCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.shared.changed.notify_all();
    }
}

pub struct ScreenCastManager {
    capture: WaylandCapture,
}

impl ScreenCastManager {
    pub fn new(token_path: String) -> Result<Self, Box<dyn Error>> {
        log::info!("Using Rust Wayland ScreenCast backend");
        Ok(Self {
            capture: WaylandCapture::start(token_path)?,
        })
    }

    pub fn wait_until_ready(&self) -> Result<(), Box<dyn Error>> {
        match self.capture.wait_until_ready(Duration::from_secs(63)) {
            Ok(()) => Ok(()),
            Err(WaitError::Failed(error)) => Err(error.into()),
            Err(WaitError::TimedOut) => Err("Timed out waiting for the first Wayland frame".into()),
        }
    }

    fn request_frame(&self) -> Result<Frame, Box<dyn Error>> {
        self.capture
            .latest_frame()
            .ok_or_else(|| "Invalid frame received".into())
    }
}

pub struct MssWaylandShim {
    screencast: ScreenCastManager,
    monitors: Vec<Monitor>,
}

impl MssWaylandShim {
    pub fn new(token_path: String) -> Result<Self, Box<dyn Error>> {
        let screencast = ScreenCastManager::new(token_path)?;
        screencast.wait_until_ready()?;
        let mut shim = Self {
            screencast,
            monitors: Vec::new(),
        };
        shim._create_monitors()?;
        Ok(shim)
    }

    fn _create_monitors(&mut self) -> Result<(), Box<dyn Error>> {
        self.monitors = Vec::new();

        let frame = self.screencast.request_frame()?;
        let fake_monitor = Monitor {
            top: frame.top,
            left: frame.left,
            width: frame.width,
            height: frame.height,
        };

        // Match mss: monitor 0 is the virtual desktop and physical monitors
        // start at index 1. The portal provides one selected stream.
        self.monitors.push(fake_monitor.clone());
        self.monitors.push(fake_monitor);
        Ok(())
    }

    fn _grab_screenshot(&self, monitor: &Monitor) -> Result<Screenshot, Box<dyn Error>> {
        let frame = self.screencast.request_frame()?;
        let (raw, width, height) = crop_bgra_impl(
            frame.data.as_slice(),
            frame.width,
            frame.height,
            (monitor.left - frame.left) as i64,
            (monitor.top - frame.top) as i64,
            monitor.width as i64,
            monitor.height as i64,
        )?;
        Ok(Screenshot { raw, width, height })
    }
}

impl FrameProvider for MssWaylandShim {
    fn monitors(&mut self) -> Result<Vec<Monitor>, Box<dyn Error>> {
        Ok(self.monitors.clone())
    }

    fn frame(&mut self, monitor: &Monitor) -> Result<Screenshot, Box<dyn Error>> {
        self._grab_screenshot(monitor)
    }
}

#[cfg(test)]
mod tests {
    use super::copy_strided_bgra;

    #[test]
    fn removes_row_padding() {
        let data = [
            1, 2, 3, 4, 5, 6, 7, 8, 99, 99, 99, 99, 9, 10, 11, 12, 13, 14, 15, 16, 88, 88, 88, 88,
        ];
        let packed = copy_strided_bgra(&data, 2, 2, 12).unwrap();
        assert_eq!(
            packed,
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
    }

    #[test]
    fn rejects_a_short_stride() {
        let error = copy_strided_bgra(&[0; 8], 2, 1, 4).unwrap_err();
        assert!(error.contains("smaller"));
    }
}
