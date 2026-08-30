import logging
import re
import threading
from collections import OrderedDict
from dataclasses import dataclass
from typing import Dict, List

from meikipop.config.config import config, MAX_DICT_ENTRIES, DICT_PATH
from meikipop.dictionary.customdict import Dictionary
from meikipop_native.dictionary.lookup import LookupEngine

KANJI_REGEX = re.compile(r'[\u4e00-\u9faf]')
JAPANESE_SEPARATORS = {
    "、", "。", "「", "」", "｛", "｝", "（", "）", "【", "】",
    "『", "』", "〈", "〉", "《", "》", "：", "・", "／",
    "…", "︙", "‥", "︰", "＋", "＝", "－", "÷", "？", "！",
    "．", "～", "―", "!", "?",
}

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

        self.dictionary = Dictionary()
        self.lookup_cache: OrderedDict = OrderedDict()
        self.CACHE_SIZE = 500

        if not self.dictionary.load_dictionary(DICT_PATH):
            raise RuntimeError("Failed to load dictionary.")
        self.lookup_engine = LookupEngine(
            self.dictionary.entries,
            self.dictionary.lookup_map,
            self.dictionary.deconjugator_rules,
            MAX_DICT_ENTRIES,
        )

    def clear_cache(self):
        self.lookup_cache = OrderedDict()

    def run(self):
        logger.debug("Lookup thread started.")
        while self.shared_state.running:
            try:
                hit_result = self.shared_state.lookup_queue.get()
                if not self.shared_state.running:
                    break
                logger.debug("Lookup: Triggered")

                # skip lookup if hit_result didnt change
                if hit_result == self.last_hit_result:
                    continue
                self.last_hit_result = hit_result

                lookup_result = self.lookup(self.last_hit_result) if self.last_hit_result else None
                self.popup_window.set_latest_data(lookup_result)
            except Exception:
                logger.exception("An unexpected error occurred in the lookup loop. Continuing...")
        logger.debug("Lookup thread stopped.")

    def lookup(self, lookup_string: str) -> List:
        if not lookup_string:
            return []
        logger.info(f"Looking up: {lookup_string}")  # keep at info level so people know whats up

        text = lookup_string.strip()
        text = text[:config.max_lookup_length]
        for i, ch in enumerate(text):
            if ch in JAPANESE_SEPARATORS:
                text = text[:i]
                break
        if not text:
            return []

        if text in self.lookup_cache:
            self.lookup_cache.move_to_end(text)
            return self.lookup_cache[text]

        results = self._do_lookup(text)

        # Append kanji entry for the first character if applicable
        if config.show_kanji and KANJI_REGEX.match(text[0]):
            kd = self.dictionary.kanji_entries.get(text[0])
            if kd:
                results.append(KanjiEntry(
                    character=kd['character'],
                    meanings=kd['meanings'],
                    readings=kd['readings'],
                    components=kd.get('components', []),
                    examples=kd.get('examples', []),
                ))

        self.lookup_cache[text] = results
        if len(self.lookup_cache) > self.CACHE_SIZE:
            self.lookup_cache.popitem(last=False)
        return results

    def _do_lookup(self, text: str) -> List[DictionaryEntry]:
        results = []
        for entry in self.lookup_engine.lookup(text):
            entry['deconjugation_process'] = tuple(entry['deconjugation_process'])
            results.append(DictionaryEntry(**entry))
        return results
