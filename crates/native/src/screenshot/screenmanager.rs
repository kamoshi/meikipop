// meikipop/screenshot/screenmanager.rs

use std::error::Error;
use std::time::{Duration, Instant};

use crate::screenshot::interface::{CaptureGeometry, FrameProvider, RgbImage, Screenshot};

#[derive(Clone, Debug)]
pub struct ScreenManagerConfig {
    pub auto_scan_mode: bool,
    pub is_enabled: bool,
    pub auto_scan_interval_seconds: f64,
    pub auto_scan_on_mouse_move: bool,
}

pub trait ScreenManagerRuntime {
    fn running(&self) -> bool;
    fn config(&self) -> ScreenManagerConfig;
    fn wait_for_screenshot_trigger(&mut self);
    fn clear_screenshot_trigger(&mut self);
    fn get_mouse_pos(&mut self) -> (i32, i32);
    fn screen_lock_acquire(&mut self);
    fn screen_lock_release(&mut self);
    fn put_ocr(&mut self, image: RgbImage);
    fn sleep(&mut self, interval: Duration);
    fn set_screenshot_trigger(&mut self);
    fn trigger_hit_scan(&mut self);
    fn log_error(&mut self, error: &dyn Error);
}

pub struct ScreenManager<B> {
    pub capture_geometry: Option<CaptureGeometry>,
    pub last_ocr_put_time: Option<Instant>,
    pub last_screenshot: Option<Screenshot>,
    pub last_mouse_pos: Option<(i32, i32)>,
    frame_provider: B,
}

impl<B: FrameProvider> ScreenManager<B> {
    pub fn new(frame_provider: B) -> Self {
        Self {
            capture_geometry: None,
            last_ocr_put_time: None,
            last_screenshot: None,
            last_mouse_pos: None,
            frame_provider,
        }
    }

    pub fn run<R: ScreenManagerRuntime>(&mut self, runtime: &mut R) {
        log::debug!("Screenshot thread started.");
        while runtime.running() {
            if let Err(error) = self.run_once(runtime) {
                runtime.log_error(error.as_ref());
                self._sleep_and_handle_loop_exit(runtime, Duration::from_secs(1));
            }
        }
        log::debug!("Screenshot thread stopped.");
    }

    pub(crate) fn run_once<R: ScreenManagerRuntime>(
        &mut self,
        runtime: &mut R,
    ) -> Result<(), Box<dyn Error>> {
        let config = runtime.config();
        if config.auto_scan_mode && !config.is_enabled {
            log::debug!("paused while auto mode");
            self._sleep_and_handle_loop_exit(runtime, Duration::from_secs(1));
            return Ok(());
        }
        runtime.wait_for_screenshot_trigger();
        runtime.clear_screenshot_trigger();
        if !runtime.running() {
            return Ok(());
        }
        log::debug!("Screenshot: Triggered!");

        // Read the configuration again after waking, as upstream does. Settings
        // may have changed while waiting for the screenshot trigger.
        let config = runtime.config();

        // prevent multiple ocr runs during auto_scan_interval_seconds
        let seconds_since_last_ocr = self
            .last_ocr_put_time
            .map_or(f64::INFINITY, |last_ocr_put_time| {
                last_ocr_put_time.elapsed().as_secs_f64()
            });
        if config.auto_scan_mode && seconds_since_last_ocr < config.auto_scan_interval_seconds {
            let remaining = config.auto_scan_interval_seconds - seconds_since_last_ocr;
            log::debug!(
                "...{seconds_since_last_ocr:.2}s since last ocr, sleeping for another {remaining:.2}s"
            );
            self._sleep_and_handle_loop_exit(runtime, Duration::from_secs_f64(remaining));
            return Ok(());
        }

        // prevent ocr runs without mouse movements for auto-on-mouse-move mode
        let mouse_pos = runtime.get_mouse_pos();
        if config.auto_scan_mode
            && config.auto_scan_on_mouse_move
            && self.last_mouse_pos == Some(mouse_pos)
        {
            return Ok(());
        }
        self.last_mouse_pos = Some(runtime.get_mouse_pos());

        log::debug!("screenmanager acquiring lock...");
        runtime.screen_lock_acquire();
        log::debug!("...successfully acquired lock by screenmanager");
        let start_time = Instant::now();
        let screenshot = self.take_screenshot();
        runtime.screen_lock_release();
        log::debug!("...successfully released lock by screenmanager");
        let screenshot = screenshot?;
        let processing_duration = start_time.elapsed().as_secs_f64();
        log::debug!(
            "Screenshot {:?} complete in {processing_duration:.2}s",
            screenshot.size()
        );

        if self
            .last_screenshot
            .as_ref()
            .is_some_and(|last_screenshot| last_screenshot.raw == screenshot.raw)
        {
            log::debug!("Screen content didnt change... skipping ocr");
            self._sleep_and_handle_loop_exit(runtime, Duration::from_secs_f64(0.1));
            return Ok(());
        }

        self.last_screenshot = Some(screenshot);
        self.last_mouse_pos = Some(runtime.get_mouse_pos());
        let image = self
            .last_screenshot
            .as_ref()
            .expect("last_screenshot was just set")
            .to_rgb()?;
        runtime.put_ocr(image);
        self.last_ocr_put_time = Some(Instant::now());
        Ok(())
    }

    pub fn take_screenshot(&mut self) -> Result<Screenshot, Box<dyn Error>> {
        self.frame_provider
            .capture_frame()?
            .map(|frame| frame.screenshot)
            .ok_or_else(|| "No capture source is currently shared".into())
    }

    pub fn refresh_capture_geometry(&mut self) -> Result<(), Box<dyn Error>> {
        self.capture_geometry = self.frame_provider.capture_geometry()?;
        Ok(())
    }

    pub fn get_capture_geometry(&self) -> (i32, i32, usize, usize) {
        let Some(geometry) = &self.capture_geometry else {
            return (0, 0, 0, 0);
        };
        (geometry.left, geometry.top, geometry.width, geometry.height)
    }

    pub fn force_screenshot_trigger(&mut self) {
        self.last_screenshot = None;
        self.last_mouse_pos = None;
    }

    pub(crate) fn _sleep_and_handle_loop_exit<R: ScreenManagerRuntime>(
        &self,
        runtime: &mut R,
        interval: Duration,
    ) {
        if runtime.config().auto_scan_mode {
            runtime.sleep(interval);
            runtime.set_screenshot_trigger();
        } else {
            runtime.trigger_hit_scan();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screenshot::interface::CapturedFrame;
    use std::collections::VecDeque;

    struct FakeFrameProvider {
        geometry: CaptureGeometry,
        screenshots: VecDeque<Screenshot>,
    }

    impl FrameProvider for FakeFrameProvider {
        fn capture_geometry(&mut self) -> Result<Option<CaptureGeometry>, Box<dyn Error>> {
            Ok(Some(self.geometry.clone()))
        }

        fn capture_frame(&mut self) -> Result<Option<CapturedFrame>, Box<dyn Error>> {
            let screenshot = self.screenshots.pop_front().ok_or("no screenshot queued")?;
            Ok(Some(CapturedFrame {
                source_generation: 1,
                sequence: self.screenshots.len() as u64,
                screenshot,
                geometry: self.geometry.clone(),
            }))
        }
    }

    struct FakeRuntime {
        running: bool,
        config: ScreenManagerConfig,
        mouse_positions: VecDeque<(i32, i32)>,
        ocr_images: Vec<RgbImage>,
        slept: Vec<Duration>,
        screenshot_triggers: usize,
        hit_scan_triggers: usize,
        screen_lock_acquires: usize,
        screen_lock_releases: usize,
    }

    impl ScreenManagerRuntime for FakeRuntime {
        fn running(&self) -> bool {
            self.running
        }
        fn config(&self) -> ScreenManagerConfig {
            self.config.clone()
        }
        fn wait_for_screenshot_trigger(&mut self) {}
        fn clear_screenshot_trigger(&mut self) {}
        fn get_mouse_pos(&mut self) -> (i32, i32) {
            self.mouse_positions.front().copied().unwrap_or_default()
        }
        fn screen_lock_acquire(&mut self) {
            self.screen_lock_acquires += 1;
        }
        fn screen_lock_release(&mut self) {
            self.screen_lock_releases += 1;
        }
        fn put_ocr(&mut self, image: RgbImage) {
            self.ocr_images.push(image);
        }
        fn sleep(&mut self, interval: Duration) {
            self.slept.push(interval);
        }
        fn set_screenshot_trigger(&mut self) {
            self.screenshot_triggers += 1;
        }
        fn trigger_hit_scan(&mut self) {
            self.hit_scan_triggers += 1;
        }
        fn log_error(&mut self, _error: &dyn Error) {}
    }

    fn geometry() -> CaptureGeometry {
        CaptureGeometry {
            top: 20,
            left: 10,
            width: 1,
            height: 1,
        }
    }

    fn screenshot(pixel: [u8; 4]) -> Screenshot {
        Screenshot {
            raw: pixel.into(),
            width: 1,
            height: 1,
        }
    }

    fn manager(screenshots: Vec<Screenshot>) -> ScreenManager<FakeFrameProvider> {
        let backend = FakeFrameProvider {
            geometry: geometry(),
            screenshots: screenshots.into(),
        };
        let mut manager = ScreenManager::new(backend);
        manager.capture_geometry = Some(geometry());
        manager
    }

    fn runtime() -> FakeRuntime {
        FakeRuntime {
            running: true,
            config: ScreenManagerConfig {
                auto_scan_mode: false,
                is_enabled: true,
                auto_scan_interval_seconds: 0.5,
                auto_scan_on_mouse_move: false,
            },
            mouse_positions: VecDeque::from([(100, 200)]),
            ocr_images: Vec::new(),
            slept: Vec::new(),
            screenshot_triggers: 0,
            hit_scan_triggers: 0,
            screen_lock_acquires: 0,
            screen_lock_releases: 0,
        }
    }

    #[test]
    fn converts_and_puts_a_changed_screenshot() {
        let mut manager = manager(vec![screenshot([10, 20, 30, 255])]);
        let mut runtime = runtime();

        manager.run_once(&mut runtime).unwrap();

        assert_eq!(runtime.ocr_images[0].as_raw(), &[30, 20, 10]);
        assert_eq!(manager.last_mouse_pos, Some((100, 200)));
        assert!(manager.last_ocr_put_time.is_some());
    }

    #[test]
    fn skips_unchanged_screen_content() {
        let frame = screenshot([10, 20, 30, 255]);
        let mut manager = manager(vec![frame.clone()]);
        manager.last_screenshot = Some(frame);
        let mut runtime = runtime();

        manager.run_once(&mut runtime).unwrap();

        assert!(runtime.ocr_images.is_empty());
        assert_eq!(runtime.hit_scan_triggers, 1);
    }

    #[test]
    fn auto_mode_skips_scans_without_mouse_movement() {
        let mut manager = manager(vec![screenshot([10, 20, 30, 255])]);
        manager.last_mouse_pos = Some((100, 200));
        let mut runtime = runtime();
        runtime.config.auto_scan_mode = true;
        runtime.config.auto_scan_on_mouse_move = true;

        manager.run_once(&mut runtime).unwrap();

        assert!(runtime.ocr_images.is_empty());
    }

    #[test]
    fn paused_auto_mode_sleeps_and_retriggers_screenshot() {
        let mut manager = manager(Vec::new());
        let mut runtime = runtime();
        runtime.config.auto_scan_mode = true;
        runtime.config.is_enabled = false;

        manager.run_once(&mut runtime).unwrap();

        assert_eq!(runtime.slept, [Duration::from_secs(1)]);
        assert_eq!(runtime.screenshot_triggers, 1);
    }

    #[test]
    fn auto_mode_respects_the_upstream_scan_interval() {
        let mut manager = manager(Vec::new());
        manager.last_ocr_put_time = Some(Instant::now());
        let mut runtime = runtime();
        runtime.config.auto_scan_mode = true;

        manager.run_once(&mut runtime).unwrap();

        assert_eq!(runtime.slept.len(), 1);
        assert!(runtime.slept[0] <= Duration::from_secs_f64(0.5));
        assert_eq!(runtime.screenshot_triggers, 1);
        assert!(runtime.ocr_images.is_empty());
    }

    #[test]
    fn capture_errors_still_release_the_screen_lock() {
        let mut manager = manager(Vec::new());
        let mut runtime = runtime();

        assert!(manager.run_once(&mut runtime).is_err());

        assert_eq!(runtime.screen_lock_acquires, 1);
        assert_eq!(runtime.screen_lock_releases, 1);
    }

    #[test]
    fn force_screenshot_trigger_clears_upstream_state() {
        let mut manager = manager(Vec::new());
        manager.last_screenshot = Some(screenshot([10, 20, 30, 255]));
        manager.last_mouse_pos = Some((100, 200));

        manager.force_screenshot_trigger();

        assert!(manager.last_screenshot.is_none());
        assert!(manager.last_mouse_pos.is_none());
    }

    #[test]
    fn refreshes_the_selected_source_geometry() {
        let mut manager = manager(Vec::new());
        manager.capture_geometry = None;

        manager.refresh_capture_geometry().unwrap();

        assert_eq!(manager.capture_geometry, Some(geometry()));
        assert_eq!(manager.get_capture_geometry(), (10, 20, 1, 1));
    }
}
