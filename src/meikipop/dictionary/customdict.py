# customdict.py
import logging
import pickle
import time
from collections import defaultdict

from meikipop.utils.paths import paths
from meikipop_native.dictionary.customdict import download_dictionary

logger = logging.getLogger(__name__)

DEFAULT_FREQ = 999_999

# MapEntry tuple field indices. value: (written_form, reading, freq, entry_id)
WRITTEN_FORM_INDEX = 0
READING_INDEX = 1
FREQUENCY_INDEX = 2
ENTRY_ID_INDEX = 3

class Dictionary:
    def __init__(self):
        # Core entries: {entry_id: [sense, ...]}
        # Each sense: {'glosses': [...], 'pos': [...], 'misc': [...]}
        self.entries: dict[int, list] = {}

        # lookup_map: surface_form → [(written_form, reading_or_None, freq, entry_id), ...]
        self.lookup_map: dict[str, list] = defaultdict(list)

        # Kanji character entries from kanjidic2: {character: {...}}
        self.kanji_entries: dict[str, dict] = {}

        # Deconjugation rules consumed by Deconjugator at runtime
        self.deconjugator_rules: list[dict] = []

        self._is_loaded = False

    def load_dictionary(self, file_path: str) -> bool:
        if self._is_loaded:
            return True
        logger.info(f"Loading dictionary from '{file_path}'")
        start = time.perf_counter()
        try:
            with open(file_path, 'rb') as f:
                data = pickle.load(f)
            self.entries            = data['entries']
            self.lookup_map         = data['lookup_map']
            self.kanji_entries      = data.get('kanji_entries', {})
            self.deconjugator_rules = data.get('deconjugator_rules', [])
            self._is_loaded = True
            n_refs = sum(len(v) for v in self.lookup_map.values())
            logger.info(
                f"Dictionary loaded in {time.perf_counter() - start:.2f}s"
                f"({len(self.entries)} core entries, {n_refs} lookup refs)"
            )
            return True
        except FileNotFoundError:
            logger.warning(f"Dictionary file not found. Trying download...")
            if self._download_dictionary():
                return self.load_dictionary(file_path)  # retry once after download
            logger.error(
                "Dictionary could not be downloaded. "
                "You can build it manually by running 'meikipop build-dict'."
            )
            return False
        except Exception as e:
            logger.error(f"Failed to load dictionary: {e}")
            return False

    def _download_dictionary(self) -> bool:
        try:
            download_dictionary(paths.data_dir)
            logger.info("Dictionary downloaded successfully.")
            return True
        except Exception as e:
            logger.error(f"Failed to download dictionary: {e}")
            return False

    def log_validation(self, issues, warnings):
        for warning in warnings:
            logger.warning(warning)
        if issues == 0:
            logger.info("Dictionary validation passed with no issues.")
        else:
            logger.warning(f"Dictionary validation found {issues} issue(s) — "
                           f"some entries may display incorrectly.")
