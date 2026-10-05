"""Regression tests for release-time captured-constant maintenance."""

from __future__ import annotations

import hashlib
import io
import json
import zipfile

import pytest
from scripts import refresh_release_constants as refresh

_PHASE5_FIXTURE = (
    "BINARY_SIZE_BASELINES = {\n"
    '    "darwin": (10_000, "0.1.0"),  # host release build\n'
    '    "linux": (20_000, "0.0.9"),  # published manylinux2014 x86_64 wheel member\n'
    "}\n"
    'LINUX_SIZE_WHEEL = "kglite-0.0.9-cp310-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl"\n'
    'LINUX_SIZE_WHEEL_SHA256 = "' + "0" * 64 + '"\n\n'
    "    Baseline history:\n"
    "    Raising the baseline is a deliberate act\n"
)


def test_binary_size_refresh_updates_platform_entry_idempotently(tmp_path, monkeypatch):
    phase5 = tmp_path / "test_phase5_parity.py"
    phase5.write_text(_PHASE5_FIXTURE, encoding="utf-8")
    monkeypatch.setattr(refresh, "PHASE5_TEST", phase5)
    monkeypatch.setattr(refresh.sys, "platform", "darwin")

    changed, _ = refresh.refresh_binary_size("1.2.3", 12_345)
    assert changed
    text = phase5.read_text(encoding="utf-8")
    assert '"darwin": (12_345, "1.2.3"),  # host release build' in text
    assert '"linux": (20_000, "0.0.9")' in text
    assert text.count("- 1.2.3:") == 1

    changed, _ = refresh.refresh_binary_size("1.2.3", 12_345)
    assert not changed
    assert phase5.read_text(encoding="utf-8").count("- 1.2.3:") == 1


def test_host_binary_size_refresh_refuses_a_non_macos_host(tmp_path, monkeypatch):
    """The host step measures the macOS build; any other host's library is a
    different artifact and must never be written into another platform's row."""
    phase5 = tmp_path / "test_phase5_parity.py"
    phase5.write_text(_PHASE5_FIXTURE, encoding="utf-8")
    monkeypatch.setattr(refresh, "PHASE5_TEST", phase5)
    for host in ("linux", "win32"):
        monkeypatch.setattr(refresh.sys, "platform", host)
        with pytest.raises(refresh.RefreshError, match="macOS"):
            refresh.refresh_binary_size("1.2.3", 12_345)
    assert phase5.read_text(encoding="utf-8") == _PHASE5_FIXTURE


def _wheel_bytes(member_size: int) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("kglite/kglite.abi3.so", b"\0" * member_size)
        zf.writestr("kglite/__init__.py", b"")
    return buf.getvalue()


def _pypi_index(files_by_version: dict[str, list[dict]]) -> bytes:
    return json.dumps({"releases": files_by_version}).encode()


def _file(name: str, payload: bytes, *, yanked: bool = False, sha: str | None = None) -> dict:
    return {
        "filename": name,
        "url": f"https://files.example/{name}",
        "digests": {"sha256": sha or hashlib.sha256(payload).hexdigest()},
        "yanked": yanked,
    }


_LINUX = "kglite-{v}-cp310-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl"
_ARM = "kglite-{v}-cp310-abi3-manylinux_2_17_aarch64.manylinux2014_aarch64.whl"
_MAC = "kglite-{v}-cp310-abi3-macosx_11_0_arm64.whl"


def _serve(monkeypatch, index: bytes, blobs: dict[str, bytes]):
    def get(url: str) -> bytes:
        if url == refresh.PYPI_JSON_URL:
            return index
        return blobs[url.rsplit("/", 1)[1]]

    monkeypatch.setattr(refresh, "_http_get", get)


def test_linux_member_is_the_newest_published_release_below_the_cut(monkeypatch):
    old, prev, arm = _wheel_bytes(100), _wheel_bytes(300), _wheel_bytes(999)
    index = _pypi_index(
        {
            "0.9.0": [_file(_LINUX.format(v="0.9.0"), old)],
            "0.10.0": [_file(_LINUX.format(v="0.10.0"), prev), _file(_ARM.format(v="0.10.0"), arm)],
            # Yanked, and the version being cut: neither may be chosen.
            "0.10.1": [_file(_LINUX.format(v="0.10.1"), old, yanked=True)],
            "0.11.0": [_file(_LINUX.format(v="0.11.0"), old)],
            # A macOS-only release is skipped, not an error.
            "0.10.2": [_file(_MAC.format(v="0.10.2"), old)],
        }
    )
    _serve(
        monkeypatch,
        index,
        {
            _LINUX.format(v="0.9.0"): old,
            _LINUX.format(v="0.10.0"): prev,
            _ARM.format(v="0.10.0"): arm,
            _LINUX.format(v="0.10.1"): old,
            _LINUX.format(v="0.11.0"): old,
        },
    )
    member = refresh.previous_published_linux_member("0.11.0")
    assert member.version == "0.10.0"
    assert member.wheel == _LINUX.format(v="0.10.0")
    assert member.size == 300
    assert member.sha256 == hashlib.sha256(prev).hexdigest()


def test_linux_member_refuses_a_digest_mismatch(monkeypatch):
    payload = _wheel_bytes(300)
    index = _pypi_index({"0.10.0": [_file(_LINUX.format(v="0.10.0"), payload, sha="f" * 64)]})
    _serve(monkeypatch, index, {_LINUX.format(v="0.10.0"): payload})
    with pytest.raises(refresh.RefreshError, match="SHA-256"):
        refresh.previous_published_linux_member("0.11.0")


def test_linux_refresh_rewrites_entry_and_identity_idempotently(tmp_path, monkeypatch):
    phase5 = tmp_path / "test_phase5_parity.py"
    phase5.write_text(_PHASE5_FIXTURE, encoding="utf-8")
    monkeypatch.setattr(refresh, "PHASE5_TEST", phase5)
    member = refresh.LinuxMember("0.1.0", _LINUX.format(v="0.1.0"), "a" * 64, 21_000)

    changed, _ = refresh.refresh_linux_binary_size(member)
    assert changed
    text = phase5.read_text(encoding="utf-8")
    assert '"linux": (21_000, "0.1.0"),  # published manylinux2014 x86_64 wheel member' in text
    assert f'LINUX_SIZE_WHEEL = "{member.wheel}"' in text
    assert f'LINUX_SIZE_WHEEL_SHA256 = "{"a" * 64}"' in text
    assert '"darwin": (10_000, "0.1.0")' in text

    changed, _ = refresh.refresh_linux_binary_size(member)
    assert not changed


def test_the_real_phase5_table_parses_for_both_rewriters():
    """The rewriters' anchors must match the committed file, not only the fixture."""
    text = refresh.PHASE5_TEST.read_text(encoding="utf-8")
    for platform in ("darwin", "linux"):
        assert refresh._baseline_entry(text, platform) is not None, platform
    assert refresh.LINUX_WHEEL_RE.search(text) is not None
    assert refresh.LINUX_SHA_RE.search(text) is not None


def test_perf_capture_is_pending_until_explicit_qualification(tmp_path, monkeypatch):
    import json
    from pathlib import Path
    import subprocess

    from scripts.benchmark_qualification import qualify

    directory = tmp_path / "baselines"
    directory.mkdir()
    monkeypatch.setattr(refresh, "REPO_ROOT", tmp_path)
    monkeypatch.setattr(refresh, "BASELINES_DIR", directory)
    monkeypatch.setattr(refresh.sys, "platform", "darwin")
    source = {
        "machine_info": {"system": "Darwin", "machine": "arm64"},
        "benchmarks": [{"name": "cell", "stats": {"min": 1.0, "data": [1.0]}}],
    }
    calls = []

    def benchmark(command, **kwargs):
        calls.append(command)
        output = Path(next(arg.split("=", 1)[1] for arg in command if arg.startswith("--benchmark-json=")))
        output.write_text(json.dumps(source), encoding="utf-8")
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(refresh.subprocess, "run", benchmark)
    changed, message = refresh.refresh_perf_baseline("0.17.0")
    assert changed and "pending" in message
    target = directory / "0_17_0.json"
    raw = target.read_bytes()
    assert (directory / "current.json").read_bytes() == raw
    assert "data" not in json.loads(raw)["benchmarks"][0]["stats"]
    manifest = json.loads((directory / "qualifications.json").read_text(encoding="utf-8"))
    assert manifest["captures"][target.name]["status"] == "pending"
    assert manifest["references"] == {}
    qualify(target, "accepted", "Two agreeing synthetic controls", promote=True)
    before = (directory / "qualifications.json").read_bytes()
    changed, _ = refresh.refresh_perf_baseline("0.17.0")
    assert not changed and len(calls) == 1
    assert target.read_bytes() == raw
    assert (directory / "qualifications.json").read_bytes() == before
