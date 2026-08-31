import os
from pathlib import Path


class MeikiPaths:
    """Centralized path resolution for the standalone dictionary scripts."""

    def __init__(self):
        home = Path.home()
        self.data_dir = Path(os.environ.get("XDG_DATA_HOME", home / ".local/share")) / "meikipop"
        self.cache_dir = Path(os.environ.get("XDG_CACHE_HOME", home / ".cache")) / "meikipop"
        self.data_dir.mkdir(parents=True, exist_ok=True)
        self.cache_dir.mkdir(parents=True, exist_ok=True)

    @property
    def dictionary_path(self):
        """Location of dictionary.pkl."""
        return str(self.data_dir / "dictionary.pkl")


paths = MeikiPaths()
