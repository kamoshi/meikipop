# Modified from AuroraWright's OwOCR
import logging
import threading
from pathlib import Path

from meikipop.utils.paths import paths

import mss as real_mss
from mss.exception import ScreenShotError
from mss.screenshot import ScreenShot, Size
from mss.models import Monitor
from meikipop_native import WaylandCapture, crop_bgra

logger = logging.getLogger(__name__)

screencast = None
screencast_lock = threading.Lock()

token_file = Path(paths.cache_dir) / '.ocr_screencapture_token'


class ScreenCastManager:
    def __init__(self):
        logger.info("Using Rust Wayland ScreenCast backend")
        self.capture = WaylandCapture(str(token_file))

    def wait_until_ready(self):
        try:
            self.capture.wait_ready(63)
        except (RuntimeError, TimeoutError) as error:
            raise ScreenShotError(str(error)) from error

    def request_frame(self):
        frame = self.capture.request_frame()
        if frame is None:
            return (None, 0, 0)
        return frame

    def stop(self):
        self.capture.stop()


class MSSWaylandShim:
    def __init__(self):
        global screencast
        with screencast_lock:
            if not screencast:
                screencast = ScreenCastManager()
                screencast.wait_until_ready()
        self._create_monitors()

    @property
    def monitors(self):
        return self._monitors

    def grab(self, sct_params):
        frame_data = self._grab_screenshot(sct_params)
        bgra_data, crop_width, crop_height = frame_data

        return ScreenShot(bgra_data, self._monitors[0], size=Size(crop_width, crop_height))

    def _create_monitors(self):
        self._monitors = []

        frame = screencast.request_frame()
        if frame[0] is None:
            raise ScreenShotError('Invalid frame received')

        _, width, height = frame

        fake_monitor = Monitor({
            'top': 0,
            'left': 0,
            'width': width,
            'height': height
        })

        self._monitors.append(fake_monitor)
        self._monitors.append(fake_monitor)

    def _grab_screenshot(self, sct_params):
        frame = screencast.request_frame()
        if frame[0] is None:
            raise ScreenShotError('Invalid frame received')

        bgra_data, full_width, full_height = frame

        if sct_params != self._monitors[0]:
            crop_top = sct_params['top']
            crop_left = sct_params['left']
            crop_width = sct_params['width']
            crop_height = sct_params['height']

            return crop_bgra(
                bgra_data,
                full_width,
                full_height,
                (crop_left, crop_top, crop_width, crop_height),
            )

        return bgra_data, full_width, full_height

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        pass


class MSSModuleShim:
    def mss(self):
        return MSSWaylandShim()

    def __getattr__(self, name):
        return getattr(real_mss, name)

