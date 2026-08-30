"""Python integration tests for the native Rust deconjugator."""

import json
from pathlib import Path

from meikipop.dictionary.deconjugator import Deconjugator, Form
from meikipop_native.dictionary.deconjugator import (
    Deconjugator as RustDeconjugator,
)
from meikipop_native.dictionary.deconjugator import Form as RustForm


RULES_PATH = (
    Path(__file__).parents[2]
    / "src"
    / "meikipop"
    / "scripts"
    / "deconjugator.json"
)


def test_existing_import_path_uses_the_rust_classes():
    assert Deconjugator is RustDeconjugator
    assert Form is RustForm


def test_representative_deconjugation_results():
    rules = json.loads(RULES_PATH.read_text(encoding="utf-8"))
    deconjugator = Deconjugator(rules)

    forms = deconjugator.deconjugate("食べました")
    assert len(forms) == 26
    assert Form("食べました") in forms
    assert Form("食べる", ("past polite", "(infinitive)"), ("v1",)) in forms

    forms = deconjugator.deconjugate("読んだ")
    assert len(forms) == 9
    assert Form("読む", ("past", "(unstressed infinitive)"), ("v5m",)) in forms

    assert deconjugator.deconjugate("") == set()


def test_rust_form_has_the_python_value_semantics():
    left = RustForm("食べる", ("polite",), ("v1",))
    right = RustForm("食べる", ("polite",), ("v1",))

    assert left == right
    assert hash(left) == hash(right)
    assert left.process == ("polite",)
    assert left.tags == ("v1",)
    assert {left, right} == {left}
