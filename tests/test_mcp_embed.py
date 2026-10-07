"""Unit tests for `kglite._mcp_embed.build_embedder` — the Python-side embedder
dispatch for the bundled MCP server's `extensions.embedder.library`.

Dispatch + error messaging only (no model downloads / network)."""

from __future__ import annotations

import json
import sys

import pytest

from kglite import _mcp_embed


def _cfg(**kw) -> str:
    return json.dumps(kw)


def test_factory_escape_is_called(tmp_path) -> None:
    # A `factory: module:attr` builder is imported and called with the model.
    mod = tmp_path / "myembed.py"
    mod.write_text(
        "class _Stub:\n"
        "    dimension = 3\n"
        "    def embed(self, texts):\n"
        "        return [[0.0, 0.0, 0.0] for _ in texts]\n"
        "def build(model):\n"
        "    s = _Stub(); s.model = model; return s\n",
        encoding="utf-8",
    )
    sys.path.insert(0, str(tmp_path))
    try:
        obj = _mcp_embed.build_embedder(_cfg(factory="myembed:build", model="anything"))
    finally:
        sys.path.remove(str(tmp_path))
    assert obj.dimension == 3
    assert obj.model == "anything"


def test_factory_must_be_module_colon_attr() -> None:
    with pytest.raises(RuntimeError, match="module:attr"):
        _mcp_embed.build_embedder(_cfg(factory="no_colon_here"))


def test_factory_import_failure_is_clear() -> None:
    with pytest.raises(RuntimeError, match="failed to import"):
        _mcp_embed.build_embedder(_cfg(factory="nonexistent_pkg_xyz:build", model="m"))


def test_unknown_library_lists_known() -> None:
    with pytest.raises(RuntimeError, match="unknown embedder library 'frobnicate'"):
        _mcp_embed.build_embedder(_cfg(library="frobnicate", model="m"))


def test_missing_model_errors() -> None:
    with pytest.raises(RuntimeError, match="model is required"):
        _mcp_embed.build_embedder(_cfg(library="fastembed"))


def test_sentence_transformers_not_installed_message() -> None:
    # sentence-transformers is not a kglite dependency; the adapter must raise an
    # actionable "pip install sentence-transformers" rather than a bare ImportError.
    if "sentence_transformers" in sys.modules or _st_importable():
        pytest.skip("sentence-transformers is installed in this env")
    with pytest.raises(RuntimeError, match="pip install sentence-transformers"):
        _mcp_embed.build_embedder(_cfg(library="sentence-transformers", model="BAAI/bge-m3"))


def _st_importable() -> bool:
    import importlib.util

    return importlib.util.find_spec("sentence_transformers") is not None


# --- sentence-transformers device selection and release -----------------------
#
# Measured on Apple silicon (bge-m3, 2026-10): the library's automatic device
# choice is `mps`, which holds ~3 GB of GPU-allocator memory per server (counted
# in the process footprint, not in RSS) and grows the CPU heap by several MB per
# distinct query length (shape-keyed graph cache). `cpu` stays at ~0.7 GB flat.


class _FakeTensorModel:
    """Stands in for `SentenceTransformer`; records placement, never loads weights."""

    instances: list = []

    def __init__(self, name, device=None):
        self.name = name
        self.device_arg = device
        self.device = device or "auto-device"
        self.moves: list = []
        type(self).instances.append(self)

    def get_sentence_embedding_dimension(self):
        return 4

    def to(self, device):
        self.moves.append(device)
        self.device = device
        return self

    def encode(self, texts):
        class _Out:
            def tolist(self_inner):
                return [[0.0, 0.0, 0.0, 0.0] for _ in texts]

        return _Out()


def _install_fake_stack(monkeypatch, *, cuda: bool, mps: bool):
    import types

    emptied: list = []
    torch = types.ModuleType("torch")
    torch.cuda = types.SimpleNamespace(is_available=lambda: cuda, empty_cache=lambda: emptied.append("cuda"))
    torch.mps = types.SimpleNamespace(empty_cache=lambda: emptied.append("mps"))
    torch.backends = types.SimpleNamespace(mps=types.SimpleNamespace(is_available=lambda: mps))
    st = types.ModuleType("sentence_transformers")
    _FakeTensorModel.instances = []
    st.SentenceTransformer = _FakeTensorModel
    monkeypatch.setitem(sys.modules, "torch", torch)
    monkeypatch.setitem(sys.modules, "sentence_transformers", st)
    return emptied


def _build_st(**extra):
    return _mcp_embed.build_embedder(_cfg(library="sentence-transformers", model="m", **extra))


def test_sentence_transformers_defaults_to_cpu_where_the_library_would_pick_mps(monkeypatch) -> None:
    _install_fake_stack(monkeypatch, cuda=False, mps=True)
    _build_st()
    assert _FakeTensorModel.instances[0].device_arg == "cpu"


def test_sentence_transformers_leaves_cuda_to_the_library(monkeypatch) -> None:
    _install_fake_stack(monkeypatch, cuda=True, mps=False)
    _build_st()
    assert _FakeTensorModel.instances[0].device_arg is None


def test_sentence_transformers_device_key_is_honoured(monkeypatch) -> None:
    _install_fake_stack(monkeypatch, cuda=False, mps=True)
    _build_st(device="mps")
    assert _FakeTensorModel.instances[0].device_arg == "mps"


def test_sentence_transformers_unload_leaves_the_accelerator_and_embed_returns(monkeypatch) -> None:
    emptied = _install_fake_stack(monkeypatch, cuda=False, mps=True)
    embedder = _build_st(device="mps")
    model = _FakeTensorModel.instances[0]
    embedder.unload()
    assert model.moves == ["cpu"]
    assert emptied == ["mps"]
    embedder.unload()  # idempotent
    assert model.moves == ["cpu"]
    assert embedder.embed(["x"]) == [[0.0, 0.0, 0.0, 0.0]]
    assert model.moves == ["cpu", "mps"]  # embed() puts it back on its device


def test_sentence_transformers_unload_is_a_noop_on_cpu(monkeypatch) -> None:
    emptied = _install_fake_stack(monkeypatch, cuda=False, mps=True)
    embedder = _build_st()
    embedder.unload()
    assert _FakeTensorModel.instances[0].moves == []
    assert emptied == []
