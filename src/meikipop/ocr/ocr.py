# meikipop/ocr/ocr.py
import logging
import sys
import threading

from meikipop.config.config import config
from meikipop_native.ocr.processor import OcrProcessor as NativeOcrProcessor

logger = logging.getLogger(__name__)  # Get the logger

class OcrProcessor(threading.Thread):
    def __init__(self, shared_state, screen_manager):
        super().__init__(daemon=True, name="OcrProcessor")
        self.shared_state = shared_state
        self.screen_manager = screen_manager
        self.ocr_backend = NativeOcrProcessor()

        self.available_providers = self._discover_providers()
        if not self.available_providers:
            logger.critical("No OCR providers found! The application cannot continue.")
            sys.exit(1)

        self._load_provider_from_config()

    def run(self):
        self.ocr_backend.start_worker(self.shared_state, config, logger)

    # todo combine methods?
    def switch_provider(self, provider_name: str):
        if self.ocr_backend and provider_name == self.ocr_backend.NAME:
            return

        if provider_name in self.available_providers:
            logger.info(f"Switching OCR provider to '{provider_name}'...")
            try:
                self.ocr_backend.switch_provider(provider_name)
                logger.info(f"Successfully switched OCR provider to '{self.ocr_backend.NAME}'")
                config.ocr_provider = self.ocr_backend.NAME
                config.save()  # todo fix tray showing wrong provider
                if config.auto_scan_mode:
                    self.shared_state.hit_scan_queue.put(None)
                    self.screen_manager.force_screenshot_trigger()
                    self.shared_state.screenshot_trigger_event.set()
            except Exception as e:
                logger.error(f"Failed to instantiate provider '{provider_name}': {e}", exc_info=True)
                if self.ocr_backend:
                    logger.info(f"Reverting to previous provider '{self.ocr_backend.NAME}'.")
                    config.ocr_provider = self.ocr_backend.NAME
                    config.save()  # todo fix tray showing wrong provider

        else:
            logger.error(f"Attempted to switch to an unknown provider: '{provider_name}'")

    def _load_provider_from_config(self):
        configured_provider_name = config.ocr_provider
        default_provider_name = "meikiocr (local)"

        provider_to_load_name = configured_provider_name

        if configured_provider_name not in self.available_providers:
            logger.warning(
                f"Configured OCR provider '{configured_provider_name}' not found. "
                f"Falling back to default provider '{default_provider_name}'."
            )
            provider_to_load_name = default_provider_name

        if provider_to_load_name not in self.available_providers:
            fallback_provider_name = list(self.available_providers.keys())[0]
            logger.warning(
                f"Default OCR provider '{provider_to_load_name}' not found. "
                f"Falling back to first available provider: '{fallback_provider_name}'."
            )
            provider_to_load_name = fallback_provider_name

        config.ocr_provider = provider_to_load_name

        try:
            self.ocr_backend.switch_provider(provider_to_load_name)
            logger.info(f"Initialized OCR with '{self.ocr_backend.NAME}' provider.")
        except Exception as e:
            logger.critical(f"Failed to instantiate provider '{provider_to_load_name}' on startup: {e}", exc_info=True)
            try:
                self.switch_provider(default_provider_name)
            except Exception as e:
                self.ocr_backend = None
                sys.exit(1)

    def _discover_providers(self):
        providers = {}

        # Native providers are registered statically instead of discovered by
        # scanning Python packages.
        for provider_name in self.ocr_backend.available_providers:
            providers[provider_name] = None
            logger.debug(f" -> Discovered provider: '{provider_name}'")
        return providers
