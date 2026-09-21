"""Keep external workflow code immutable while Dependabot updates the pins."""

from pathlib import Path
import re

import yaml


def test_all_external_workflow_actions_use_full_commit_pins():
    workflows = Path(__file__).resolve().parents[1] / ".github" / "workflows"
    references = []
    for path in sorted(workflows.glob("*.y*ml")):
        jobs = yaml.safe_load(path.read_text(encoding="utf-8"))["jobs"]
        for job in jobs.values():
            for step in [job, *job.get("steps", [])]:
                uses = step.get("uses", "")
                if not uses or uses.startswith("./"):
                    continue
                references.append((path.name, uses))
                assert re.fullmatch(r"[^@\s]+@[0-9a-f]{40}", uses), f"{path.name}: mutable action {uses}"
                if uses.startswith("dtolnay/rust-toolchain@"):
                    # Pinning the action's master commit drops the defaults on
                    # its stable/nightly branches; the input must be explicit.
                    assert step.get("with", {}).get("toolchain"), f"{path.name}: missing Rust toolchain input"
    assert references, "no external workflow actions found"
