"""Identity keying of scripts/check_lint_allowances.py.

An allowance is keyed to the item it is attached to. Keying to a later item
(the next `fn`) moved identities whenever an unrelated line between the two
changed, so these fixtures pin each item kind and the comment-insensitivity.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys

import pytest

_SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "check_lint_allowances.py"
_SPEC = importlib.util.spec_from_file_location("_check_lint_allowances", _SCRIPT)
assert _SPEC is not None and _SPEC.loader is not None
_MODULE = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = _MODULE  # dataclasses resolve their module by name
_SPEC.loader.exec_module(_MODULE)


def _scopes(tmp_path: Path, source: str) -> list[str]:
    source_dir = tmp_path / "crates" / "demo" / "src"
    source_dir.mkdir(parents=True, exist_ok=True)
    (source_dir / "lib.rs").write_text(source, encoding="utf-8")
    return [allowance.scope for allowance in _MODULE.collect_allowances(tmp_path)]


FOLLOWING_FN = "\n/// Doc.\npub(crate) fn resolve() -> u8 {\n    0\n}\n"


@pytest.mark.parametrize(
    ("item", "scope"),
    [
        ("pub struct Resolved {\n    x: u8,\n}\n", "struct:Resolved"),
        ("pub(crate) enum Kind {\n    A,\n}\n", "enum:Kind"),
        ("pub type KgResult<T> = Result<T, ()>;\n", "type:KgResult"),
        ("const LIMIT: u8 = 1;\n", "const:LIMIT"),
        ("static mut COUNTER: u8 = 0;\n", "static:COUNTER"),
        ("pub trait Walk {}\n", "trait:Walk"),
        ("mod tests {\n}\n", "mod:tests"),
        ("macro_rules! twice {\n    () => {};\n}\n", "macro:twice"),
        ("impl<T> Wrapper<T> {\n}\n", "impl:impl<T> Wrapper<T>"),
        ("unsafe impl Send for Wrapper {}\n", "impl:impl Send for Wrapper"),
        ("pub use crate::storage::{A, B};\n", "use:pub use crate::storage::{A, B}"),
        ("pub const fn width() -> u8 {\n    1\n}\n", "fn:width:fn width() -> u8"),
        ("pub fn first(a: u8) -> u8 {\n    a\n}\n", "fn:first:pub fn first(a: u8) -> u8"),
    ],
)
def test_allowance_keys_to_the_item_it_is_attached_to(tmp_path: Path, item: str, scope: str) -> None:
    source = "// A reason that is long enough.\n#[derive(Debug)]\n#[allow(dead_code)]\n" + item + FOLLOWING_FN
    assert _scopes(tmp_path, source) == [scope]


def test_struct_allowance_does_not_key_to_the_next_fn(tmp_path: Path) -> None:
    source = "#[allow(dead_code)]\npub struct ResolvedFilter {\n    x: u8,\n}\n" + FOLLOWING_FN
    assert _scopes(tmp_path, source) == ["struct:ResolvedFilter"]


def test_identity_ignores_comment_lines_between_items(tmp_path: Path) -> None:
    compact = "#[allow(dead_code)]\npub struct S;\nfn a() {}\n"
    padded = "#[allow(dead_code)]\npub struct S;\n// one\n// two\n// three\nfn b() {}\n"
    assert _scopes(tmp_path, compact) == _scopes(tmp_path, padded) == ["struct:S"]


def test_statement_level_allowance_keys_to_its_statement(tmp_path: Path) -> None:
    source = "fn f(x: u8) {\n    match x {\n        #[allow(unreachable_patterns)]\n        _ => {}\n    }\n}\n"
    assert _scopes(tmp_path, source) == ["statement:_ => {}"]


def test_attribute_quoted_in_a_line_comment_is_not_an_allowance(tmp_path: Path) -> None:
    source = "// kept under `#[allow(dead_code)]` until a caller lands\n#[allow(dead_code)]\nimpl S {}\n"
    assert _scopes(tmp_path, source) == ["impl:impl S"]


def test_self_test_passes() -> None:
    _MODULE.self_test()
