"""`kglite okf check|build` — the CLI front end for the vault format.

Exercises the shipped binary, not the library: the exit code, the printed
report, the JSON shape and the written `.kgl` are what an operator and a
converter's CI actually see.
"""

from __future__ import annotations

import json
from pathlib import Path
import subprocess

import pytest

import kglite
from kglite import okf
from tests.test_cli_shell_smoke import BINARY, SKIP_REASON

ROOT = Path(__file__).resolve().parent.parent
GOLDEN_VAULT = ROOT / "tests" / "fixtures" / "okf" / "golden" / "vault"

pytestmark = pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")


def _run(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run([str(BINARY), *args], input="", capture_output=True, text=True, timeout=60)


@pytest.fixture
def clean_vault(tmp_path: Path) -> Path:
    """A vault with nothing to report: no errors, no warnings."""
    (tmp_path / "notes").mkdir()
    (tmp_path / "notes" / "alpha.md").write_text("Links to [[beta]].\n", encoding="utf-8")
    (tmp_path / "notes" / "beta.md").write_text("Plain prose.\n", encoding="utf-8")
    return tmp_path


@pytest.fixture
def warned_vault(tmp_path: Path) -> Path:
    """A vault whose only finding is a warning — what `--strict` is for."""
    (tmp_path / "a.md").write_text("Links to [[Nowhere]].\n", encoding="utf-8")
    return tmp_path


def test_check_exits_zero_on_a_clean_vault(clean_vault: Path):
    proc = _run("okf", "check", str(clean_vault))
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert proc.stdout.startswith("files scanned: 2\n")
    assert "errors: none" in proc.stdout
    assert "warnings: none" in proc.stdout


def test_check_exits_nonzero_on_the_golden_vault_and_names_the_error():
    """The committed fixture carries a deliberate id collision (VAULT.md §3)."""
    proc = _run("okf", "check", str(GOLDEN_VAULT))
    assert proc.returncode != 0
    assert "id collision" in proc.stdout
    assert "dangling link: `Missing`" in proc.stdout


def test_strict_flips_a_warnings_only_vault(warned_vault: Path):
    lenient = _run("okf", "check", str(warned_vault))
    assert lenient.returncode == 0, lenient.stdout + lenient.stderr
    strict = _run("okf", "check", str(warned_vault), "--strict")
    assert strict.returncode != 0
    # Same report either way: --strict changes the verdict, not the findings.
    assert strict.stdout == lenient.stdout


def test_json_parses_and_carries_the_same_verdict(warned_vault: Path):
    proc = _run("okf", "check", str(warned_vault), "--json")
    assert proc.returncode == 0
    payload = json.loads(proc.stdout)
    assert payload["ok"] is True
    assert payload["strict"] is False
    assert payload["errors"] == []
    assert payload["warnings"] == ["dangling link: `Nowhere`"]
    assert payload["counts"]["files_scanned"] == 1
    assert payload["counts"]["nodes_by_label"] == {"Concept": 1, "Note": 1}

    strict = _run("okf", "check", str(warned_vault), "--strict", "--json")
    assert strict.returncode != 0
    strict_payload = json.loads(strict.stdout)
    assert strict_payload["ok"] is False
    assert strict_payload["warnings"] == payload["warnings"]


def test_the_library_default_reads_a_vault_exactly_as_the_command_does():
    """VAULT.md §9: the two are one read, so they cannot start from two dialects.

    `okf.validate` defaulted to `okf` while `kglite okf check` defaulted to
    `obsidian`, so the Python half of the same check silently resolved no
    wikilink, minted no `Tag`, read no `.kglite/vault.yaml` — and reported a
    *healthier* vault than the command did on the same directory.
    """
    proc = _run("okf", "check", str(GOLDEN_VAULT), "--json")
    printed = json.loads(proc.stdout)
    report = okf.validate(str(GOLDEN_VAULT))
    assert report.errors == printed["errors"]
    assert report.warnings == printed["warnings"]
    assert report.counts["nodes_by_label"] == printed["counts"]["nodes_by_label"]
    assert report.counts["edges_by_type"] == printed["counts"]["edges_by_type"]
    # …and the bundle dialect is still one keyword away, for a caller of
    # `okf.build`, which keeps the `okf` default it has always had.
    bundle = okf.validate(str(GOLDEN_VAULT), dialect="okf")
    assert bundle.counts["nodes_by_label"] != report.counts["nodes_by_label"]


def test_an_unknown_dialect_is_refused(clean_vault: Path):
    proc = _run("okf", "check", str(clean_vault), "--dialect", "obsidan")
    assert proc.returncode == 2
    assert "obsidian" in proc.stderr


def test_a_missing_directory_is_an_error_not_a_report(tmp_path: Path):
    proc = _run("okf", "check", str(tmp_path / "nope"))
    assert proc.returncode != 0
    assert "does not exist" in proc.stderr


def test_build_writes_a_loadable_kgl_matching_okf_build(tmp_path: Path):
    output = tmp_path / "vault.kgl"
    proc = _run("okf", "build", str(GOLDEN_VAULT), "-o", str(output))
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert output.exists()
    # The report goes to stderr; stdout names the file it wrote.
    assert "files scanned: 13" in proc.stderr
    assert str(output) in proc.stdout

    loaded = kglite.load(str(output))
    reference = okf.build(str(GOLDEN_VAULT), dialect="obsidian")
    for query in (
        "MATCH (n) RETURN labels(n)[0] AS k, count(*) AS c ORDER BY k",
        "MATCH ()-[r]->() RETURN type(r) AS k, count(*) AS c ORDER BY k",
    ):
        assert loaded.cypher(query).to_list() == reference.cypher(query).to_list(), query


def test_export_writes_a_vault_from_a_built_kgl(tmp_path: Path):
    built = tmp_path / "vault.kgl"
    assert _run("okf", "build", str(GOLDEN_VAULT), "-o", str(built)).returncode == 0

    out = tmp_path / "out"
    proc = _run("okf", "export", str(built), str(out), "--source-root", str(GOLDEN_VAULT))
    assert proc.returncode == 0, proc.stdout + proc.stderr
    # The report goes to stderr; stdout names the directory it wrote.
    assert "files written:" in proc.stderr
    assert "refusals: none" in proc.stderr
    assert str(out) in proc.stdout
    assert (out / ".kglite" / "export-manifest.json").is_file()
    assert (out / "img" / "faults.png").is_file()

    # And it is the same vault `okf.export` writes from the same graph.
    direct = tmp_path / "direct"
    okf.export(okf.build(str(GOLDEN_VAULT), dialect="obsidian"), str(direct), source_root=str(GOLDEN_VAULT))

    def listing(root: Path) -> list[str]:
        return sorted(str(p.relative_to(root)) for p in root.rglob("*") if p.is_file())

    assert listing(out) == listing(direct)


def test_export_exits_nonzero_when_it_refuses_a_file(tmp_path: Path):
    built = tmp_path / "vault.kgl"
    _run("okf", "build", str(GOLDEN_VAULT), "-o", str(built))
    out = tmp_path / "out"
    (out / "Article").mkdir(parents=True)
    (out / "Article" / "welcome.md").write_text("somebody else's file\n", encoding="utf-8")

    refused = _run("okf", "export", str(built), str(out))
    assert refused.returncode != 0
    assert "Article/welcome.md: not written by an export" in refused.stderr
    assert (out / "Article" / "welcome.md").read_text(encoding="utf-8") == "somebody else's file\n"

    forced = _run("okf", "export", str(built), str(out), "--force")
    assert forced.returncode == 0, forced.stdout + forced.stderr
    assert (out / "Article" / "welcome.md").read_text(encoding="utf-8") != "somebody else's file\n"


def test_export_declares_an_edge_table_from_the_command_line(tmp_path: Path):
    """`--edge-table TYPE=Heading` is `export.edge_tables:` for a graph whose
    vault does not declare one (`VAULT.md` §7.3, §10.6). The warning about the
    missing import rule rides the report on stderr."""
    vault = tmp_path / "vault"
    (vault / "Note").mkdir(parents=True)
    (vault / "Note" / "paper.md").write_text('---\nworked_on_by: "[[alice]]"\n---\n# Paper\n', encoding="utf-8")
    (vault / "Note" / "alice.md").write_text("Alice.\n", encoding="utf-8")
    built = tmp_path / "vault.kgl"
    assert _run("okf", "build", str(vault), "-o", str(built)).returncode == 0

    out = tmp_path / "out"
    proc = _run(
        "okf",
        "export",
        str(built),
        str(out),
        "--source-root",
        str(vault),
        "--edge-table",
        "WORKED_ON_BY=Worked on by",
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    body = (out / "Note" / "paper.md").read_text(encoding="utf-8")
    assert "## Worked on by\n\n| target |\n| --- |\n| [[alice]] |\n" in body, body
    assert "worked_on_by:" not in body

    bad = _run("okf", "export", str(built), str(tmp_path / "out2"), "--edge-table", "WORKED_ON_BY")
    assert bad.returncode != 0
    assert "expected TYPE=HEADING" in bad.stderr


def test_export_of_a_missing_graph_is_an_error(tmp_path: Path):
    proc = _run("okf", "export", str(tmp_path / "nope.kgl"), str(tmp_path / "out"))
    assert proc.returncode != 0
    assert "failed to open" in proc.stderr


# ── `kglite okf status` — the lifecycle question (VAULT.md §12) ──────────────


def _vault_beside_its_graph(tmp_path: Path) -> tuple[Path, Path]:
    """A vault, and a `.kgl` path *outside* it.

    Every non-hidden file under the root is a candidate attachment, so a `.kgl`
    written into the vault is itself a change to the vault and the directory
    would read stale the moment it was built.
    """
    vault = tmp_path / "vault"
    (vault / "notes").mkdir(parents=True)
    (vault / "notes" / "alpha.md").write_text("Links to [[beta]].\n", encoding="utf-8")
    (vault / "notes" / "beta.md").write_text("Plain prose.\n", encoding="utf-8")
    return vault, tmp_path / "vault.kgl"


def test_status_prints_the_fingerprint_and_agrees_with_the_library(clean_vault: Path):
    proc = _run("okf", "status", str(clean_vault))
    assert proc.returncode == 0, proc.stdout + proc.stderr
    printed, path = proc.stdout.split(maxsplit=1)
    assert path.strip() == str(clean_vault)
    assert int(printed, 16) == okf.fingerprint(str(clean_vault), dialect="obsidian")


def test_status_is_current_then_stale_as_the_vault_moves(tmp_path: Path):
    vault, graph = _vault_beside_its_graph(tmp_path)
    assert _run("okf", "build", str(vault), "-o", str(graph)).returncode == 0

    current = _run("okf", "status", str(vault), "--graph", str(graph))
    assert current.returncode == 0, current.stdout + current.stderr
    assert current.stdout.startswith("current")

    (vault / "notes" / "gamma.md").write_text("A new note.\n", encoding="utf-8")
    stale = _run("okf", "status", str(vault), "--graph", str(graph))
    assert stale.returncode == 1, stale.stdout + stale.stderr
    assert stale.stdout.startswith("stale")
    # The verdict is the whole diagnostic: no second, vaguer explanation.
    assert "Error:" not in stale.stderr


def test_status_refuses_a_graph_that_carries_no_provenance(tmp_path: Path):
    """A `.kgl` that was not built from a directory cannot answer the question,
    which is an error rather than a third verdict — the CLI has two exit codes."""
    vault, plain = _vault_beside_its_graph(tmp_path)
    kglite.KnowledgeGraph().save(str(plain))

    proc = _run("okf", "status", str(vault), "--graph", str(plain))
    assert proc.returncode == 1
    assert "no vault provenance" in proc.stderr
    assert proc.stdout == ""
