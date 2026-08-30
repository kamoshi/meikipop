# meikipop/ocr/providers/dummy/provider.py
import logging
from typing import List, Optional

from PIL import Image

# The "contract" classes that a new provider MUST use for its return value.
from meikipop.ocr.interface import OcrProvider, Paragraph
from meikipop_native.ocr.providers.dummy import DummyProvider as NativeDummyProvider

logger = logging.getLogger(__name__)


class DummyProvider(OcrProvider):
    """
    A template for creating new OCR providers.

    This class demonstrates the required structure and data transformations.
    Developers can copy this file to start their own provider implementation.
    When this provider is selected, it returns a fixed set of Japanese text
    to allow for testing of the popup window without a real OCR backend.
    """
    # The NAME is displayed in the settings and tray menu. Make it unique and descriptive.
    NAME = "Dummy OCR (Developer Template)"

    def __init__(self):
        self._native = NativeDummyProvider()

    def scan(self, image: Image.Image) -> Optional[List[Paragraph]]:
        """
        Performs OCR on the given image.

        This method must be implemented. Its main job is to:
        1. Get OCR data from an external source (library, API, etc.).
        2. Convert the proprietary data format into meikipop's standard format
           (a list of Paragraph objects with normalized coordinates).
        3. Return the list of Paragraphs.
        """
        try:
            return self._native.scan(*image.size)
        except Exception as e:
            logger.error(f"An error occurred in {self.NAME}: {e}", exc_info=True)
            return None  # Returning None indicates a failure.
