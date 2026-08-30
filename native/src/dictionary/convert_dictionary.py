import json
import os
import pickle
import sys
import tempfile


FORMAT_VERSION = 1


def main():
    pickle_path = os.path.abspath(sys.argv[1])
    json_path = os.path.abspath(sys.argv[2])

    source = os.stat(pickle_path)
    with open(pickle_path, "rb") as file:
        data = pickle.load(file)
    source_after_load = os.stat(pickle_path)
    if (source.st_size, source.st_mtime_ns) != (
        source_after_load.st_size,
        source_after_load.st_mtime_ns,
    ):
        raise RuntimeError("Dictionary pickle changed while it was being converted")

    entries = data["entries"]
    lookup_map = data["lookup_map"]
    kanji_entries = data.get("kanji_entries", {})
    deconjugator_rules = data.get("deconjugator_rules", [])

    output = {
        "format_version": FORMAT_VERSION,
        "source_size": source.st_size,
        "source_mtime_ns": source.st_mtime_ns,
        "entries": entries,
        "lookup_map": lookup_map,
        "kanji_entries": kanji_entries,
        "deconjugator_rules": deconjugator_rules,
    }

    output_directory = os.path.dirname(json_path)
    os.makedirs(output_directory, exist_ok=True)
    descriptor, temporary_path = tempfile.mkstemp(
        dir=output_directory,
        prefix="dictionary-",
        suffix=".json.tmp",
    )

    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as file:
            json.dump(output, file, ensure_ascii=False, separators=(",", ":"))
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary_path, json_path)
    except BaseException:
        try:
            os.unlink(temporary_path)
        except FileNotFoundError:
            pass
        raise


if __name__ == "__main__":
    main()
