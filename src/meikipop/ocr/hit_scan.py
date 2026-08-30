import logging
import threading

from meikipop.gui.magpie_manager import magpie_manager

logger = logging.getLogger(__name__)


class HitScanner(threading.Thread):
    def __init__(self, shared_state, input_loop, screen_manager, ocr_processor):
        super().__init__(daemon=True, name="HitScanner")
        self.shared_state = shared_state
        self.input_loop = input_loop
        self.screen_manager = screen_manager
        self.ocr_processor = ocr_processor
        self.last_ocr_result = None

    def run(self):
        logger.debug("HitScanner thread started.")
        while self.shared_state.running:
            try:
                self.shared_state.hit_scan_queue.get()
                if not self.shared_state.running:
                    break
                logger.debug("HitScanner: Triggered")
                hit_scan_result = self.hit_scan()
                self.shared_state.lookup_queue.put(hit_scan_result)
            except Exception:
                logger.exception("An unexpected error occurred in the hit scan loop. Continuing...")
        logger.debug("HitScanner thread stopped.")

    def hit_scan(self):
        mouse_x, mouse_y = magpie_manager.transform_raw_to_visual(
            self.input_loop.get_mouse_pos(), 1
        )
        mouse_off_x, mouse_off_y, img_w, img_h = self.screen_manager.get_scan_geometry()
        relative_x = mouse_x - mouse_off_x
        relative_y = mouse_y - mouse_off_y
        norm_x, norm_y = relative_x / img_w, relative_y / img_h

        return self.ocr_processor.ocr_backend.hit_scan(norm_x, norm_y)
