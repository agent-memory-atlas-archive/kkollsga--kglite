"""The skills-layout gate can go red: each wrong layout is reported."""

import importlib.util
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location(
    "check_skill_layout", Path(__file__).resolve().parent.parent / "scripts" / "check_skill_layout.py"
)
layout = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(layout)


def test_neither_directory_is_nothing_to_check(tmp_path):
    problems, _ = layout.check(tmp_path)
    assert problems == []


def test_symlinked_claude_skills_passes(tmp_path):
    (tmp_path / ".agents/skills").mkdir(parents=True)
    (tmp_path / ".claude").mkdir()
    (tmp_path / ".claude/skills").symlink_to("../.agents/skills")
    assert layout.check(tmp_path)[0] == []


def test_agents_skills_without_the_claude_link_fails(tmp_path):
    (tmp_path / ".agents/skills").mkdir(parents=True)
    problems, _ = layout.check(tmp_path)
    assert problems and "Claude Code sees no skills" in problems[0]


def test_a_real_claude_skills_directory_fails(tmp_path):
    (tmp_path / ".agents/skills").mkdir(parents=True)
    (tmp_path / ".claude/skills").mkdir(parents=True)
    problems, _ = layout.check(tmp_path)
    assert problems and "not a symlink" in problems[0]


def test_a_symlink_to_elsewhere_fails(tmp_path):
    (tmp_path / ".agents/skills").mkdir(parents=True)
    (tmp_path / "other").mkdir()
    (tmp_path / ".claude").mkdir()
    (tmp_path / ".claude/skills").symlink_to("../other")
    problems, _ = layout.check(tmp_path)
    assert problems and "not to .agents/skills" in problems[0]


def test_claude_skills_without_agents_skills_fails(tmp_path):
    (tmp_path / ".claude/skills").mkdir(parents=True)
    problems, _ = layout.check(tmp_path)
    assert problems and ".agents/skills does not" in problems[0]
