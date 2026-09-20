"""Executable contracts for the bounded synthetic knowledge-base example."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import shutil
import zipfile

import pytest

from kglite import okf

SCRIPT = Path(__file__).parents[1] / "examples" / "knowledge_base" / "knowledge_base.py"
SPEC = importlib.util.spec_from_file_location("knowledge_base_example", SCRIPT)
assert SPEC and SPEC.loader
kb = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(kb)


def built(tmp_path: Path) -> Path:
    vault = tmp_path / "vault"
    kb.build(vault)
    return vault


def test_portable_lifecycle_recovers_late_warning_and_expands_grid(tmp_path, capsys):
    vault = built(tmp_path)
    kb.query(vault, "procedure", 2)
    payload = json.loads(capsys.readouterr().out.split("\n", 1)[1])
    blocks = [block for page in payload["pages"] for block in page]

    assert payload["returned"] == payload["total"] == len(blocks)
    assert len(payload["pages"]) > 1
    assert any(block["kind"] == "warning" and "remove power" in block["text"] for block in blocks)
    grid = next(json.loads(block["text"]) for block in blocks if block["kind"] == "table")
    assert grid[1] == ["Mode", "Minimum", "Maximum"]

    archive = tmp_path / "public.zip"
    kb.share(vault, archive)
    kb.verify_share(archive)
    with zipfile.ZipFile(archive) as public:
        assert all(not name.startswith("Memories/") for name in public.namelist())
        assert ".kglite/recipes/article-index.md" in public.namelist()
        assert kb.MANIFEST["private_canary"].encode() not in b"".join(public.read(n) for n in public.namelist())


@pytest.mark.parametrize(
    "relative, old, new, message",
    [
        ("Articles/index.md", "- `procedure.html`", "", "rendered reference missing"),
        (
            "Articles/client-api.md",
            "Raises ValueError when timeout is negative.",
            "Raises ValueError when controller_id is empty.",
            "repeated exception conditions",
        ),
        (
            "Articles/reset-controller.md",
            "> [!warning] Warning: do not remove power after Apply until Ready appears.",
            "",
            "procedure warning missing",
        ),
    ],
)
def test_check_fails_for_independent_rendered_semantic_loss(tmp_path, relative, old, new, message):
    vault = built(tmp_path)
    path = vault / relative
    path.write_text(path.read_text(encoding="utf-8").replace(old, new, 1), encoding="utf-8")
    with pytest.raises(ValueError, match=message):
        kb.check(vault)


def test_inventory_covers_hidden_attachment_and_download(tmp_path):
    vault = built(tmp_path)
    report = json.loads((vault / "build-report.json").read_text(encoding="utf-8"))
    by_path = {item["path"]: item for item in report["inventory"]}
    assert {"Sources/.catalog-version", "Sources/warning.svg", "Sources/reset-checklist.txt"} <= by_path.keys()
    assert all(item["size"] == (vault / path).stat().st_size for path, item in by_path.items())


def test_reviewed_annotation_hash_is_not_rewritten_to_bless_changed_source(tmp_path):
    source = tmp_path / "inputs"
    shutil.copytree(kb.HERE, source, ignore=shutil.ignore_patterns("__pycache__"))
    procedure = source / "originals" / "procedure.html"
    procedure.write_text(procedure.read_text(encoding="utf-8") + "\n<!-- changed -->\n", encoding="utf-8")
    with pytest.raises(ValueError, match="stale source evidence"):
        kb.build(tmp_path / "stale", source)


def test_memory_correction_and_deletion_rebuild_from_copied_inputs(tmp_path):
    source = tmp_path / "inputs"
    shutil.copytree(kb.HERE, source, ignore=shutil.ignore_patterns("__pycache__"))
    memory = source / "private" / "memory.md"
    memory.write_text(
        memory.read_text(encoding="utf-8").replace("PRIVATE-CANARY-7F3A", "CORRECTED-ACCESS"), encoding="utf-8"
    )
    corrected = tmp_path / "corrected"
    kb.build(corrected, source)
    assert "CORRECTED-ACCESS" in (corrected / "Memories" / "memory.md").read_text(encoding="utf-8")

    memory.unlink()
    deleted = tmp_path / "deleted"
    kb.build(deleted, source)
    assert not (deleted / "Memories").exists()
    assert "CORRECTED-ACCESS" not in "".join(
        path.read_text(encoding="utf-8", errors="ignore") for path in deleted.rglob("*") if path.is_file()
    )
    fresh = okf.open(deleted, dialect="obsidian", cache=False)
    remaining = fresh.cypher(
        "MATCH (n) WHERE n.body CONTAINS $canary RETURN count(n) AS found",
        params={"canary": "CORRECTED-ACCESS"},
    ).to_list()[0]["found"]
    assert remaining == 0


def test_verify_share_fails_if_private_canary_is_copied(tmp_path):
    vault = built(tmp_path)
    archive = tmp_path / "leaky.zip"
    with zipfile.ZipFile(archive, "w") as output:
        output.write(vault / "build-report.json", "build-report.json")
        output.writestr("Articles/leak.md", kb.MANIFEST["private_canary"])

    with pytest.raises(ValueError, match="private or derived content leaked"):
        kb.verify_share(archive)


def test_build_refuses_to_mix_with_existing_output(tmp_path):
    output = tmp_path / "existing"
    output.mkdir()
    (output / "owned.txt").write_text("keep")
    with pytest.raises(ValueError, match="absent or empty"):
        kb.build(output)
    assert (output / "owned.txt").read_text() == "keep"


def test_share_refuses_a_symlink_to_content_outside_the_vault(tmp_path):
    vault = built(tmp_path)
    outside = tmp_path / "outside.txt"
    outside.write_text("not shareable")
    (vault / "Articles" / "escape.md").symlink_to(outside)

    with pytest.raises(ValueError, match="regular file inside"):
        kb.share(vault, tmp_path / "public.zip")


def test_build_refuses_symlinked_public_input_from_copied_fixture(tmp_path):
    source = tmp_path / "inputs"
    shutil.copytree(kb.HERE, source, ignore=shutil.ignore_patterns("__pycache__"))
    workflow = source / "workflows" / "reset-review.md"
    workflow.unlink()
    outside = tmp_path / "outside.md"
    outside.write_text("external private text", encoding="utf-8")
    workflow.symlink_to(outside)

    with pytest.raises(ValueError, match="regular file inside"):
        kb.build(tmp_path / "vault", source)


def test_share_refuses_missing_declared_public_root(tmp_path):
    vault = built(tmp_path)
    (vault / "Workflows" / "reset-review.md").unlink()
    (vault / "Workflows").rmdir()

    with pytest.raises(ValueError, match="required public root"):
        kb.share(vault, tmp_path / "public.zip")
