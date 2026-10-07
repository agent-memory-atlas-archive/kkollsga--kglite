"""Python embedder construction for the bundled MCP server.

When a manifest declares `extensions.embedder` with a Python embedding library,
the Rust server hands the *whole* config object (as JSON) to
:func:`build_embedder` here. We pick the library, build the model, and return an
object satisfying kglite's ``EmbeddingModel`` protocol (``dimension`` +
``embed(texts) -> list[list[float]]``); the server wraps it in a
``PyEmbedderAdapter`` and calls it once per ``text_score()`` query.

Adding a library is a change **here only** — the Rust server stays agnostic
(it just asks "is this a Python library? hand it to Python"). The user installs
whichever library they name in the manifest:

```yaml
extensions:
  embedder:
    library: sentence-transformers   # pip install sentence-transformers
    model: BAAI/bge-m3               # ← works (fastembed-py has no bge-m3)
    # device: cpu                    # sentence-transformers only; Apple silicon defaults to cpu
# library: fastembed              → pip install fastembed
# factory: mypkg.embed:build      → any custom builder returning an EmbeddingModel
```

(`library: fastembed-rs` is the Rust engine — handled in the cargo binary, never
reaches here.)
"""

from __future__ import annotations

import importlib
import json


class _FastEmbedModel:
    """fastembed-py `TextEmbedding` → the EmbeddingModel protocol."""

    def __init__(self, model_name: str, cfg: dict) -> None:
        try:
            from fastembed import TextEmbedding
        except ImportError as exc:  # pragma: no cover - exercised only without fastembed
            raise RuntimeError(
                "extensions.embedder.library 'fastembed' is not installed: pip install fastembed"
            ) from exc
        self._model = TextEmbedding(model_name=model_name)
        # The protocol needs `dimension` up front; fastembed reveals it per
        # query, so probe once. (Raises here if the model name is unsupported —
        # e.g. fastembed-py has no bge-m3; use library: sentence-transformers.)
        probe = next(iter(self._model.embed(["x"])))
        self.dimension = int(len(probe))

    def embed(self, texts: list[str]) -> list[list[float]]:
        return [[float(x) for x in vec] for vec in self._model.embed(list(texts))]


def _default_st_device() -> str | None:
    """`cpu` where sentence-transformers would pick Apple's `mps`, else `None`
    (the library decides; CUDA stays automatic).

    The server embeds one short query per `text_score()`, so an accelerator buys
    ~35 ms per query but costs ~3 GB of GPU-allocator memory (in the process
    footprint, not RSS) and a CPU-heap cache that grows ~7 MB per distinct query
    length without bound. Measured for bge-m3: `mps` 3.8 GB at load and climbing,
    `cpu` 0.7 GB flat at ~65 ms per query.
    """
    try:
        import torch

        if not torch.cuda.is_available() and torch.backends.mps.is_available():
            return "cpu"
    except Exception:  # torch layout differs: let the library choose
        pass
    return None


class _SentenceTransformerModel:
    """sentence-transformers `SentenceTransformer` → the EmbeddingModel protocol.
    Loads any HuggingFace embedding model, including `BAAI/bge-m3`.

    `device:` in the manifest block overrides the placement (`cpu`, `cuda`,
    `mps`); without it Apple silicon runs on `cpu` (see `_default_st_device`)."""

    def __init__(self, model_name: str, cfg: dict) -> None:
        try:
            from sentence_transformers import SentenceTransformer
        except ImportError as exc:
            raise RuntimeError(
                "extensions.embedder.library 'sentence-transformers' is not installed: "
                "pip install sentence-transformers"
            ) from exc
        device = cfg.get("device") or _default_st_device()
        self._model = SentenceTransformer(model_name, device=device)
        self.dimension = int(self._model.get_sentence_embedding_dimension())
        self._home = self._model.device
        self._resident = True

    def _on_cpu(self) -> bool:
        return getattr(self._home, "type", self._home) == "cpu"

    def unload(self) -> None:
        """Give the accelerator its memory back; the weights stay in host RAM.

        Dropping the model object does not release `mps` memory (measured: a
        `del` plus `gc.collect()` left the 3 GB resident), so the weights are
        moved to the CPU first. `embed()` and `load()` move them back."""
        if self._resident and not self._on_cpu():
            self._model.to("cpu")
            self._resident = False
            import gc

            import torch

            gc.collect()  # the released tensors are freed only after a collection

            kind = getattr(self._home, "type", str(self._home))
            if kind == "mps":
                torch.mps.empty_cache()
            elif kind == "cuda":
                torch.cuda.empty_cache()

    def load(self) -> None:
        if not self._resident:
            self._model.to(self._home)
            self._resident = True

    def embed(self, texts: list[str]) -> list[list[float]]:
        self.load()
        return self._model.encode(list(texts)).tolist()


#: Curated Python embedding libraries. Anything outside this set goes through
#: the `factory:` escape (so we don't grow a wrapper per library).
_LIBRARIES = {
    "fastembed": _FastEmbedModel,
    "sentence-transformers": _SentenceTransformerModel,
}


def build_embedder(config_json: str):
    """Build an `EmbeddingModel` from the `extensions.embedder` config (JSON).

    Dispatch: `factory:` (a `module:attr` builder) wins; otherwise `library:`
    (default `fastembed`) selects a curated wrapper. Errors are raised with an
    actionable message and surface to the server as a boot failure.
    """
    cfg = json.loads(config_json)

    factory = cfg.get("factory")
    if factory:
        module_path, sep, attr = str(factory).partition(":")
        if not sep:
            raise RuntimeError(f"extensions.embedder.factory must be 'module:attr', got {factory!r}")
        try:
            fn = getattr(importlib.import_module(module_path), attr)
        except Exception as exc:
            raise RuntimeError(f"extensions.embedder.factory {factory!r} failed to import: {exc}") from exc
        return fn(cfg.get("model"))

    model = cfg.get("model")
    if not model:
        raise RuntimeError("extensions.embedder.model is required")
    library = cfg.get("library", "fastembed")
    cls = _LIBRARIES.get(library)
    if cls is None:
        known = ", ".join(sorted(_LIBRARIES))
        raise RuntimeError(
            f"unknown embedder library {library!r}; known Python libraries: {known} — "
            f"or use `factory: module:attr` for any other embedder. "
            f"(For the Rust engine use `library: fastembed-rs` on the cargo "
            f"`--features fastembed` binary.)"
        )
    return cls(model, cfg)
