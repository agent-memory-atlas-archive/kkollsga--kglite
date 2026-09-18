---
name: save_graph
description: "Persist the active graph to its bound `.kgl` file after mutating Cypher (CREATE / SET / DELETE / MERGE / REMOVE). TRIGGER after a chain of mutations the user explicitly wants kept. ALSO TRIGGER when the user says \"save\" or \"commit\" in the context of graph edits. SKIP for exploratory mutations the user is iterating on. The tool registers when `builtins.save_graph: true` or writable mode is enabled."
applies_to:
  mcp_methods: ">=0.3.36"
  kglite_mcp_server: ">=0.9.31"
references_tools:
  - save_graph
references_arguments: []
auto_inject_hint: true
applies_when:
  tool_registered: save_graph
---

# `save_graph` methodology

## Overview

`save_graph` writes the active graph back to its bound `.kgl` file. It is the **persistence tool** — call it once after a coherent chain of mutations the user wants kept. The tool registers when the manifest declares `builtins.save_graph: true` or the server is write-enabled with `--writable` / `extensions.writable: true`. The `builtins.save_graph` switch alone exposes save for already-dirty or boot-configured graph state; it does not authorize Cypher mutations.

## When the tool isn't registered

If neither `builtins.save_graph: true` nor writable mode enables the route, `save_graph` won't appear in `tools/list`. If the user asks to save changes and the tool isn't available, surface the gate clearly:

> "The server doesn't have save enabled — the operator can set `builtins.save_graph: true`, or enable writable mode when graph mutations are intended."

Don't try to write the file directly via `read_source` / shell tools; the active graph lives in memory and the on-disk format is binary. Use `save_graph` for the bound path or `save_graph_as` on a write-enabled server for another path.

## What gets saved

The entire active graph at the moment of the call. Specifically:

- All nodes (types, properties, including any added since boot)
- All edges (types and properties)
- All schema metadata (type-level introspection caches)

What does NOT get saved:

- Embedder state (those load lazily; if you've called `text_score` recently, the model is in memory but doesn't persist)
- Source-tool bindings (`source_roots`, watch handles — these are session state)
- Workspace state (clone inventory, active repo path — those live in their own files)

## When to call it

Once, after a coherent chain: CREATE → SET → SET → DELETE → `save_graph()`. Each call writes the whole file, so saving per statement writes it three times over for one change. Every storage mode works this way — a disk-backed mutation is session state until a save publishes it.

If the operator's intent is **try-it-and-see** mutations (a Cypher CREATE to see what the schema looks like with a hypothetical node, or a SET to test a query against modified data), don't call `save_graph` proactively, and don't call it after a read query at all. The next server restart discards uncommitted changes, which is the right behaviour. Save when the user says "save" / "commit" / "make this permanent."

## Sharing the file with other servers

The server holds the cross-process writer lease only **between your first unsaved change and the save that publishes it**. Outside that window the `.kgl` is lockable by anybody — other MCP clients on the same file, an external rebuilder, the `kglite` CLI. Two consequences for how you work:

- Don't sit on unsaved changes. While you hold them, every other client's write is refused by name. Save (or discard) when the chain is done rather than leaving the graph dirty across a long exploration.
- A refused write is never a lost write. Both refusals below say so explicitly, because the reflex on reading "refused" is to assume the mutation half-landed. It did not: the refusals happen before anything changes, or roll back to the state before the attempt.

## Error modes

- **"save_graph requires --graph mode (no source path bound)."** — the server booted in workspace mode (`--workspace dir/`) or source-root mode (`--source-root path/`); no `.kgl` to write back to. Expected.
- **"…is open for writing by …"** (a write, not a save) — another client is mid-write on the same file. Nothing changed here and the graph is still fully readable; keep querying. Retry once that client saves, and call `reload_graph` to pick up what it wrote. Never delete the `.lock` file to clear this: the lock lives in the OS, and deleting the file only removes the record of who holds it.
- **"…changed on disk since you loaded it"** (a save) — somebody else republished the file after this server read it, so saving would overwrite their version. Your unsaved changes are intact and still queryable. Two ways out, and they are alternatives — **there is no merge**:
  - `save_graph_as` to a different path keeps your work (and releases the original file, so the other writer is unblocked);
  - `reload_graph(discard_unsaved=true)` throws your work away and serves the file as it is on disk.
  Choose deliberately, and tell the user which one you took.
- **"…has unsaved changes"** (from `reload_graph` / `load_graph` / `create_graph`) — you asked to replace the active graph while holding work that only exists in memory. `save_graph` first to keep it, or `reload_graph(discard_unsaved=true)` to drop it. That flag is the *only* spelling for "throw it away"; the other two tools deliberately have no discard argument.
- **OSError on write** — disk full, permission denied, file removed. Surface to the user verbatim; the tool returns the underlying error message.
- **Read-only graph** — if the operator booted with a graph marked read-only (rare; via `KnowledgeGraph(read_only=True)`), the in-memory mutations would have failed earlier. Save can't fix that.
