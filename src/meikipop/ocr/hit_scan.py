import logging
import threading

from meikipop.gui.magpie_manager import magpie_manager
from meikipop_native.ocr.hit_scan import HitScanner as NativeHitScanner

logger = logging.getLogger(__name__)


class HitScanner(threading.Thread):
    def __init__(self, shared_state, input_loop, screen_manager, ocr_processor):
        super().__init__(daemon=True, name="HitScanner")
        self.shared_state = shared_state
        self.input_loop = input_loop
        self.screen_manager = screen_manager
        self.ocr_processor = ocr_processor
        self.last_ocr_result = None
        self._native = NativeHitScanner(
            shared_state, input_loop, screen_manager, ocr_processor.ocr_backend,
            magpie_manager, logger
        )

    def run(self):
        self._native.start()

    def hit_scan(self):
        return self._native.hit_scan()
