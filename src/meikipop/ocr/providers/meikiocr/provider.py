import logging
from typing import List, Optional

from PIL import Image

# Import the MeikiOCR provider
from meikipop_native.ocr.providers.meikiocr import MeikiOcrProvider as NativeMeikiOcrProvider

# Import the "contract" classes from your application's interface
from meikipop.ocr.interface import OcrProvider, Paragraph

logger = logging.getLogger(__name__)

class MeikiOcrProvider(OcrProvider):
    """
    An OCR provider that uses the high-performance meikiocr library.
    This provider is specifically optimized for recognizing Japanese text from video games.
    """
    NAME = "meikiocr (local)"

    def __init__(self):
        """
        Initializes the provider by creating an instance of the MeikiOCR client.
        The library handles the model downloading and session management internally.
        """
        logger.info(f"initializing {self.NAME} provider...")
        self.ocr_client = None
        try:
            self.ocr_client = NativeMeikiOcrProvider()
            logger.info(f"{self.NAME} initialized successfully, running on: {self.ocr_client.active_provider}")

        except Exception as e:
            logger.error(f"failed to initialize {self.NAME}: {e}", exc_info=True)

    def scan(self, image: Image.Image) -> Optional[List[Paragraph]]:
        """
        Performs OCR on the given image by calling the meikiocr library.
        """
        if not self.ocr_client:
            logger.error(f"{self.NAME} was not initialized correctly. Cannot perform scan.")
            return None

        try:
            image_rgb = image.convert("RGB")
            img_width, img_height = image_rgb.size

            if img_width == 0 or img_height == 0:
                logger.error("invalid image dimensions received.")
                return None

            return self.ocr_client.scan(
                image_rgb.tobytes(),
                img_width,
                img_height
            )

        except Exception as e:
            logger.error(f"an error occurred in {self.NAME}: {e}", exc_info=True)
            return None  # returning none indicates a failure.
