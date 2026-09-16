"""Clap help, error, and version contracts for the shipped `kglite` CLI."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess

import pytest

from tests.test_cli_shell_smoke import BINARY, SKIP_REASON

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "tests" / "api-baselines" / "cli-interface.json"
COMMANDS = {
    "query": ("query",),
    "write": ("write",),
    "ready-set": ("ready-set",),
    "skill": ("skill",),
    "describe": ("describe",),
    "session": ("session",),
    "export-text": ("export-text",),
    "diff": ("diff",),
    "export-sqlite": ("export-sqlite",),
    "migrate": ("migrate",),
    "schema-version": ("schema-version",),
}

requires_binary = pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")


def _run(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run([str(BINARY), *args], capture_output=True, text=True, timeout=30)


def _shell_input(*argv: str) -> subprocess.CompletedProcess[str]:
    """Run `argv` with stdin closed — an argv that falls through to the REPL
    would otherwise block on the interactive prompt."""
    return subprocess.run(list(argv), input="", capture_output=True, text=True, timeout=30)


def _text(value: str) -> str:
    return "\n".join(line.rstrip() for line in value.strip().splitlines()) + "\n"


def capture_cli_contract() -> dict:
    help_text = {"root": _text(_run("--help").stdout)}
    for name, command in COMMANDS.items():
        help_text[name] = _text(_run(*command, "--help").stdout)

    errors = {}
    for name, args in {
        "unknown_subcommand": ("unknown-command",),
        "missing_query_args": ("query",),
        "graph_subcommand_conflict": ("graph.kgl", "query", "graph.kgl", "RETURN 1"),
    }.items():
        proc = _run(*args)
        errors[name] = {"code": proc.returncode, "stderr": _text(proc.stderr)}
    return {"help": help_text, "errors": errors}


@requires_binary
def test_cli_help_and_error_contract_matches_baseline():
    assert capture_cli_contract() == json.loads(BASELINE.read_text(encoding="utf-8"))


@requires_binary
def test_cli_offers_the_skill_command():
    """`skill` is the offline read of the graph-carried skills layer.

    Asserted on both halves, so a partial landing — a help entry with no
    dispatch, or dispatch with no help entry — still fails. `kglite` has no
    unknown-subcommand error (a stray word becomes the `[GRAPH]` positional),
    so dispatch is proven by the usage line the command prints for itself.
    """
    root_help = _run("--help")
    assert root_help.returncode == 0
    assert "skill" in root_help.stdout

    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    assert "skill" in baseline["help"]

    usage = _run("skill", "--help")
    assert usage.returncode == 0
    assert "Usage: kglite skill [OPTIONS] <GRAPH> [NAME]" in usage.stdout, usage.stdout


def test_cli_docs_route_skill_installation_to_codingest():
    """The code-review Agent Skill is installed by codingest, not by this CLI.

    `kglite skill` reads what a `.kgl` carries; it installs nothing, so the
    docs must keep pointing installation at the project that owns it.
    """
    for path in (
        ROOT / "crates" / "kglite-cli" / "README.md",
        ROOT / "docs" / "operators" / "cli.md",
    ):
        text = path.read_text(encoding="utf-8")
        assert "codingest skill install" in text, path
        commands = [line.strip().lstrip("$ ").strip() for line in text.splitlines()]
        taught = [line for line in commands if line.startswith("kglite skill install")]
        assert not taught, f"{path} still teaches: {taught}"


@requires_binary
def test_cli_version_tracks_workspace_version():
    metadata = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    packages = json.loads(metadata.stdout)["packages"]
    expected = next(package["version"] for package in packages if package["name"] == "kglite-cli")
    proc = _run("--version")
    assert proc.returncode == 0
    assert proc.stdout.strip() == f"kglite {expected}"
