"""Where a disk graph's published generation lives, for tests that read its files."""

from __future__ import annotations

import os
from pathlib import Path


def current_generation(root: str | os.PathLike) -> Path:
    """The generation directory the graph at ``root``'s ``CURRENT`` pointer selects."""
    root = Path(root)
    return root / "generations" / (root / "CURRENT").read_text(encoding="utf-8").strip()
