"""Native OCR postprocessing API.

The implementation lives in the ``meikipop_native`` Rust extension. This
module preserves MeikiPop's existing import path for OCR providers.
"""

from meikipop_native.ocr.providers.postprocessing import (
    FURIGANA_HORIZONTAL_HEIGHT_THRESHOLD,
    FURIGANA_VERTICAL_WIDTH_THRESHOLD,
    group_lines_into_paragraphs,
)

__all__ = [
    "FURIGANA_HORIZONTAL_HEIGHT_THRESHOLD",
    "FURIGANA_VERTICAL_WIDTH_THRESHOLD",
    "group_lines_into_paragraphs",
]
