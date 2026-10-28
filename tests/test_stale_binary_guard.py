"""The guard that keeps a prebuilt server binary from being tested against a
newer on-disk format.

`conftest.binary_skip_reason` classifies a binary as stale by mtime. It used to
compare against the root `Cargo.toml` alone, which is a proxy for "the version
moved" — and a WAL revision bump moves no version. The 0.17 revision 6→7 landed
with the manifest untouched, so `kglite-mcp-server` and `kglite-bolt-server`
binaries built against revision 6 kept being tested against logs the freshly
built extension wrote at revision 7, and the guard had nothing to say.

These tests are the non-vacuity proof the guard was missing: each one moves one
input and requires the verdict to change with it.
"""

import os
from pathlib import Path

import pytest

from tests.conftest import (
    _FORMAT_SOURCES,
    _REPO_ROOT,
    binary_skip_reason,
    binary_version,
    newest_binary_source,
    newest_format_source,
    workspace_version,
)


def _binary_at(tmp_path: Path, mtime: float) -> Path:
    binary = tmp_path / "kglite-fake-server"
    binary.write_bytes(b"")
    os.utime(binary, (mtime, mtime))
    return binary


def test_every_named_format_source_exists():
    """A renamed module must fail loudly here, not quietly narrow the guard to
    the manifest — which is the failure mode that let the revision bump
    through."""
    missing = [path for path in _FORMAT_SOURCES if not path.exists()]
    assert missing == [], f"update _FORMAT_SOURCES in tests/conftest.py: {missing}"


def test_the_wal_revision_source_is_watched():
    """The specific file whose bump the old guard could not see."""
    wal = _REPO_ROOT / "crates" / "kglite" / "src" / "graph" / "wal.rs"
    assert wal in _FORMAT_SOURCES
    assert "WAL_FORMAT_VERSION" in wal.read_text(encoding="utf-8"), (
        "the watched file must be the one that carries the WAL revision"
    )


def test_a_binary_newer_than_every_format_source_runs(tmp_path):
    fresh = newest_format_source().stat().st_mtime + 60
    binary = _binary_at(tmp_path, fresh)
    assert binary_skip_reason("srv", binary, "make dev") is None


def test_a_binary_older_than_a_format_source_is_skipped(tmp_path):
    """The guard can go red — and names the file that outran the binary."""
    stale = newest_format_source().stat().st_mtime - 60
    binary = _binary_at(tmp_path, stale)
    reason = binary_skip_reason("srv", binary, "make dev")
    assert reason is not None
    assert "stale build" in reason
    assert "rebuild with: make dev" in reason


def test_a_touched_format_source_alone_turns_a_fresh_binary_stale(tmp_path, monkeypatch):
    """The bump case, reproduced: nothing but a format source moves, and the
    binary that was current a moment ago is refused."""
    manifest = tmp_path / "Cargo.toml"
    wal = tmp_path / "wal.rs"
    for path in (manifest, wal):
        path.write_text("x", encoding="utf-8")
        os.utime(path, (1_000, 1_000))
    monkeypatch.setattr("tests.conftest._FORMAT_SOURCES", (manifest, wal))
    monkeypatch.setattr("tests.conftest._REPO_ROOT", tmp_path)

    binary = _binary_at(tmp_path, 2_000)
    assert binary_skip_reason("srv", binary, "make dev") is None

    os.utime(wal, (3_000, 3_000))
    reason = binary_skip_reason("srv", binary, "make dev")
    assert reason is not None and "wal.rs" in reason


def test_a_touched_mcp_source_turns_a_fresh_mcp_binary_stale(tmp_path, monkeypatch):
    """Tool discovery changed without a manifest edit in the budget rollout."""
    root_manifest = tmp_path / "Cargo.toml"
    lockfile = tmp_path / "Cargo.lock"
    mcp_dir = tmp_path / "crates" / "kglite-mcp-server"
    core_dir = tmp_path / "crates" / "kglite"
    (mcp_dir / "src").mkdir(parents=True)
    (core_dir / "src").mkdir(parents=True)
    crate_manifest = mcp_dir / "Cargo.toml"
    boot = mcp_dir / "src" / "boot.rs"
    parameter_presence = core_dir / "src" / "parameter_presence.rs"
    for path in (root_manifest, lockfile, crate_manifest, boot, parameter_presence):
        path.write_text("x", encoding="utf-8")
        os.utime(path, (1_000, 1_000))
    format_source = tmp_path / "format.rs"
    format_source.write_text("x", encoding="utf-8")
    os.utime(format_source, (1_000, 1_000))
    monkeypatch.setattr("tests.conftest._FORMAT_SOURCES", (format_source,))
    monkeypatch.setattr("tests.conftest._BINARY_CRATES", {"kglite-mcp-server": "kglite-mcp-server"})
    monkeypatch.setattr("tests.conftest._REPO_ROOT", tmp_path)

    binary = _binary_at(tmp_path, 2_000)
    assert binary_skip_reason("kglite-mcp-server", binary, "make test-mcp") is None

    os.utime(boot, (3_000, 3_000))
    assert newest_binary_source("kglite-mcp-server") == boot
    reason = binary_skip_reason("kglite-mcp-server", binary, "make test-mcp")
    assert reason is not None and "boot.rs" in reason

    os.utime(boot, (1_000, 1_000))
    os.utime(parameter_presence, (4_000, 4_000))
    reason = binary_skip_reason("kglite-mcp-server", binary, "make test-mcp")
    assert reason is not None and "parameter_presence.rs" in reason

    # A manifest touch is not an input: `cargo build` would not rebuild for it,
    # so a skip on it could never be cleared.
    os.utime(parameter_presence, (1_000, 1_000))
    for untracked in (crate_manifest, root_manifest):
        os.utime(untracked, (5_000, 5_000))
    assert binary_skip_reason("kglite-mcp-server", binary, "make test-mcp") is None

    # The lockfile is: the crate's build.rs makes cargo rebuild for it.
    os.utime(lockfile, (6_000, 6_000))
    reason = binary_skip_reason("kglite-mcp-server", binary, "make test-mcp")
    assert reason is not None and "Cargo.lock" in reason


def test_every_binary_crate_rebuilds_when_the_lockfile_changes():
    """The staleness check treats `Cargo.lock` as an input only because these
    crates' build scripts declare it; without the declaration a lock-only
    change would skip the suite and the advised rebuild would do nothing."""
    from tests.conftest import _BINARY_CRATES

    for crate in sorted(set(_BINARY_CRATES.values())):
        build_rs = _REPO_ROOT / "crates" / crate / "build.rs"
        assert build_rs.is_file(), f"{crate} needs a build.rs declaring ../../Cargo.lock"
        text = build_rs.read_text(encoding="utf-8")
        assert "cargo:rerun-if-changed" in text and "Cargo.lock" in text, crate


def test_an_unbuilt_binary_is_reported_as_unbuilt(tmp_path):
    reason = binary_skip_reason("srv", tmp_path / "nope", "make dev")
    assert reason is not None and "not built" in reason


def test_a_format_source_list_that_names_nothing_real_refuses(tmp_path, monkeypatch):
    """Silence is the one answer this guard must never give."""
    monkeypatch.setattr("tests.conftest._FORMAT_SOURCES", (tmp_path / "gone.rs",))
    with pytest.raises(RuntimeError, match="no format-defining source"):
        newest_format_source()


def _versioned_binary(tmp_path: Path, version: str, mtime: float) -> Path:
    binary = tmp_path / "kglite-fake-versioned"
    binary.write_text(f"#!/bin/sh\necho 'kglite-fake {version}'\n", encoding="utf-8")
    binary.chmod(0o755)
    os.utime(binary, (mtime, mtime))
    return binary


def test_a_comment_only_manifest_edit_does_not_make_a_binary_stale(tmp_path, monkeypatch):
    """`cargo build` ignores such an edit and reports "Finished", so a skip on
    it would repeat forever after the advised rebuild."""
    manifest = tmp_path / "Cargo.toml"
    manifest.write_text('[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8")
    os.utime(manifest, (1_000, 1_000))
    monkeypatch.setattr("tests.conftest._REPO_ROOT", tmp_path)
    format_source = tmp_path / "format.rs"
    format_source.write_text("x", encoding="utf-8")
    os.utime(format_source, (1_000, 1_000))
    monkeypatch.setattr("tests.conftest._FORMAT_SOURCES", (format_source,))
    binary = _versioned_binary(tmp_path, "1.2.3", 2_000)
    assert binary_skip_reason("kglite-fake", binary, "make dev") is None

    manifest.write_text('# a new comment\n[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8")
    os.utime(manifest, (3_000, 3_000))
    assert binary_skip_reason("kglite-fake", binary, "make dev") is None


def test_a_binary_reporting_another_version_is_skipped(tmp_path, monkeypatch):
    manifest = tmp_path / "Cargo.toml"
    manifest.write_text('[workspace.package]\nversion = "1.2.4"\n', encoding="utf-8")
    os.utime(manifest, (1_000, 1_000))
    monkeypatch.setattr("tests.conftest._REPO_ROOT", tmp_path)
    format_source = tmp_path / "format.rs"
    format_source.write_text("x", encoding="utf-8")
    os.utime(format_source, (1_000, 1_000))
    monkeypatch.setattr("tests.conftest._FORMAT_SOURCES", (format_source,))
    binary = _versioned_binary(tmp_path, "1.2.3", 2_000)
    assert binary_version(binary) == "1.2.3"
    assert workspace_version(tmp_path) == "1.2.4"
    reason = binary_skip_reason("kglite-fake", binary, "make dev")
    assert reason is not None
    assert "reports version 1.2.3" in reason and "rebuild with: make dev" in reason
