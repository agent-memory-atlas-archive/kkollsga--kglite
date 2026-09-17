"""The round-trip contract through the Python surface (VAULT.md §10.9).

The Rust corpus in ``crates/kglite/src/okf/export/roundtrip_tests.rs`` proves
the properties over several vaults. This file proves the wheel's own three
calls compose the same way — ``okf.build`` → ``okf.export`` → ``okf.build`` —
and that an exported vault passes ``okf.validate``, because an export that
fails its own validator is a bug however good the bytes look.
"""

from __future__ import annotations

from collections import Counter
from pathlib import Path

from kglite import okf

VAULT = Path(__file__).parent / "fixtures" / "okf" / "golden" / "vault"


def _build(path: Path):
    return okf.build(str(path), dialect="obsidian")


def _labels(graph) -> Counter:
    rows = graph.cypher("MATCH (n) RETURN labels(n)[0] AS l").to_list()
    return Counter(r["l"] for r in rows)


def _edges(graph) -> Counter:
    rows = graph.cypher("MATCH ()-[r]->() RETURN type(r) AS t").to_list()
    return Counter(r["t"] for r in rows)


def _nodes(graph) -> dict:
    """Every node's properties, keyed by ``(label, concept_id)``."""
    rows = graph.cypher("MATCH (n) RETURN labels(n)[0] AS l, n.concept_id AS id, properties(n) AS p").to_list()
    return {(r["l"], r["id"]): {k: str(v) for k, v in sorted(r["p"].items())} for r in rows}


def _tree(root: Path) -> dict[str, bytes]:
    return {str(p.relative_to(root)).replace("\\", "/"): p.read_bytes() for p in sorted(root.rglob("*")) if p.is_file()}


def _round(graph, out: Path, source_root: Path):
    """One export-then-import round, returning the tree and the new graph."""
    out.mkdir(parents=True, exist_ok=True)
    report = okf.export(graph, str(out), source_root=str(source_root))
    assert report.ok, report.refusals
    return _tree(out), _build(out)


class TestRoundTrip:
    """``import ∘ export ∘ import`` and ``export ∘ import ∘ export``."""

    def test_an_exported_vault_passes_its_own_validator(self, tmp_path):
        graph = _build(VAULT)
        out = tmp_path / "vault"
        out.mkdir()
        okf.export(graph, str(out), source_root=str(VAULT))
        report = okf.validate(str(out), dialect="obsidian")
        assert report.errors == [], report.errors
        assert report.ok

    def test_export_import_export_is_byte_identical(self, tmp_path):
        first, graph2 = _round(_build(VAULT), tmp_path / "a", VAULT)
        second, _graph3 = _round(graph2, tmp_path / "b", tmp_path / "a")
        assert sorted(first) == sorted(second)
        for rel, data in first.items():
            assert data == second[rel], rel

    def test_import_export_import_reaches_a_fixed_point(self, tmp_path):
        """The losses are one-shot, so the second round changes nothing."""
        _first, graph2 = _round(_build(VAULT), tmp_path / "a", VAULT)
        _second, graph3 = _round(graph2, tmp_path / "b", tmp_path / "a")
        assert _labels(graph2) == _labels(graph3)
        assert _edges(graph2) == _edges(graph3)
        assert _nodes(graph2) == _nodes(graph3)

    def test_the_documented_losses_are_the_only_differences(self, tmp_path):
        """§10.9, at the Python surface: the notes survive, the declarations do not."""
        graph = _build(VAULT)
        before = _labels(graph)
        _tree_a, graph2 = _round(graph, tmp_path / "a", VAULT)
        after = _labels(graph2)

        # The notes themselves, label for label.
        for label in ("Article", "Initiative"):
            assert after[label] == before[label], label
        # Loss 4 — the `hubs:` declaration lives in `.kglite/vault.yaml`, which
        # a graph does not carry, so the hub and its edges do not come back.
        assert before["Keyword"] > 0
        assert "Keyword" not in after
        assert "HAS_KEYWORD" not in _edges(graph2)
        # Loss 3 — everything the prose decides is re-made from the same prose.
        for regenerated in ("Tag", "Image", "Concept"):
            assert after[regenerated] == before[regenerated], regenerated

    def test_the_report_counts_what_it_dropped(self, tmp_path):
        graph = _build(VAULT)
        out = tmp_path / "vault"
        out.mkdir()
        carried = okf.export(graph, str(out), source_root=str(VAULT))
        assert carried.edge_properties_dropped > 0
        assert (carried.attachments_copied, carried.attachments_unresolved) == (3, 0)

        rootless = tmp_path / "no-root"
        rootless.mkdir()
        without = okf.export(graph, str(rootless))
        assert (without.attachments_copied, without.attachments_unresolved) == (0, 3)
        # Loss 2 — the references now name files the exported vault does not
        # hold, and the re-import says so. One reference was already missing in
        # the source vault, so the count has to *grow*, not merely be non-zero.
        source_missing = okf.validate(str(VAULT), dialect="obsidian").counts["missing_attachments"]
        assert source_missing == 1
        reimported = okf.validate(str(rootless), dialect="obsidian").counts["missing_attachments"]
        assert reimported > source_missing
