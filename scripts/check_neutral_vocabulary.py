#!/usr/bin/env python3
"""Keep industry-specific example vocabulary out of the tracked tree.

Examples, fixtures, tests and docs use one neutral model: ``Project`` /
``Contract`` / ``Site`` / ``Proposal`` / ``Initiative`` / ``Portfolio`` nodes
joined by ``HAS_HOLDER`` / ``MANAGED_BY`` / ``IN_CONTRACT`` and so on. This
gate fails on the oil-and-gas registry vocabulary those names replaced, so it
cannot creep back through a copied snippet.

Words that are ordinary English in another sense stay legal and are not
patterns here: ``discovery`` (tool or schema discovery), ``licence`` /
``license`` (a software licence, or the permission sense in executor
comments), ``prospect`` only as the domain word. Only the domain forms are
matched: the registry's own names, the label/relationship spellings
(``:Licence``, ``IN_LICENCE``, ``BECAME_DISCOVERY``), and the term
``HAS_OPERATOR``.

Scope: every tracked text file except the allowlist below. An allowlist entry
names a path prefix and the reason that path may carry the vocabulary; add one
only for a file whose subject is an external name it does not control.

Wired into ``make gate`` — pure file reading, no imports of the project.
"""

from __future__ import annotations

from pathlib import Path
import re
import subprocess
import sys

REPO_ROOT = Path(__file__).resolve().parents[1]

# Registry names: matched in any case.
CASE_FOLDED = re.compile(r"sodir|wellbore|licensee|petroleum|oilfield|reservoir|\bprospects?\b", re.I)
# Label and relationship spellings: matched exactly, so the prose words
# "licence" and "discovery" (and the DISCOVERY_STEER constant) stay legal.
EXACT_ONLY = re.compile(
    r"\bHAS_OPERATOR\b|[:'\"`(\[]Licences?\b|\bPetregLicence|\b(?:IN|OF|COVERS)_LICENCE\b|\bLICENCE_TRANSFER\b"
    r"|\b(?:BECAME|IN|OF|INCLUDES)_DISCOVERY\b|\bDISCOVERY_(?:SITE|WELLBORE)\b|[:'\"`(\[]Discovery\b|\bDiscoveries\b"
)

# (path prefix, reason). A prefix ending in "/" covers a directory.
ALLOWLIST: tuple[tuple[str, str], ...] = (
    ("CHANGELOG.md", "released history is a citation and stays as written"),
    ("dev-docs/", "local working state, not user-facing"),
    ("docs/python/migrations/", "migration guides name the removed module paths verbatim"),
    ("stubtest_allowlist.txt", "names removed module identifiers the stub check tolerates"),
    ("mypy_stubtest.ini", "names removed module identifiers the stub check tolerates"),
    (
        "tests/benchmarks/internal/projects_graph_config.json",
        "binds to an external dataset's own CSV paths and column names; the labels are neutral",
    ),
    ("scripts/check_neutral_vocabulary.py", "states the patterns it enforces"),
    ("tests/test_check_neutral_vocabulary.py", "feeds the patterns to the guard"),
    ("tests/api-baselines/", "generated API surface dumps"),
    ("tests/benchmarks/baselines/", "recorded benchmark captures"),
)


def allowed(path: str) -> bool:
    return any(path == prefix or path.startswith(prefix) for prefix, _ in ALLOWLIST)


def violations(files: dict[str, str]) -> list[str]:
    """Return ``path:line: text`` for every forbidden term outside the allowlist."""
    found = []
    for path in sorted(files):
        if allowed(path):
            continue
        for number, line in enumerate(files[path].splitlines(), 1):
            if CASE_FOLDED.search(line) or EXACT_ONLY.search(line):
                found.append(f"{path}:{number}: {line.strip()[:120]}")
    return found


def tracked_text_files() -> dict[str, str]:
    names = subprocess.run(
        ["git", "ls-files", "-z"], cwd=REPO_ROOT, capture_output=True, check=True, text=True
    ).stdout.split("\0")
    files = {}
    for name in filter(None, names):
        path = REPO_ROOT / name
        if not path.is_file():
            continue
        try:
            files[name] = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
    return files


def main() -> int:
    bad = violations(tracked_text_files())
    if not bad:
        print("check_neutral_vocabulary: ok")
        return 0
    print("check_neutral_vocabulary: industry-specific vocabulary found —", file=sys.stderr)
    print("  use the neutral model (Project / Contract / Site / HAS_HOLDER / MANAGED_BY).", file=sys.stderr)
    for entry in bad:
        print(f"  {entry}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
