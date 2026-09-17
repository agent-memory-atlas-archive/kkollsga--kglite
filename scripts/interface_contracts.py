#!/usr/bin/env python3
"""Capture deterministic public-interface contracts for reviewable baselines."""

from __future__ import annotations

import argparse
import inspect
import json
from pathlib import Path
import subprocess
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
PYTHON_BASELINE = ROOT / "tests" / "api-baselines" / "python-api.json"
CLI_BASELINE = ROOT / "tests" / "api-baselines" / "cli-interface.json"
#: Every subcommand whose `--help` the golden pins. A nested group is a tuple
#: of argv words, so `okf check` is captured as its own entry rather than
#: hiding behind the group's one-line summary.
CLI_COMMANDS: dict[str, tuple[str, ...]] = {
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
    "okf": ("okf",),
    "okf check": ("okf", "check"),
    "okf build": ("okf", "build"),
}
#: argv → the error contract it must keep. A run whose exit code or stderr
#: moves is a change to what an operator sees when they get it wrong.
CLI_ERRORS: dict[str, tuple[str, ...]] = {
    "unknown_subcommand": ("unknown-command",),
    "missing_query_args": ("query",),
    "graph_subcommand_conflict": ("graph.kgl", "query", "graph.kgl", "RETURN 1"),
    # A typo inside a subcommand group has no `[GRAPH]` positional to fall
    # through to, so clap must reject it rather than do something else.
    "mistyped_okf_sub_verb": ("okf", "chekc", "vault"),
}
PROTOCOL_MEMBERS = {"__enter__", "__exit__", "__iter__", "__len__", "__next__"}
NONCONSTRUCTIBLE = {"FrozenGraph", "ResultIter", "ResultView", "Session", "Transaction"}


def _signature(value: Any) -> str | None:
    try:
        return str(inspect.signature(value))
    except (TypeError, ValueError):
        return None


def _member_contract(cls: type, name: str, raw: Any) -> dict[str, Any]:
    resolved = getattr(cls, name)
    if isinstance(raw, property):
        return {"kind": "property"}
    signature = _signature(resolved)
    if signature is not None:
        return {"kind": "method", "signature": signature}
    return {"kind": "attribute", "type": type(resolved).__name__}


def capture_python_api() -> dict[str, Any]:
    """Return the runtime Python surface in a stable JSON-ready shape."""
    import kglite

    exports = list(kglite.__all__)
    objects: dict[str, Any] = {}
    for name in exports:
        value = getattr(kglite, name)
        if inspect.isclass(value):
            member_names = sorted(
                member for member in value.__dict__ if (not member.startswith("_") or member in PROTOCOL_MEMBERS)
            )
            objects[name] = {
                "kind": "class",
                "module": value.__module__,
                "bases": [base.__name__ for base in value.__bases__],
                "constructible": name not in NONCONSTRUCTIBLE,
                "constructor": _signature(value),
                "members": {member: _member_contract(value, member, value.__dict__[member]) for member in member_names},
            }
        elif callable(value):
            objects[name] = {"kind": "function", "signature": _signature(value)}
        else:
            objects[name] = {"kind": "constant", "type": type(value).__name__}

    non_exported_runtime = {
        name: type(value).__name__
        for name, value in sorted(vars(kglite).items())
        if not name.startswith("_") and name not in exports and not inspect.ismodule(value)
    }
    return {
        "schema_version": 1,
        "exports": exports,
        "non_exported_runtime": non_exported_runtime,
        "objects": objects,
    }


def _normalize(value: str) -> str:
    """Trailing whitespace is a terminal-width artefact, not a contract."""
    return "\n".join(line.rstrip() for line in value.strip().splitlines()) + "\n"


def capture_cli_contract(binary: Path | str) -> dict[str, Any]:
    """Return the shipped CLI's help and error surface in a stable shape.

    One capture for the writer below and for
    `tests/test_cli_interface_contract.py`, so a refreshed golden cannot
    describe something other than what the test compares.
    """

    def run(*args: str) -> subprocess.CompletedProcess[str]:
        # stdin closed, always: an argv the root command does not recognise
        # becomes its `[GRAPH]` positional and starts the interactive shell,
        # which would block here on a terminal.
        return subprocess.run([str(binary), *args], input="", capture_output=True, text=True, timeout=30)

    help_text = {"root": _normalize(run("--help").stdout)}
    for name, command in CLI_COMMANDS.items():
        help_text[name] = _normalize(run(*command, "--help").stdout)

    errors = {}
    for name, args in CLI_ERRORS.items():
        proc = run(*args)
        errors[name] = {"code": proc.returncode, "stderr": _normalize(proc.stderr)}
    return {"help": help_text, "errors": errors}


def _cli_binary() -> Path:
    """The newest built `kglite`, resolved exactly as the tests resolve it."""
    import sys

    sys.path.insert(0, str(ROOT))
    from tests.conftest import workspace_binary

    binary = workspace_binary("kglite")
    if not binary.exists():
        raise SystemExit(f"{binary} is not built — run `cargo build -p kglite-cli` first")
    return binary


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help=f"write {PYTHON_BASELINE.relative_to(ROOT)}")
    parser.add_argument(
        "--write-cli-interface",
        action="store_true",
        help=f"write {CLI_BASELINE.relative_to(ROOT)} from the built `kglite` binary",
    )
    args = parser.parse_args()
    if args.write_cli_interface:
        rendered = json.dumps(capture_cli_contract(_cli_binary()), indent=2, sort_keys=True) + "\n"
        CLI_BASELINE.write_text(rendered, encoding="utf-8", newline="\n")
        print(f"wrote {CLI_BASELINE.relative_to(ROOT)}")
        return 0
    rendered = json.dumps(capture_python_api(), indent=2, sort_keys=True) + "\n"
    if args.write:
        PYTHON_BASELINE.write_text(rendered, encoding="utf-8", newline="\n")
        print(f"wrote {PYTHON_BASELINE.relative_to(ROOT)}")
    else:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
