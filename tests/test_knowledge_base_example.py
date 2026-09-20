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


def test_native_api_facts_preserve_owner_signature_defaults_returns_and_conditions(tmp_path):
    vault = built(tmp_path)
    graph = okf.open(vault, dialect="obsidian", cache=False)

    articles = graph.cypher("MATCH (n:Article) RETURN count(n) AS total").to_list()[0]["total"]
    symbol = graph.cypher(
        "MATCH (s:ApiSymbol) RETURN s.qualified_name AS name, s.owner AS owner, "
        "s.signature AS signature, s.source_signature AS source_signature, "
        "s.returns AS returns, s.provenance AS provenance, s.source_sha256 AS source_sha256"
    ).to_list()
    parameters = graph.cypher(
        "MATCH (p:ApiParameter) RETURN p.name AS name, p.type AS type, p.default AS default, "
        "p.owner AS owner, p.source_anchor AS source_anchor, p.source_sha256 AS source_sha256 ORDER BY name"
    ).to_list()
    returns = graph.cypher(
        "MATCH (r:ApiReturn) RETURN r.name AS name, r.type AS type, r.owner AS owner, "
        "r.source_anchor AS source_anchor, r.source_sha256 AS source_sha256"
    ).to_list()
    exceptions = graph.cypher(
        "MATCH (s:ApiSymbol)<-[:PARENT_SECTION]-(h:Section)-[:RAISES]->(e:ApiException) "
        "RETURN e.condition_id AS condition_id, e.condition AS condition, "
        "e.exception AS exception, e.source_anchor AS source_anchor, e.source_sha256 AS source_sha256 "
        "ORDER BY condition_id"
    ).to_list()
    attached_parameter_count = graph.cypher(
        "MATCH (:ApiSymbol)<-[:PARENT_SECTION]-(:Section)-[:HAS_PARAMETER]->(p:ApiParameter) RETURN count(p) AS total"
    ).to_list()[0]["total"]
    attached_return_count = graph.cypher(
        "MATCH (:ApiSymbol)<-[:PARENT_SECTION]-(:Section)-[:HAS_RETURN]->(r:ApiReturn) RETURN count(r) AS total"
    ).to_list()[0]["total"]

    source_hash = kb.MANIFEST["source_inventory"][1]["sha256"]
    assert articles == 3
    assert attached_parameter_count == 3
    assert attached_return_count == 1
    assert symbol == [
        {
            "name": "Client.connect",
            "owner": "Client",
            "signature": '(controller_id: str, timeout: int = 30, options: dict = {"mode": "safe"}) → Session',
            "source_signature": kb.API_SOURCE_SIGNATURE,
            "returns": "Session",
            "provenance": "Sources/api.html#connect",
            "source_sha256": source_hash,
        }
    ]
    assert [(row["name"], row["default"]) for row in parameters] == [
        ("controller_id", "required"),
        ("options", '{"mode": "safe"}'),
        ("timeout", "30"),
    ]
    assert returns == [
        {
            "name": "return",
            "type": "Session",
            "owner": "Client",
            "source_anchor": "api.html#connect",
            "source_sha256": source_hash,
        }
    ]
    assert [(row["condition_id"], row["condition"], row["exception"]) for row in exceptions] == [
        ("connection-error-unreachable", "controller cannot be reached", "ConnectionError"),
        ("value-error-empty-id", "controller_id is empty", "ValueError"),
        ("value-error-negative-timeout", "timeout is negative", "ValueError"),
    ]
    assert all(row["source_anchor"] == "api.html#connect" for row in exceptions)
    assert all(row["source_sha256"] == source_hash for row in parameters + exceptions)


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
    (output / "owned.txt").write_text("keep", encoding="utf-8")
    with pytest.raises(ValueError, match="absent or empty"):
        kb.build(output)
    assert (output / "owned.txt").read_text(encoding="utf-8") == "keep"


def test_share_refuses_a_symlink_to_content_outside_the_vault(tmp_path):
    vault = built(tmp_path)
    outside = tmp_path / "outside.txt"
    outside.write_text("not shareable", encoding="utf-8")
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
