#!/usr/bin/env python3
"""Assert the skills folders have the agreed layout.

`.agents/skills/` is the one real skills folder (Codex reads it directly);
`.claude/skills` is a symlink to `../.agents/skills` so Claude Code reads the
same files. Both directories are gitignored and workstation-local, so a fresh
clone (and CI) has neither and the check has nothing to compare.

A regular `.claude/skills` directory next to `.agents/skills` is the failure
this exists for: two hand-maintained copies drift, and a stale copy teaches a
procedure the live one warns against (KGLite's `release` skill once taught
the exact version-bump belief that broke the 0.15.0 release).
"""

from __future__ import annotations

from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parent.parent


def check(root: Path) -> tuple[list[str], str]:
    """Return `(problems, summary)` for the skills layout under `root`."""
    agents = root / ".agents" / "skills"
    claude = root / ".claude" / "skills"
    if not agents.exists() and not claude.is_symlink() and not claude.exists():
        return [], "no .agents/skills or .claude/skills — nothing to check"
    if not agents.is_dir():
        return [
            ".claude/skills exists but .agents/skills does not — .agents/skills is the real "
            "skills folder; move the files there and symlink .claude/skills to it "
            "(`ln -s ../.agents/skills .claude/skills`)"
        ], ""
    if not claude.exists() and not claude.is_symlink():
        return [
            ".agents/skills exists but .claude/skills does not, so Claude Code sees no "
            "skills — `ln -s ../.agents/skills .claude/skills`"
        ], ""
    fix = "move any edits into .agents/skills, then `rm -r .claude/skills && ln -s ../.agents/skills .claude/skills`"
    if not claude.is_symlink():
        return [f".claude/skills is a real directory, not a symlink to .agents/skills — {fix}"], ""
    if claude.resolve() != agents.resolve():
        return [f".claude/skills is a symlink to {claude.resolve()}, not to .agents/skills — {fix}"], ""
    return [], ".claude/skills is a symlink to .agents/skills"


def main() -> int:
    problems, summary = check(ROOT)
    if problems:
        print("skill layout: WRONG", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1
    print("skill layout: " + summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
