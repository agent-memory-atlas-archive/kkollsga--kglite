#!/usr/bin/env python3
"""Prose-density ratchet for the user docs.

Measures, per Markdown file, the average words per sentence and the share of
sentences over 30 words (code blocks, headings and tables excluded; list items
count as sentences). A file may not get denser than its recorded baseline:
`--check` fails when a file's average rises more than 1.0 word or its
long-sentence share rises more than 3 points. `--update` rewrites the baseline
(run it after a deliberate rewrite). The thresholds absorb small edits; they
exist so dense prose cannot creep back in unnoticed.

The write-docs skill (`.claude/skills/write-docs`) states the target: about 20
words per sentence and under ~5% of sentences over 30 words.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "scripts" / "doc_density_baseline.json"
TOP_LEVEL = ["README.md", "CYPHER.md", "FLUENT.md", "VAULT.md"]
EXCLUDED_DIRS = {"_build", "_generated", "history"}
AVG_SLACK = 1.0
LONG_SLACK = 3.0
LONG_SENTENCE = 30

_FENCE = re.compile(r"^(```|~~~).*?^\1", re.S | re.M)
_SPLIT = re.compile(r"(?<=[.!?:;])\s+(?=[A-Z`(*\"])")
_LIST_MARK = re.compile(r"^([-*>]|\d+\.)\s+")


def doc_files() -> list[Path]:
    files = [ROOT / name for name in TOP_LEVEL if (ROOT / name).exists()]
    for path in sorted((ROOT / "docs").rglob("*.md")):
        if not EXCLUDED_DIRS.intersection(path.relative_to(ROOT / "docs").parts):
            files.append(path)
    return files


def measure(text: str) -> dict[str, float]:
    text = _FENCE.sub("", text)
    lines = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith(("#", "|", "<", "```")):
            continue
        lines.append(_LIST_MARK.sub("", line))
    sentences = [s for s in _SPLIT.split(" ".join(lines)) if len(s.split()) > 2]
    if not sentences:
        return {"sentences": 0, "avg_words": 0.0, "long_pct": 0.0}
    words = [len(s.split()) for s in sentences]
    long = sum(1 for w in words if w > LONG_SENTENCE)
    return {
        "sentences": len(words),
        "avg_words": round(sum(words) / len(words), 1),
        "long_pct": round(100 * long / len(words), 1),
    }


def current() -> dict[str, dict[str, float]]:
    return {str(p.relative_to(ROOT)): measure(p.read_text(encoding="utf-8")) for p in doc_files()}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--update", action="store_true", help="rewrite the baseline")
    mode.add_argument("--report", action="store_true", help="print every file's figures")
    args = parser.parse_args()

    now = current()
    if args.update:
        BASELINE.write_text(json.dumps(now, indent=1, sort_keys=True) + "\n", encoding="utf-8")
        print(f"doc density: baseline written for {len(now)} files")
        return 0
    if args.report:
        for name, m in sorted(now.items(), key=lambda kv: -kv[1]["avg_words"]):
            print(f"{m['avg_words']:5.1f} avg  {m['long_pct']:5.1f}% >30  {m['sentences']:5d}  {name}")
        return 0

    base = json.loads(BASELINE.read_text(encoding="utf-8")) if BASELINE.exists() else {}
    failures = []
    for name, m in now.items():
        b = base.get(name)
        if b is None:
            if m["long_pct"] > 5.0 + LONG_SLACK:
                failures.append(f"{name}: new file with {m['long_pct']}% sentences over 30 words (target < 5%)")
            continue
        if m["avg_words"] > b["avg_words"] + AVG_SLACK:
            failures.append(f"{name}: average sentence {m['avg_words']} words, baseline {b['avg_words']}")
        if m["long_pct"] > b["long_pct"] + LONG_SLACK:
            failures.append(f"{name}: {m['long_pct']}% sentences over 30 words, baseline {b['long_pct']}%")
    if failures:
        print("doc density check failed (see the write-docs skill; `--update` after a deliberate rewrite):")
        for line in failures:
            print(f"  - {line}")
        return 1
    print(f"doc density: {len(now)} files at or below baseline")
    return 0


if __name__ == "__main__":
    sys.exit(main())
