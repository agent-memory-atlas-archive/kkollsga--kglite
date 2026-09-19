"""Clap help, error, and version contracts for the shipped `kglite` CLI."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys

import pytest

from tests.test_cli_shell_smoke import BINARY, SKIP_REASON

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))

from interface_contracts import capture_cli_contract  # noqa: E402

BASELINE = ROOT / "tests" / "api-baselines" / "cli-interface.json"

requires_binary = pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")


def _run(*args: str) -> subprocess.CompletedProcess[str]:
    """stdin closed: an argv the root command does not recognise becomes its
    `[GRAPH]` positional and starts the interactive shell."""
    return subprocess.run([str(BINARY), *args], input="", capture_output=True, text=True, timeout=30)


@requires_binary
def test_cli_help_and_error_contract_matches_baseline():
    """The golden is written by `make refresh-cli-interface`, which captures
    it through this same function — so a refresh cannot record something other
    than what this compares."""
    assert capture_cli_contract(BINARY) == json.loads(BASELINE.read_text(encoding="utf-8"))


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


@requires_binary
def test_cli_offers_the_okf_commands():
    """`okf check` / `okf build` are the vault front end (VAULT.md).

    Both halves, like the skill test: a help entry with no dispatch, or a
    dispatch with no help entry, still fails. The group *does* have an
    unknown-subcommand error — there is no `[GRAPH]` positional inside it to
    swallow a typo — so that is asserted directly rather than inferred.
    """
    root_help = _run("--help")
    assert root_help.returncode == 0
    assert "okf" in root_help.stdout

    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    for entry in ("okf", "okf check", "okf build", "okf open"):
        assert entry in baseline["help"], entry

    group = _run("okf", "--help")
    assert group.returncode == 0
    assert "Usage: kglite okf <COMMAND>" in group.stdout, group.stdout

    check = _run("okf", "check", "--help")
    assert check.returncode == 0
    assert "Usage: kglite okf check [OPTIONS] <DIRECTORY>" in check.stdout, check.stdout
    for flag in ("--dialect", "--strict", "--json"):
        assert flag in check.stdout, flag

    build = _run("okf", "build", "--help")
    assert build.returncode == 0
    assert "Usage: kglite okf build [OPTIONS] --output <OUTPUT> <DIRECTORY>" in build.stdout, build.stdout

    # `open` is the one vault command that needs no output path — the vault
    # carries the graph — so the absence of `--output` in its usage line is
    # the contract, not an omission.
    opened = _run("okf", "open", "--help")
    assert opened.returncode == 0
    assert "Usage: kglite okf open [OPTIONS] <DIRECTORY>" in opened.stdout, opened.stdout
    for flag in ("--cache", "--dialect"):
        assert flag in opened.stdout, flag

    typo = _run("okf", "chekc", "vault")
    assert typo.returncode == 2, typo.stdout + typo.stderr
    assert "unrecognized subcommand 'chekc'" in typo.stderr, typo.stderr


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
