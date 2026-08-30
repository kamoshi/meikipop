"""Native deconjugator API.

The implementation lives in the ``meikipop_native`` Rust extension. This
module preserves MeikiPop's existing import path for callers.
"""

from meikipop_native.dictionary.deconjugator import (
    MAX_DECONJ_ITERATIONS,
    Deconjugator,
    Form,
)

__all__ = ["MAX_DECONJ_ITERATIONS", "Deconjugator", "Form"]
