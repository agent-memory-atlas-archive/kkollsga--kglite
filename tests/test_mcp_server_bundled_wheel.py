"""The bundled MCP server reachable from `pip install kglite`.

As of 0.10.26 the Rust `kglite-mcp-server` lives in the wheel: its *library*
(`crates/kglite-mcp-server/src/lib.rs::run`) is statically linked into the
extension and exposed to Python as `kglite._run_mcp_server`, with the
`kglite-mcp-server` console script (a thin `kglite/mcp_server.py` shim)
forwarding argv into it. So `pip install kglite && kglite-mcp-server ...` runs
the identical server as `cargo install kglite-mcp-server`.

Unlike `test_mcp_server_smoke.py` (which drives the compiled cargo *binary*),
these tests drive the *wheel-hosted* server via `python -m kglite.mcp_server`
— so they run wherever the wheel is importable, no cargo build required. They
reuse the JSON-RPC stdio client from the smoke module.
"""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
from typing import Optional

import pandas as pd
import pytest

import kglite
from tests.test_mcp_server_smoke import McpClient, _text_content


def _build_fixture_graph(path: Path) -> None:
    g = kglite.KnowledgeGraph()
    nodes = pd.DataFrame({"id": [1, 2, 3, 4], "title": ["Alice", "Bob", "Carol", "Dave"]})
    g.add_nodes(nodes, "Person", "id", "title")
    edges = pd.DataFrame({"src": [1, 2, 3], "dst": [2, 3, 4]})
    g.add_connections(edges, "KNOWS", "Person", "src", "Person", "dst")
    g.save(str(path))


def _spawn_wheel(args: list[str], cwd: Optional[Path] = None) -> McpClient:
    """Launch the bundled server through the shim module (the same code path
    the `kglite-mcp-server` console script runs) and complete the handshake."""
    proc = subprocess.Popen(
        [sys.executable, "-m", "kglite.mcp_server", *args],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=str(cwd) if cwd else None,
    )
    client = McpClient(proc)
    client.initialize()
    return client


def test_entry_point_is_bundled() -> None:
    """The wheel exposes the in-process Rust entry point and the thin shim."""
    assert hasattr(kglite, "_run_mcp_server"), "wheel is missing the bundled MCP server entry point"
    from kglite import mcp_server

    assert callable(mcp_server.main)


def test_bundled_server_boots_and_lists_tools(tmp_path: Path) -> None:
    manifest = tmp_path / "bare_mcp.yaml"
    manifest.write_text("name: Bundled Wheel Smoke\n", encoding="utf-8")
    client = _spawn_wheel(["--mcp-config", str(manifest)])
    try:
        names = {t["name"] for t in client.list_tools()}
        assert "ping" in names
        assert "cypher_query" in names
        assert "graph_overview" in names
    finally:
        client.shutdown()


def test_bundled_server_runs_cypher_on_graph(tmp_path: Path) -> None:
    kgl = tmp_path / "fixture.kgl"
    _build_fixture_graph(kgl)
    client = _spawn_wheel(["--graph", str(kgl)])
    try:
        out = _text_content(client.call_tool("cypher_query", {"query": "MATCH (p:Person) RETURN count(p) AS n"}))
    finally:
        client.shutdown()
    assert "4" in out


def test_python_library_embedder_powers_text_score(tmp_path: Path) -> None:
    """A Python embedder library (`extensions.embedder.library: …`) lets the
    bundled server run `text_score()` via a Python embedder handed in by
    `_run_mcp_server`'s factory (which receives the config JSON). Uses a
    deterministic stub (no network, no real model): identical text → identical
    vector, so the exact-match node ranks top."""
    # A stub embedder shared by build-time (g.embed_texts) and serve-time (the
    # factory), so the stored node vectors and the query vector use one scheme.
    stub_mod = tmp_path / "stub_embed.py"
    stub_mod.write_text(
        "import hashlib\n"
        "class StubEmbedder:\n"
        "    dimension = 8\n"
        "    def embed(self, texts):\n"
        "        return [[float(b) for b in hashlib.sha256(t.encode()).digest()[:8]] for t in texts]\n",
        encoding="utf-8",
    )
    sys.path.insert(0, str(tmp_path))
    try:
        import stub_embed  # type: ignore

        g = kglite.KnowledgeGraph()
        df = pd.DataFrame(
            {"id": [1, 2, 3], "title": ["A", "B", "C"], "summary": ["alpha alpha", "beta beta", "gamma gamma"]}
        )
        g.add_nodes(df, "Doc", "id", "title")
        g.set_embedder(stub_embed.StubEmbedder())
        g.embed_texts("Doc", "summary", show_progress=False)
        kgl = tmp_path / "docs.kgl"
        g.save(str(kgl))
    finally:
        sys.path.remove(str(tmp_path))

    manifest = tmp_path / "docs_mcp.yaml"
    # `library: stub` (anything but fastembed-rs) routes to the Python factory.
    manifest.write_text(
        "name: stub\ntrust:\n  allow_embedder: true\nextensions:\n  embedder:\n    library: stub\n    model: stub\n",
        encoding="utf-8",
    )

    # Launch the bundled server with the same stub via the factory arg — the
    # real console-script path dispatches on library; here we inject the stub
    # (ignoring the config JSON) so the test is deterministic and offline.
    launcher = tmp_path / "launch.py"
    launcher.write_text(
        "import sys\n"
        f"sys.path.insert(0, {str(tmp_path)!r})\n"
        "import kglite, stub_embed\n"
        "kglite._run_mcp_server(sys.argv[1:], embedder_factory=lambda cfg: stub_embed.StubEmbedder())\n",
        encoding="utf-8",
    )
    proc = subprocess.Popen(
        [sys.executable, str(launcher), "--graph", str(kgl), "--mcp-config", str(manifest)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    client = McpClient(proc)
    client.initialize()
    try:
        out = _text_content(
            client.call_tool(
                "cypher_query",
                {
                    "query": "MATCH (d:Doc) RETURN d.title AS t, "
                    "text_score(d, 'summary', 'beta beta') AS s ORDER BY s DESC"
                },
            )
        )
    finally:
        client.shutdown()
    # The query ran (no "requires the pip-hosted server" / embedder error) and
    # the exact-match node B (summary 'beta beta') is present and ranked first.
    assert "error" not in out.lower()[:60], out
    assert "3 row(s)" in out, out
    first_data_line = next(ln for ln in out.splitlines() if "B" in ln or "A" in ln or "C" in ln)
    assert "B" in first_data_line, f"exact-match node B should rank first:\n{out}"


def _lazy_embedder_fixture(tmp_path: Path, load_line: str) -> tuple[Path, Path, Path, dict]:
    """A graph with stored vectors plus a `factory: lazy_stub:build` manifest
    whose builder appends a line to a marker file each time it runs."""
    import os

    (tmp_path / "lazy_stub.py").write_text(
        "import hashlib, os\n"
        "class Stub:\n"
        "    dimension = 8\n"
        "    def __del__(self):\n"
        "        path = os.environ.get('LAZY_MARKER')\n"
        "        if path:\n"
        "            with open(path, 'a') as f:\n"
        "                f.write('freed\\n')\n"
        "    def embed(self, texts):\n"
        "        return [[float(b) for b in hashlib.sha256(t.encode()).digest()[:8]] for t in texts]\n"
        "def build(model):\n"
        "    with open(os.environ['LAZY_MARKER'], 'a') as f:\n"
        "        f.write('built\\n')\n"
        "    return Stub()\n",
        encoding="utf-8",
    )
    sys.path.insert(0, str(tmp_path))
    try:
        import lazy_stub  # type: ignore

        g = kglite.KnowledgeGraph()
        df = pd.DataFrame({"id": [1, 2], "title": ["A", "B"], "summary": ["alpha alpha", "beta beta"]})
        g.add_nodes(df, "Doc", "id", "title")
        g.set_embedder(lazy_stub.Stub())
        g.embed_texts("Doc", "summary", show_progress=False)
        kgl = tmp_path / "docs.kgl"
        g.save(str(kgl))
    finally:
        sys.path.remove(str(tmp_path))
        sys.modules.pop("lazy_stub", None)
    manifest = tmp_path / "mcp.yaml"
    manifest.write_text(
        "name: lazy\ntrust:\n  allow_embedder: true\nextensions:\n  embedder:\n"
        f"    factory: lazy_stub:build\n    model: stub\n{load_line}",
        encoding="utf-8",
    )
    marker_file = tmp_path / "marker.txt"
    env = {**os.environ, "PYTHONPATH": str(tmp_path), "LAZY_MARKER": str(marker_file)}
    return kgl, manifest, marker_file, env


def _spawn_wheel_env(args: list[str], env: dict) -> McpClient:
    proc = subprocess.Popen(
        [sys.executable, "-m", "kglite.mcp_server", *args],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )
    client = McpClient(proc)
    client.initialize()
    return client


def _events(marker_file: Path) -> list[str]:
    return marker_file.read_text(encoding="utf-8").splitlines() if marker_file.exists() else []


def _rows_only(out: str) -> str:
    """The result table without the footer, which carries a clock."""
    return out.split("— active graph")[0]


def _built_count(marker_file: Path) -> int:
    return _events(marker_file).count("built")


def test_manifest_embedder_is_built_on_first_semantic_call(tmp_path: Path) -> None:
    """The default `load: lazy` constructs the embedder through the real wheel
    factory (`kglite._mcp_embed.build_embedder`) only on the first
    `text_score()`, and only once."""
    kgl, manifest, marker_file, env = _lazy_embedder_fixture(tmp_path, "")
    client = _spawn_wheel_env(["--graph", str(kgl), "--mcp-config", str(manifest)], env)
    query = {"query": "MATCH (d:Doc) RETURN d.title AS t, text_score(d, 'summary', 'beta beta') AS s ORDER BY s DESC"}
    try:
        client.list_tools()
        client.call_tool("cypher_query", {"query": "MATCH (d:Doc) RETURN count(d) AS n"})
        assert _built_count(marker_file) == 0, "booting and plain queries must not build the embedder"
        out = _text_content(client.call_tool("cypher_query", query))
        assert "error" not in out.lower()[:60], out
        assert _built_count(marker_file) == 1
        _text_content(client.call_tool("cypher_query", query))
        assert _built_count(marker_file) == 1, "the embedder is built once"
    finally:
        client.shutdown()


def test_manifest_embedder_load_eager_builds_at_boot(tmp_path: Path) -> None:
    kgl, manifest, marker_file, env = _lazy_embedder_fixture(tmp_path, "    load: eager\n")
    client = _spawn_wheel_env(["--graph", str(kgl), "--mcp-config", str(manifest)], env)
    try:
        client.list_tools()
        assert _built_count(marker_file) == 1
    finally:
        client.shutdown()


def test_manifest_embedder_is_freed_after_its_cooldown_and_rebuilt(tmp_path: Path) -> None:
    """After `cooldown` idle seconds the server drops the model (the Python
    object is destroyed, not merely unreferenced from Rust) and the next
    `text_score()` rebuilds it and answers normally."""
    import time

    kgl, manifest, marker_file, env = _lazy_embedder_fixture(tmp_path, "    cooldown: 1\n")
    client = _spawn_wheel_env(["--graph", str(kgl), "--mcp-config", str(manifest)], env)
    query = {"query": "MATCH (d:Doc) RETURN d.title AS t, text_score(d, 'summary', 'beta beta') AS s ORDER BY s DESC"}
    try:
        first = _text_content(client.call_tool("cypher_query", query))
        assert "error" not in first.lower()[:60], first
        assert _events(marker_file) == ["built"]
        deadline = time.monotonic() + 15
        while "freed" not in _events(marker_file) and time.monotonic() < deadline:
            time.sleep(0.1)
        assert _events(marker_file) == ["built", "freed"], "the idle model must be destroyed"
        again = _text_content(client.call_tool("cypher_query", query))
        assert _rows_only(again) == _rows_only(first), "the rebuilt model answers the same"
        assert _events(marker_file)[:3] == ["built", "freed", "built"]
    finally:
        client.shutdown()


def test_manifest_embedder_cooldown_zero_keeps_the_model(tmp_path: Path) -> None:
    import time

    kgl, manifest, marker_file, env = _lazy_embedder_fixture(tmp_path, "    cooldown: 0\n")
    client = _spawn_wheel_env(["--graph", str(kgl), "--mcp-config", str(manifest)], env)
    query = {"query": "MATCH (d:Doc) RETURN text_score(d, 'summary', 'beta beta') AS s"}
    try:
        _text_content(client.call_tool("cypher_query", query))
        time.sleep(2.5)
        assert _events(marker_file) == ["built"]
    finally:
        client.shutdown()


def test_shim_exit_code_on_bad_args(tmp_path: Path) -> None:
    """clap parses argv Rust-side; a bad flag should make the shim exit
    non-zero (the server never reaches the serve loop)."""
    proc = subprocess.run(
        [sys.executable, "-m", "kglite.mcp_server", "--graph", str(tmp_path / "does_not_exist.kgl")],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
    )
    assert proc.returncode != 0


def test_selftest_on_wheel_install(tmp_path: Path) -> None:
    """`--selftest` must work on the *wheel* install, not just the cargo
    binary. Regression: the self-test re-spawns the server, and on the wheel
    `current_exe()` is the Python interpreter (the console script is a shim),
    so a naive re-spawn launched `python <server-flags>` and failed with
    "Unknown option". `kglite.mcp_server.main` now exports KGLITE_MCP_RESPAWN
    so the child is launched via the module entry. Drives the exact wheel
    code path (`python -m kglite.mcp_server`)."""
    kgl = tmp_path / "fixture.kgl"
    _build_fixture_graph(kgl)
    proc = subprocess.run(
        [sys.executable, "-m", "kglite.mcp_server", "--selftest", "--graph", str(kgl)],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=60,
    )
    out = proc.stdout.decode(errors="replace") + proc.stderr.decode(errors="replace")
    assert proc.returncode == 0, out
    assert "Selftest PASSED" in out
    assert "graph hydrates" in out


def test_selftest_on_wheel_install_bad_graph_fails(tmp_path: Path) -> None:
    """The wheel self-test still reports a genuine misconfiguration as a
    non-zero failure (not a false green)."""
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "kglite.mcp_server",
            "--selftest",
            "--graph",
            str(tmp_path / "missing.kgl"),
        ],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=60,
    )
    out = proc.stdout.decode(errors="replace") + proc.stderr.decode(errors="replace")
    assert proc.returncode != 0
    assert "Selftest FAILED" in out


def _write_wide_local_workspace(tmp_path: Path, n_files: int = 400) -> tuple[Path, Path, Path]:
    """A broad `workspace.kind: local` root (many files across dirs) plus a
    tiny representative subdir. Mirrors the deployed code-review archetype's
    shape (a wide sandbox agents narrow with set_root_dir). Returns
    (manifest, root, small_subdir)."""
    root = tmp_path / "wide_root"
    for d in range(n_files // 20):
        pkg = root / f"pkg{d}"
        pkg.mkdir(parents=True, exist_ok=True)
        for f in range(20):
            (pkg / f"m{f}.py").write_text(
                f"class C{f}:\n    def meth(self, x): return x + {f}\ndef fn{f}(a): return a\n", encoding="utf-8"
            )
    small = root / "sub_small"
    small.mkdir(parents=True, exist_ok=True)
    (small / "a.py").write_text("def g(x):\n    return x\nclass W:\n    def r(self): return 1\n", encoding="utf-8")
    manifest = tmp_path / "wide_mcp.yaml"
    manifest.write_text(
        f"name: Wide Root Selftest\nworkspace: {{ kind: local, root: {root}, watch: true }}\n", encoding="utf-8"
    )
    return manifest, root, small


def test_selftest_wide_local_workspace_does_not_build_root(tmp_path: Path) -> None:
    """Regression (wide-root hang): `--selftest` against a local-workspace
    server with a *wide* root must NOT build a code graph over the whole root
    (that path no client uses; for a broad root it's unbounded → a silent
    hang). It stays registration-only and completes fast. Runs on the wheel
    install, the deployed shape."""
    manifest, _root, _small = _write_wide_local_workspace(tmp_path)
    # A 60s cap that the old build-the-whole-root behaviour would blow on a
    # genuinely wide tree; registration-only returns in a couple of seconds.
    proc = subprocess.run(
        [sys.executable, "-m", "kglite.mcp_server", "--selftest", "--mcp-config", str(manifest)],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=60,
    )
    out = proc.stdout.decode(errors="replace") + proc.stderr.decode(errors="replace")
    assert proc.returncode == 0, out
    assert "Selftest PASSED" in out
    # The contract: the wide root was NOT built; the operator is pointed at
    # --selftest-path. If someone reintroduces set_root_dir(root), this flips.
    assert "not built" in out
    assert "--selftest-path" in out


def test_selftest_path_without_producer_fails_hydration(tmp_path: Path) -> None:
    """The KGLite-only wheel has no workspace graph producer, so an explicit
    activation may resolve the path but must fail the real hydration gate."""
    manifest, _root, small = _write_wide_local_workspace(tmp_path)
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "kglite.mcp_server",
            "--selftest",
            "--selftest-path",
            str(small),
            "--mcp-config",
            str(manifest),
        ],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=60,
    )
    out = proc.stdout.decode(errors="replace") + proc.stderr.decode(errors="replace")
    assert proc.returncode != 0, out
    assert "✓ workspace activation" in out
    assert "✗ graph hydrates: No active graph" in out
    assert "Selftest FAILED" in out


@pytest.mark.skipif(importlib.util.find_spec("torch") is None, reason="needs torch (not installed here or in CI)")
def test_python_embedder_lazy_torch_import_survives_repeat_calls(tmp_path: Path) -> None:
    """A Python embedder that first imports torch inside `embed()` and retains a
    tensor must keep answering when the server runs on one Tokio worker.

    torch wheels whose bundled pybind11 predates 3.0.2 (torch < 2.13) cache the
    PyThreadState of the temporary attachment that ran the lazy import, then
    reuse that stale pointer on a later callback; the second query segfaulted
    the child (-11). Success is every response arriving and a clean exit.
    """
    (tmp_path / "torch_embed.py").write_text(
        "class TorchEmbedder:\n"
        "    dimension = 8\n"
        "    tensor = None\n"
        "    def embed(self, texts):\n"
        "        import torch\n"
        "        if self.tensor is None:\n"
        "            self.tensor = torch.ones(8)\n"
        "        return [self.tensor.to('cpu').tolist() for _ in texts]\n"
        "def build(model):\n"
        "    return TorchEmbedder()\n",
        encoding="utf-8",
    )
    g = kglite.KnowledgeGraph()
    df = pd.DataFrame({"id": [1, 2], "title": ["A", "B"], "summary": ["alpha alpha", "beta beta"]})
    g.add_nodes(df, "Doc", "id", "title")

    class _Stored:
        dimension = 8

        def embed(self, texts):
            return [[float(i + 1)] * 8 for i, _ in enumerate(texts)]

    g.set_embedder(_Stored())
    g.embed_texts("Doc", "summary", show_progress=False)
    kgl = tmp_path / "docs.kgl"
    g.save(str(kgl))
    manifest = tmp_path / "mcp.yaml"
    manifest.write_text(
        "name: torch\ntrust:\n  allow_embedder: true\nextensions:\n  embedder:\n"
        "    factory: torch_embed:build\n    model: stub\n",
        encoding="utf-8",
    )
    env = {
        **os.environ,
        "PYTHONPATH": os.pathsep.join(filter(None, [str(tmp_path), os.environ.get("PYTHONPATH", "")])),
        "TOKIO_WORKER_THREADS": "1",
    }
    client = _spawn_wheel_env(["--graph", str(kgl), "--mcp-config", str(manifest)], env)
    try:
        for i in range(5):
            out = _text_content(
                client.call_tool(
                    "cypher_query",
                    {"query": "MATCH (d:Doc) RETURN d.title AS t, text_score(d, 'summary', 'probe') AS s ORDER BY t"},
                )
            )
            assert "2 row(s)" in out, f"query {i} did not return both rows:\n{out}"
    finally:
        client.shutdown()
    assert client.proc.returncode == 0, f"server exited {client.proc.returncode}"
