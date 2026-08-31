import logging
import threading
from dataclasses import dataclass
from typing import Dict, List

from meikipop.config.config import config, MAX_DICT_ENTRIES, DICT_PATH
from meikipop_native.dictionary.lookup import LookupEngine, LookupWorker

logger = logging.getLogger(__name__)


@dataclass
class DictionaryEntry:
    id: int
    written_form: str
    reading: str  # empty when written_form is already kana
    senses: list
    freq: int
    deconjugation_process: tuple
    priority: float = 0.0


@dataclass
class KanjiEntry:
    character: str
    meanings: List[str]
    readings: List[str]
    components: List[Dict[str, str]]
    examples: List[Dict[str, str]]


class Lookup(threading.Thread):
    def __init__(self, shared_state, popup_window):
        super().__init__(daemon=True, name="Lookup")
        self.shared_state = shared_state
        self.popup_window = popup_window
        self.last_hit_result = None

        self.lookup_engine = LookupEngine.open(
            DICT_PATH,
            MAX_DICT_ENTRIES,
        )
        issues, warnings = self.lookup_engine.validate()
        for warning in warnings:
            logger.warning(warning)
        if issues == 0:
            logger.info("Dictionary validation passed with no issues.")
        else:
            logger.warning(f"Dictionary validation found {issues} issue(s) — "
                           f"some entries may display incorrectly.")
        self._native = LookupWorker(
            shared_state, popup_window, self.lookup_engine, config, logger
        )

    def clear_cache(self):
        self.lookup_engine.clear_cache()

    def run(self):
        self._native.start()

    def lookup(self, lookup_string: str) -> List:
        if not lookup_string:
            return []
        logger.info(f"Looking up: {lookup_string}")  # keep at info level so people know whats up

        return self._do_lookup(lookup_string)

    def _do_lookup(self, text: str) -> List:
        results = []
        for entry in self.lookup_engine.lookup(
                text, config.max_lookup_length, config.show_kanji):
            if 'character' in entry:
                results.append(KanjiEntry(**entry))
            else:
                entry['deconjugation_process'] = tuple(entry['deconjugation_process'])
                results.append(DictionaryEntry(**entry))
        return results
