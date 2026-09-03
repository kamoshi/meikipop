// Modified from AuroraWright's OwOCR

use std::error::Error;
use std::fs;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::platform::interface::{CaptureGeometry, CapturedFrame, FrameProvider, Screenshot};
use ashpd::desktop::screencast::{
    CursorMode, Screencast, SelectSourcesOptions, SourceType, Stream,
};
use ashpd::desktop::{PersistMode, Session};
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::buffer::meta::MetaCursor;
use spa::pod::Pod;

use crate::platform::interface::{PointerProvider, PointerSnapshot};

#[derive(Clone)]
struct Frame {
    data: Arc<[u8]>,
    width: usize,
    height: usize,
    left: i32,
    top: i32,
    logical_width: usize,
    logical_height: usize,
}

#[derive(Default)]
struct CaptureState {
    frame: Option<Frame>,
    frame_sequence: u64,
    pointer: Option<(i32, i32)>,
    pointer_sequence: u64,
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
        state.frame_sequence = state.frame_sequence.wrapping_add(1);
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

    let cursor_modes = proxy
        .available_cursor_modes()
        .await
        .map_err(|error| format!("Failed to query ScreenCast cursor modes: {error}"))?;
    if !cursor_modes.contains(CursorMode::Metadata) {
        return Err("The Wayland ScreenCast portal does not support cursor metadata".to_owned());
    }

    let options = SelectSourcesOptions::default()
        .set_sources(ashpd::enumflags2::BitFlags::from_flag(SourceType::Monitor))
        .set_multiple(false)
        .set_cursor_mode(CursorMode::Metadata)
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

#[derive(Default)]
struct PipeWireUserData {
    format: spa::param::video::VideoInfoRaw,
}

fn format_parameter() -> Result<Vec<u8>, String> {
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::BGRx
        )
    );
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|result| result.0.into_inner())
    .map_err(|error| format!("Could not build PipeWire format parameter: {error}"))
}

fn cursor_meta_parameter() -> Result<Vec<u8>, String> {
    let meta_size = std::mem::size_of::<spa::sys::spa_meta_cursor>()
        + std::mem::size_of::<spa::sys::spa_meta_bitmap>();
    let cursor_size = |side: usize| (meta_size + side * side * 4) as i32;
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamMeta.as_raw(),
        id: spa::param::ParamType::Meta.as_raw(),
        properties: vec![
            spa::pod::Property::new(
                spa::sys::SPA_PARAM_META_type,
                spa::pod::Value::Id(spa::utils::Id(spa::sys::SPA_META_Cursor)),
            ),
            // Leave enough room for the metadata, bitmap descriptor, and the
            // compositor's conventional maximum 256x256 ARGB cursor image.
            spa::pod::Property::new(
                spa::sys::SPA_PARAM_META_size,
                spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
                    spa::utils::ChoiceFlags::empty(),
                    spa::utils::ChoiceEnum::Range {
                        default: cursor_size(64),
                        min: cursor_size(1),
                        max: cursor_size(256),
                    },
                ))),
            ),
        ],
    };
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|result| result.0.into_inner())
    .map_err(|error| format!("Could not build PipeWire cursor metadata parameter: {error}"))
}

fn update_cursor(
    shared: &SharedCapture,
    cursor: &MetaCursor,
    stream_position: (i32, i32),
    logical_size: (i32, i32),
    pixel_size: (u32, u32),
) {
    if !cursor.is_valid() || pixel_size.0 == 0 || pixel_size.1 == 0 {
        return;
    }
    let position = cursor.position();
    let x = stream_position.0
        + (i64::from(position.x) * i64::from(logical_size.0) / i64::from(pixel_size.0)) as i32;
    let y = stream_position.1
        + (i64::from(position.y) * i64::from(logical_size.1) / i64::from(pixel_size.1)) as i32;
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    state.pointer = Some((x, y));
    state.pointer_sequence = state.pointer_sequence.wrapping_add(1);
    shared.changed.notify_all();
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

    let stream_position = portal.stream.position().unwrap_or_default();
    let logical_size = portal
        .stream
        .size()
        .filter(|size| size.0 > 0 && size.1 > 0)
        .ok_or_else(|| "The Wayland portal returned invalid monitor geometry".to_owned())?;
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|error| format!("Could not create PipeWire main loop: {error}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|error| format!("Could not create PipeWire context: {error}"))?;
    let core = context
        .connect_fd_rc(portal.pipewire_fd, None)
        .map_err(|error| format!("Could not connect to portal PipeWire remote: {error}"))?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "meikipop-wayland-capture",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|error| format!("Could not create PipeWire stream: {error}"))?;

    let callback_shared = Arc::clone(&shared);
    let _listener = stream
        .add_local_listener_with_user_data(PipeWireUserData::default())
        .param_changed(|stream, user_data, id, param| {
            let Some(param) = param else { return };
            if id == spa::param::ParamType::Format.as_raw() {
                if let Err(error) = user_data.format.parse(param) {
                    log::error!("Could not parse PipeWire video format: {error}");
                    return;
                }
                match cursor_meta_parameter() {
                    Ok(bytes) => {
                        if let Some(meta) = Pod::from_bytes(&bytes) {
                            if let Err(error) = stream.update_params(&mut [meta]) {
                                log::error!("Could not request PipeWire cursor metadata: {error}");
                            }
                        }
                    }
                    Err(error) => {
                        log::error!("{error}");
                    }
                }
            }
        })
        .process(move |stream, user_data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };

            let pixel_size = user_data.format.size();
            if let Some(cursor) = buffer.find_meta::<MetaCursor>() {
                update_cursor(
                    &callback_shared,
                    cursor,
                    stream_position,
                    logical_size,
                    (pixel_size.width, pixel_size.height),
                );
            }

            // KDE and GNOME may send cursor-only buffers with no video data.
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let offset = data.chunk().offset() as usize;
            let size = data.chunk().size() as usize;
            let stride = data.chunk().stride();
            if size == 0 {
                return;
            }
            let Ok(stride) = usize::try_from(stride) else {
                callback_shared.fail(format!("Negative PipeWire video stride: {stride}"));
                return;
            };
            let Some(mapped) = data.data() else {
                callback_shared.fail("PipeWire returned an unmapped video buffer".to_owned());
                return;
            };
            let end = offset.saturating_add(size).min(mapped.len());
            let result = copy_strided_bgra(
                &mapped[offset..end],
                pixel_size.width as usize,
                pixel_size.height as usize,
                stride,
            );
            match result {
                Ok(data) => callback_shared.set_frame(Frame {
                    data: data.into(),
                    width: pixel_size.width as usize,
                    height: pixel_size.height as usize,
                    left: stream_position.0,
                    top: stream_position.1,
                    logical_width: logical_size.0.max(0) as usize,
                    logical_height: logical_size.1.max(0) as usize,
                }),
                Err(error) => callback_shared.fail(error),
            }
        })
        .register()
        .map_err(|error| format!("Could not register PipeWire listener: {error}"))?;

    let format = format_parameter()?;
    let mut params = [Pod::from_bytes(&format)
        .ok_or_else(|| "Could not parse PipeWire format parameter".to_owned())?];
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(portal.stream.pipe_wire_node_id()),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|error| format!("Could not connect PipeWire stream: {error}"))?;

    while !stop.load(Ordering::Acquire) {
        mainloop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(100)));
    }
    let _ = stream.disconnect();
    drop(stream);
    drop(core);
    drop(context);
    drop(mainloop);

    let mut result = Ok(());
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

    fn latest_frame(&self) -> Option<(u64, Frame)> {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Some((state.frame_sequence, state.frame.clone()?))
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

    fn request_frame(&self) -> Result<(u64, Frame), Box<dyn Error>> {
        self.capture
            .latest_frame()
            .ok_or_else(|| "Invalid frame received".into())
    }
}

/// Captures the single source selected through the desktop ScreenCast portal.
pub struct WaylandFrameProvider {
    screencast: ScreenCastManager,
}

pub struct WaylandPointerProvider {
    shared: Arc<SharedCapture>,
}

impl WaylandPointerProvider {
    pub(super) fn pipewire_observation(&self) -> Option<((i32, i32), u64)> {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Some((state.pointer?, state.pointer_sequence))
    }

    pub(super) fn snapshot_with_position_override(
        &self,
        position_override: Option<(i32, i32)>,
    ) -> Result<PointerSnapshot, Box<dyn Error>> {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let position = position_override
            .or(state.pointer)
            .ok_or("Neither X11 nor the Wayland screencast provided cursor coordinates")?;
        let frame = state
            .frame
            .as_ref()
            .ok_or("The Wayland screencast has not provided a frame yet")?;
        Ok(PointerSnapshot {
            position,
            capture_geometry: Some(CaptureGeometry {
                top: frame.top,
                left: frame.left,
                width: frame.logical_width,
                height: frame.logical_height,
            }),
            source_generation: 1,
            target_available: true,
        })
    }
}

impl PointerProvider for WaylandPointerProvider {
    fn snapshot(&mut self) -> Result<PointerSnapshot, Box<dyn Error>> {
        self.snapshot_with_position_override(None)
    }
}

impl WaylandFrameProvider {
    pub fn new(token_path: String) -> Result<Self, Box<dyn Error>> {
        let screencast = ScreenCastManager::new(token_path)?;
        screencast.wait_until_ready()?;
        Ok(Self { screencast })
    }

    pub fn pointer_provider(&self) -> WaylandPointerProvider {
        WaylandPointerProvider {
            shared: Arc::clone(&self.screencast.capture.shared),
        }
    }

    fn selected_geometry(&self) -> Result<CaptureGeometry, Box<dyn Error>> {
        let (_, frame) = self.screencast.request_frame()?;
        Ok(CaptureGeometry {
            top: frame.top,
            left: frame.left,
            width: frame.logical_width,
            height: frame.logical_height,
        })
    }

    fn latest_captured_frame(&self) -> Result<CapturedFrame, Box<dyn Error>> {
        let (sequence, frame) = self.screencast.request_frame()?;
        Ok(CapturedFrame {
            source_generation: 1,
            sequence,
            geometry: CaptureGeometry {
                top: frame.top,
                left: frame.left,
                width: frame.logical_width,
                height: frame.logical_height,
            },
            screenshot: Screenshot {
                raw: Arc::clone(&frame.data),
                width: frame.width,
                height: frame.height,
            },
        })
    }
}

impl FrameProvider for WaylandFrameProvider {
    fn capture_geometry(&mut self) -> Result<Option<CaptureGeometry>, Box<dyn Error>> {
        self.selected_geometry().map(Some)
    }

    fn capture_frame(&mut self) -> Result<Option<CapturedFrame>, Box<dyn Error>> {
        self.latest_captured_frame().map(Some)
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
