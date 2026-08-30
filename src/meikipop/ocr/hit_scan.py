import logging
import threading
from typing import List

from meikipop.gui.magpie_manager import magpie_manager
from meikipop.ocr.interface import Paragraph
from meikipop_native.ocr.hit_scan import hit_scan as native_hit_scan

logger = logging.getLogger(__name__)


class HitScanner(threading.Thread):
    def __init__(self, shared_state, input_loop, screen_manager):
        super().__init__(daemon=True, name="HitScanner")
        self.shared_state = shared_state
        self.input_loop = input_loop
        self.screen_manager = screen_manager
        self.last_ocr_result = None

    def run(self):
        logger.debug("HitScanner thread started.")
        while self.shared_state.running:
            try:
                ocr_result = self.shared_state.hit_scan_queue.get()
                if not self.shared_state.running:
                    break
                logger.debug("HitScanner: Triggered")
                hit_scan_result = self.hit_scan(ocr_result)
                self.shared_state.lookup_queue.put(hit_scan_result)
            except Exception:
                logger.exception("An unexpected error occurred in the hit scan loop. Continuing...")
        logger.debug("HitScanner thread stopped.")

    def hit_scan(self, paragraphs: List[Paragraph]):
        if not paragraphs:
            return None

        mouse_x, mouse_y = magpie_manager.transform_raw_to_visual(
            self.input_loop.get_mouse_pos(), 1
        )
        mouse_off_x, mouse_off_y, img_w, img_h = self.screen_manager.get_scan_geometry()
        relative_x = mouse_x - mouse_off_x
        relative_y = mouse_y - mouse_off_y
        norm_x, norm_y = relative_x / img_w, relative_y / img_h

        return native_hit_scan(paragraphs, norm_x, norm_y)
