use std::error::Error;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::rust_connection::RustConnection;

use super::wayland::WaylandPointerProvider;
use crate::platform::interface::{PointerProvider, PointerSnapshot};

const SWITCH_STREAK_TIMEOUT: Duration = Duration::from_millis(300);
const REQUIRED_POSITION_CHANGES: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionSource {
    PipeWire,
    X11,
}

#[derive(Clone, Debug)]
struct CandidateTracker {
    last_position: Option<(i32, i32)>,
    last_change_time: Option<Instant>,
    change_count: usize,
}

impl CandidateTracker {
    fn new() -> Self {
        Self {
            last_position: None,
            last_change_time: None,
            change_count: 0,
        }
    }

    fn reset(&mut self) {
        self.last_position = None;
        self.last_change_time = None;
        self.change_count = 0;
    }

    /// Records a candidate position from an inactive source. Returns true if the
    /// position has changed enough distinct times within the timeout window to
    /// confirm intentional motion.
    fn register_position(&mut self, pos: (i32, i32), now: Instant) -> bool {
        match self.last_position {
            Some(last) if last == pos => {
                if let Some(time) = self.last_change_time {
                    if now.saturating_duration_since(time) > SWITCH_STREAK_TIMEOUT {
                        self.change_count = 0;
                    }
                }
                false
            }
            Some(_) => {
                let is_recent = self.last_change_time.is_some_and(|time| {
                    now.saturating_duration_since(time) <= SWITCH_STREAK_TIMEOUT
                });
                if is_recent {
                    self.change_count += 1;
                } else {
                    self.change_count = 1;
                }
                self.last_position = Some(pos);
                self.last_change_time = Some(now);
                self.change_count >= REQUIRED_POSITION_CHANGES
            }
            None => {
                self.last_position = Some(pos);
                self.last_change_time = Some(now);
                self.change_count = 0;
                false
            }
        }
    }
}

#[derive(Debug)]
struct LinuxPointerState {
    active_source: PositionSource,
    x11_tracker: CandidateTracker,
    pipewire_tracker: CandidateTracker,
}

impl LinuxPointerState {
    fn new() -> Self {
        Self {
            active_source: PositionSource::PipeWire,
            x11_tracker: CandidateTracker::new(),
            pipewire_tracker: CandidateTracker::new(),
        }
    }

    fn update(
        &mut self,
        pipewire: Option<(i32, i32)>,
        x11: Result<(i32, i32), Box<dyn Error>>,
        now: Instant,
    ) -> Result<((i32, i32), PositionSource), Box<dyn Error>> {
        // Feed the inactive source into its candidate tracker to detect intentional motion.
        match self.active_source {
            PositionSource::PipeWire => {
                if let Ok(x11_pos) = x11.as_ref().copied() {
                    if self.x11_tracker.register_position(x11_pos, now) {
                        log::info!(
                            "Switching cursor source from PipeWire to X11 after active motion: {x11_pos:?}"
                        );
                        self.active_source = PositionSource::X11;
                        self.x11_tracker.reset();
                        self.pipewire_tracker.reset();
                    }
                }
            }
            PositionSource::X11 => {
                if let Some(pw_pos) = pipewire {
                    if self.pipewire_tracker.register_position(pw_pos, now) {
                        log::info!(
                            "Switching cursor source from X11 to PipeWire after active motion: {pw_pos:?}"
                        );
                        self.active_source = PositionSource::PipeWire;
                        self.x11_tracker.reset();
                        self.pipewire_tracker.reset();
                    }
                }
            }
        }

        // Return coordinates from the active source, falling back to the other if the active one has no data.
        match self.active_source {
            PositionSource::PipeWire => {
                if let Some(pw_pos) = pipewire {
                    Ok((pw_pos, PositionSource::PipeWire))
                } else {
                    x11.map(|pos| (pos, PositionSource::X11)).map_err(|error| {
                        format!("PipeWire has not provided cursor coordinates and X11 fallback failed: {error}")
                            .into()
                    })
                }
            }
            PositionSource::X11 => match x11 {
                Ok(x11_pos) => Ok((x11_pos, PositionSource::X11)),
                Err(error) => {
                    if let Some(pw_pos) = pipewire {
                        Ok((pw_pos, PositionSource::PipeWire))
                    } else {
                        Err(format!(
                            "X11 cursor query failed and PipeWire has no coordinates: {error}"
                        )
                        .into())
                    }
                }
            },
        }
    }
}

/// Uses cursor metadata supplied by PipeWire and falls back to X11 (including
/// XWayland) coordinates. Capture geometry always comes from PipeWire so the
/// position and captured frame share desktop coordinates.
pub struct LinuxPointerProvider {
    x11: Option<RustConnection>,
    pipewire: WaylandPointerProvider,
    state: LinuxPointerState,
    last_source: Option<PositionSource>,
    last_position: Option<(i32, i32)>,
}

impl LinuxPointerProvider {
    pub fn new(pipewire: WaylandPointerProvider) -> Self {
        let x11 = match x11rb::connect(None) {
            Ok((connection, _)) => {
                log::info!("Using PipeWire cursor coordinates with X11 fallback");
                Some(connection)
            }
            Err(error) => {
                log::info!("X11 cursor coordinates unavailable; using PipeWire: {error}");
                None
            }
        };
        Self {
            x11,
            pipewire,
            state: LinuxPointerState::new(),
            last_source: None,
            last_position: None,
        }
    }

    fn x11_position(&self) -> Result<(i32, i32), Box<dyn Error>> {
        let connection = self.x11.as_ref().ok_or("X11 is unavailable")?;
        for screen in &connection.setup().roots {
            let reply = connection.query_pointer(screen.root)?.reply()?;
            if reply.same_screen {
                return Ok((i32::from(reply.root_x), i32::from(reply.root_y)));
            }
        }
        Err("X11 did not report the pointer on any screen".into())
    }
}

impl PointerProvider for LinuxPointerProvider {
    fn snapshot(&mut self) -> Result<PointerSnapshot, Box<dyn Error>> {
        let pipewire = self.pipewire.pipewire_observation().map(|(pos, _)| pos);
        let x11_result = self.x11_position();
        let now = Instant::now();
        let (position, source) = self.state.update(pipewire, x11_result, now)?;
        if self.last_source != Some(source) {
            let delta = self
                .last_position
                .map(|previous| (position.0 - previous.0, position.1 - previous.1));
            log::info!(
                "Cursor source changed from {:?} to {:?}: position={position:?}, delta={delta:?}",
                self.last_source,
                source
            );
        }
        self.last_source = Some(source);
        self.last_position = Some(position);
        self.pipewire
            .snapshot_with_position_override(Some(position))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{LinuxPointerState, PositionSource};

    #[test]
    fn pipewire_position_is_preferred_without_x11_motion() {
        let mut state = LinuxPointerState::new();
        let now = Instant::now();
        let (position, source) = state.update(Some((10, 20)), Ok((30, 40)), now).unwrap();

        assert_eq!(position, (10, 20));
        assert_eq!(source, PositionSource::PipeWire);
    }

    #[test]
    fn x11_position_is_used_when_pipewire_has_none() {
        let mut state = LinuxPointerState::new();
        let now = Instant::now();
        let (position, source) = state.update(None, Ok((30, 40)), now).unwrap();
        assert_eq!(position, (30, 40));
        assert_eq!(source, PositionSource::X11);
    }

    #[test]
    fn stationary_x11_does_not_hijack_pipewire() {
        let mut state = LinuxPointerState::new();
        let mut now = Instant::now();

        for _ in 0..10 {
            let (position, source) = state.update(Some((10, 20)), Ok((969, 562)), now).unwrap();
            assert_eq!(position, (10, 20));
            assert_eq!(source, PositionSource::PipeWire);
            now += Duration::from_millis(20);
        }
    }

    #[test]
    fn active_x11_movement_switches_source_to_x11() {
        let mut state = LinuxPointerState::new();
        let mut now = Instant::now();

        // Baseline X11 position while PipeWire is active
        let _ = state.update(Some((10, 20)), Ok((100, 200)), now).unwrap();

        // 1st distinct movement from X11 (inside game)
        now += Duration::from_millis(20);
        let (pos1, src1) = state.update(Some((10, 20)), Ok((105, 202)), now).unwrap();
        assert_eq!(pos1, (10, 20));
        assert_eq!(src1, PositionSource::PipeWire);

        // 2nd distinct movement from X11 (inside game) -> switches to X11!
        now += Duration::from_millis(20);
        let (pos2, src2) = state.update(Some((10, 20)), Ok((110, 205)), now).unwrap();
        assert_eq!(pos2, (110, 205));
        assert_eq!(src2, PositionSource::X11);

        // Subsequent stationary frame in game stays on X11
        now += Duration::from_millis(20);
        let (pos3, src3) = state.update(Some((10, 20)), Ok((110, 205)), now).unwrap();
        assert_eq!(pos3, (110, 205));
        assert_eq!(src3, PositionSource::X11);
    }

    #[test]
    fn active_pipewire_movement_switches_back_from_x11() {
        let mut state = LinuxPointerState::new();
        let mut now = Instant::now();

        // Switch to X11 first
        let _ = state.update(Some((10, 20)), Ok((100, 200)), now).unwrap();
        now += Duration::from_millis(20);
        let _ = state.update(Some((10, 20)), Ok((105, 202)), now).unwrap();
        now += Duration::from_millis(20);
        let _ = state.update(Some((10, 20)), Ok((110, 205)), now).unwrap();
        assert_eq!(state.active_source, PositionSource::X11);

        // PipeWire begins moving when returning to a Wayland window
        now += Duration::from_millis(20);
        let _ = state.update(Some((50, 60)), Ok((110, 205)), now).unwrap();
        assert_eq!(state.active_source, PositionSource::X11);

        // 1st distinct PipeWire move
        now += Duration::from_millis(20);
        let _ = state.update(Some((55, 62)), Ok((110, 205)), now).unwrap();
        assert_eq!(state.active_source, PositionSource::X11);

        // 2nd distinct PipeWire move -> switches back to PipeWire!
        now += Duration::from_millis(20);
        let (pos, src) = state.update(Some((60, 65)), Ok((110, 205)), now).unwrap();
        assert_eq!(pos, (60, 65));
        assert_eq!(src, PositionSource::PipeWire);
    }

    #[test]
    fn x11_failure_surfaces_when_pipewire_has_none() {
        let mut state = LinuxPointerState::new();
        let result = state.update(None, Err("X11 query failed".into()), Instant::now());
        assert!(result.is_err());
    }
}
