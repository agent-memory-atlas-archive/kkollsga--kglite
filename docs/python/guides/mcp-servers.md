# MCP Servers

> [Model Context Protocol](https://modelcontextprotocol.io/) is the
> protocol Claude / Cursor / agentic CLIs use to call tools. Your
> KGLite graph becomes a server that speaks it over stdin/stdout. The
> agent gets Cypher access to your data through ordinary tool
> calls — no API to learn, no infrastructure to manage.

`kglite-mcp-server` is a **single, pure-Rust server** built on the
[mcp-methods] framework (rmcp + manifest-driven tool registration; no
Python runtime, no libpython link). It exposes your graph as
`graph_overview` + `cypher_query` over MCP stdio.

For project-specific tools, drop a YAML manifest next to your graph. The server picks it up automatically. Project-specific tools include:

- semantic search;
- source-file access;
- parameterised Cypher lookups;
- query preprocessing.

**No fork required for most customisation.**

> **0.10.26:** the server is reachable two ways, both running the
> identical Rust implementation.
>
> - `pip install kglite` bundles it *inside* the wheel. It is statically linked into the extension and shares the one engine, with no separate wheel and no duplicated engine. The wheel exposes the `kglite-mcp-server` command via a thin console-script shim.
> - `cargo install kglite-mcp-server` gives the same command as a standalone libpython-free binary.
>
> History: through 0.10.24 the wheel shipped a *Python* server. 0.10.25 retired it for cargo-only to stop two implementations drifting. 0.10.26 brought the command back to `pip` as the bundled Rust server.

[mcp-methods]: https://github.com/kkollsga/mcp-methods

## Quick Start

### 1. Install

```bash
pip install kglite          # ships the kglite-mcp-server command in the wheel
# — or, for a standalone binary with no Python at all:
cargo install kglite-mcp-server
```

Either way the `kglite-mcp-server` command lands on PATH running the same Rust server. Run `kglite-mcp-server --help` to confirm.

Response-control and expansion names are published by MCP discovery. They can be renamed to avoid domain-tool collisions, so clients should copy the returned action rather than assume either name. See [Bounded agent responses](../../operators/agent-responses.md) for an executable compact-to-targeted-expansion workflow.

#### Semantic search in the server

For semantic search (`text_score()`), name an embedding engine in the manifest `extensions.embedder` block. You provide the `library` and the `model`, and install that library:

- **pip wheel** → a Python library:
  - `library: sentence-transformers` (`pip install sentence-transformers`) has `bge-m3` + all of HuggingFace.
  - `library: fastembed` (`pip install fastembed`) is light, but has no `bge-m3`.
- **standalone cargo binary** → `library: fastembed-rs` + `cargo install kglite-mcp-server --features fastembed`. It needs no Python in the deployment and has `bge-m3`.

See the [embedder example](../examples/manifest_with_embedder.md). The two fastembeds are *separate* libraries with different catalogs. `bge-m3` is in fastembed-rs + sentence-transformers, **not** fastembed-py. The runtime model must match the one the graph was embedded with.

### 2. Point it at a graph file

```bash
kglite-mcp-server --graph /path/to/my_graph.kgl
```

The server speaks MCP over stdio. Its core tools are:

- `graph_overview(...)` — wraps `describe()` for progressive schema disclosure (types, connections, Cypher reference).
- `cypher_query(query)` — runs any Cypher query. It returns up to 15 rows inline. Append `FORMAT CSV` for a localhost-served file export.
  - An optional `valid_at` (an ISO date or datetime) runs the query as of that instant, behind the `FOR VALID_TIME AS OF` prefix. `"all"` reads every version (`FOR VALID_TIME ALL`).
  - With neither, a statement on a graph that declares validity reads as of today (UTC).
  - The `temporal:` line reports five things:
    - the source (`default`, `explicit`, `all` or `skipped:<reason>`);
    - the instant;
    - the route;
    - the total rows the context hid with the heaviest targets;
    - the relationships hidden only through an invalid endpoint (`endpoint_invalid`).
  - An agent that needs history sends `FOR VALID_TIME ALL` or `valid_at: "all"`.
- `ping(message?)` — liveness probe. It echoes the message or returns `pong`.

For semantic search (`text_score()` inside Cypher) or source-file access tools, drop a manifest. See step 4 below or the [Customising with a manifest](#customising-with-a-manifest) section.

### Agent graph workbench (opt-in writes)

Servers are read-only by default. A server is **write-enabled** when either `--writable` is passed on the command line or the manifest sets `extensions.writable: true`. That is one statement made two ways, and either alone is sufficient. Use it when the agent must mutate or switch graphs:

```bash
# Open an existing graph with mutation + lifecycle tools.
kglite-mcp-server --graph /data/work.kgl --writable

# Create a missing graph explicitly in one of the three storage modes.
kglite-mcp-server --graph /data/new.kgl --storage memory --writable
# --storage mapped|disk may point at a directory-backed graph instead.
```

```yaml
# the manifest half of the same switch
extensions:
  writable: true
```

Write-enabled mode registers mutation-capable `cypher_query` plus `save_graph` and the `load_graph`, `create_graph`, and `save_graph_as` lifecycle tools.

- `builtins.save_graph: true` is **not** a third spelling for it. On its own that key registers `save_graph` and nothing else, and `cypher_query` stays read-only.
- `--storage` is a creation choice, not a silent conversion of an existing graph.
- Keep the default read-only mode for untrusted clients.

A mutation refused on a read-only server names both write-enabling spellings. Any `extensions:` key this server does not read is reported at boot beside the ones it does. A misspelled `writable` is therefore visible in the log rather than silently leaving the server read-only.

When writes should be type-scoped, note who is doing the scoping. The `write_scope` argument on `cypher_query` is the *agent's* own declaration. It is useful role hygiene, but the agent can always widen it. To pin a ceiling the agent cannot reach, pass `--write-scope` (comma-separated) or set `extensions.write_scope` in the manifest:

```bash
kglite-mcp-server --graph /data/work.kgl --writable --write-scope Plan,Task
```

```yaml
extensions:
  write_scope: [Plan, Task]
```

The pin applies whether or not the agent supplies its own scope.

- An omitted `write_scope` leaves the pin in force. It never falls back to unrestricted.
- A supplied `write_scope` is **intersected** with the pin.
- A write with nothing left in scope is refused with a message naming the server's scope.
- Flag and manifest key are intersected with each other as well.
- The effective scope is logged at boot.
- A malformed `extensions.write_scope` fails the boot rather than being silently ignored.

#### Letting reads use the parallel runtime

By default the server leaves the engine's opt-in parallel regions switched off, whatever the machine has. Those regions are candidate scans, fused scan-aggregates, aggregations and `ORDER BY` sort keys. A server's cores belong to its clients, so nothing spends them by omission.

On a deployment that serves one agent against a large graph, opt in with either surface: the flag, the manifest key, or both:

```bash
kglite-mcp-server --graph /data/big.kgl --parallel
```

```yaml
extensions:
  parallel: true
```

The two are OR'd. `write_scope` is intersected instead, because a write scope is a perimeter and this is a resource permission. A wrapper that owns the manifest but not the command line, and a bare binary with no manifest at all, therefore each have a working way to say yes. A malformed `extensions.parallel` fails the boot rather than being silently ignored.

The pin does **not** do two things.

- It is a permission, not an instruction. The engine still applies its own per-operator size gate, so a small query runs sequentially either way, and the answer is identical with the pin on or off.
- It covers **reads only**. A `--writable` server keeps running mutations sequentially, because a write is the one place where extra cores are a surprise rather than a speed-up.

The pin is also not the engine's only use of threads. A handful of regions whose fan-out was measured to be an unconditional win (notably projecting a large result set) parallelise above their own row thresholds whether or not it is set. The pin controls the set of regions the engine holds back by default.

Pool width is the machine's `available_parallelism`, overridable with the `KGLITE_QUERY_THREADS` environment variable. One pool is shared by both kinds of region. When the pin is on, the boot log records it together with the width it resolved.

### 3. Register with Claude Desktop

Add to your Claude Desktop config (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS):

```json
{
  "mcpServers": {
    "my-graph": {
      "command": "/abs/path/to/your/venv/bin/kglite-mcp-server",
      "args": ["--graph", "/abs/path/to/my_graph.kgl"]
    }
  }
}
```

**Use the absolute path to the binary in `command`, not a bare `kglite-mcp-server`.** A bare command is resolved against `$PATH`. If an older install sits earlier on `$PATH` (a stray `cargo install`, a Conda base env, a previous editable build), the client silently launches *that* one. That stale server may register a different tool set or lazy-load tools your client then can't see. There is no error; the tools just quietly differ.

Point `command` at the exact binary you mean. `which kglite-mcp-server` inside your active env prints it. See [Operator notes](#operator-notes) on multi-install PATH order.

For Claude Code, add the same shape to `.claude/settings.json`. The agent can now call `graph_overview()` to learn the schema and `cypher_query()` to query.

```{important}
**Restart after any config change.** The manifest and the client's MCP
config are read **once, at server boot**. If you edit this JSON, the
manifest YAML, or a `.env`, the running server won't pick it up — fully
restart Claude Desktop / your MCP client (or the standalone process) so it
re-reads them. A surprising number of "my change had no effect" reports are
just this.
```

### Verify your setup

A misconfigured server fails *silently*: missing tools, github tools hidden for lack of a token, a stale PATH-shadowing binary, or "No active graph". The absence of errors therefore doesn't mean it's working. Run the built-in self-test to get a positive green/red answer:

```bash
kglite-mcp-server --selftest --graph /abs/path/to/my_graph.kgl
# …or with a manifest / workspace:
kglite-mcp-server --selftest --mcp-config /abs/path/to/manifest.yaml
```

The self-test re-spawns the server with the *same* flags, drives a real MCP handshake (`initialize` → `tools/list` → activate → `cypher_query`), and prints one line per capability:

```
kglite-mcp-server --selftest  (mode: single-graph)
  ✓ server initializes: serverInfo.name = KGLite (single-graph)
  ✓ graph tools registered: cypher_query + graph_overview present
  – github tools: none registered (needs `builtins.github: true` in the manifest, then a reachable GITHUB_TOKEN)
  ✓ graph hydrates: MATCH (n) RETURN count(n) → 1 row(s):
Selftest PASSED — the server is configured correctly.
```

It exits non-zero if any check fails, so you can also wire it into a deployment or CI smoke gate. Pass the *absolute path to the binary you registered* (per the caveat above), so the self-test exercises the same server your client launches.

The marks mean:

- `✓` — a working capability.
- `✗` — a failure.
- `–` — a capability that is absent or degraded without breaking the server.

For example, a manifest `source_root:` that no longer exists prints `– manifest source roots: source tools unavailable — …`, and the run still passes. The graph tools it exists to serve are intact.

When the child never answers, the failing check quotes the child's last stderr lines as the cause, so a CI log is readable without scrolling.

### 4. (Optional) Add a manifest for more tools

Drop a sibling YAML file next to your graph and you get three more tools without writing any Python:

```yaml
# my_graph_mcp.yaml
source_root: ./data
```

That auto-registers `read_source`, `grep`, and `list_source` over the `./data` directory (sandboxed, ripgrep-backed, gitignore-aware). Cypher narrows the search at the graph level. The agent follows up with `read_source` for the top hits, or `grep` for context the graph didn't lift. The full reference is in [Customising with a manifest](#customising-with-a-manifest) below.

## Customising with a manifest

A **manifest** is a YAML file that sits next to your graph and tells `kglite-mcp-server` to register additional tools. Drop a file named `<graph_basename>_mcp.yaml` alongside your graph and it loads automatically:

```
demo.kgl
demo_mcp.yaml      ← auto-detected sibling
```

Or point at any path with `--mcp-config`:

```bash
kglite-mcp-server --graph demo.kgl --mcp-config /path/to/manifest.yaml
```

A manifest can declare several kinds of additions. All are optional:

| Section | What it does | Trust |
|---|---|---|
| `source_root:` / `source_roots:` | Auto-registers `read_source` / `grep` / `list_source` over the directory tree | None — read-only |
| `tools[].cypher` | Parameterised Cypher templates as named MCP tools | None — read-only |
| `extensions.embedder` | Registers an embedder so `text_score()` works inside Cypher | `trust.allow_embedder: true` |
| `extensions.csv_http_server` | Localhost listener that serves `FORMAT CSV` exports as URLs | None |
| `extensions.value_codecs` | Position-scoped literal conversions (`'Q42'↔42`) bound to a property, applied after parsing | none (declarative; presence is opt-in) |
| `workspace:` | Bind a local directory (or clone-and-track GitHub repos) as the active source root | None |
| `extensions.writable: true` | Write-enables the server: mutation through `cypher_query`, plus `save_graph` and the `load_graph` / `create_graph` / `save_graph_as` lifecycle tools. Same statement as `--writable` | Full write access to the graph |
| `builtins.save_graph: true` | Registers `save_graph` **only** — so a server can persist what it loaded (a boot-time ontology materialization, say). Does not enable mutation | None — `cypher_query` stays read-only |

### `source_root:` — first-class source-file access

Most knowledge graphs index *something*: a codebase, a JSON corpus, scraped documents. The agent flow is almost always the same. Cypher narrows the search, source read fetches the top hits, and an occasional grep finds context that didn't make it into the graph. Wire it in with one line:

```yaml
# demo_mcp.yaml
source_root: ./data
```

`./data` is resolved relative to the yaml file's directory, so a manifest in `/proj/demo_mcp.yaml` exposes `/proj/data`. Use `../` to point at a sibling directory:

```yaml
source_root: ../scrape
```

For multi-root setups, use `source_roots:`:

```yaml
source_roots:
  - ./data
  - ../shared/lookups
```

A declared root that does not exist is **not** a boot failure, and resolution is **per root**. For each entry that fails, the server logs a `WARN`. It serves the roots that resolved and names the rest in its boot summary and in the agent's `instructions`. It answers `initialize` normally, because the graph tools never read the source root.

- **Some roots missing.** The surviving roots are searched as usual. The missing ones are simply absent from every result, and no per-call message says so. That is why the agent's `instructions` name them.
- **All roots missing.** The three source tools stay listed. Each call answers "no active source root", followed by a line per declared root naming it and the path it was looked for at.
- **`--graph` mode never falls back from a declared root.** With no declaration it binds the `.kgl`'s parent directory. When a declaration resolves to nothing, the `.kgl`'s parent directory is not auto-bound in its place. Serving a directory the operator never asked for would be a silent wrong answer.

Fix the path (or create the directory) and restart to pick the root back up. This requires kglite 0.16.18+ / mcp-methods 0.4.7+. Earlier versions resolved all-or-nothing, so one missing entry cost every root.

`source_root:` auto-registers three tools, all sandboxed to the configured roots:

**`read_source(file_path, ...)`** — read a file relative to the source root. Use `grep="pattern"` to filter to matching lines instead of dumping everything. This is essential for large files: agents can search a 50 MB JSON without exhausting context.

| Parameter | Type | Default | Notes |
|---|---|---|---|
| `file_path` | string | (required) | Relative to a configured source root. |
| `start_line` / `end_line` | int / int | `1` / EOF | 1-indexed line slice. |
| `grep` | string | `None` | Filter to lines matching this regex. |
| `grep_context` | int | `2` | Lines of context around each match. |
| `max_matches` | int | (none) | Cap matches when `grep` is set. |
| `max_chars` | int | (none) | Cap output size. |

**`grep(pattern, ...)`** — regex search across all files in the source roots. It is backed by ripgrep crates and is gitignore-aware by default.

| Parameter | Type | Default | Notes |
|---|---|---|---|
| `pattern` | string | (required) | Regex pattern. |
| `glob` | string | `*` | File-name glob filter. |
| `context` | int | `0` | Lines of context around matches. |
| `max_results` | int | `50` | Cap result count. |
| `case_insensitive` | bool | `false` | Toggle case sensitivity. |

**`list_source(...)`** — tree-formatted directory listing under the first source root.

| Parameter | Type | Default | Notes |
|---|---|---|---|
| `path` | string | `.` | Directory relative to source root. |
| `depth` | int | `1` | Tree depth; `2+` is recursive. |
| `glob` | string | `None` | Filter entries by name. |
| `dirs_only` | bool | `false` | Hide files; directories only. |

All path resolution is sandboxed. A `..` traversal that escapes the configured roots is rejected.

### `tools:` — inline Cypher tools

Declare Cypher templates as named MCP tools. Each entry becomes a top-level tool the agent can call by name with typed parameters:

```yaml
tools:
  - name: similar_sessions
    description: Top-k semantically similar sessions for a session id.
    parameters:
      type: object
      properties:
        session_id:
          type: string
        top_k:
          type: integer
          default: 5
      required: [session_id]
    cypher: |
      MATCH (s:Session {id: $session_id})-[r:SIMILAR_TO]->(t:Session)
      RETURN t.id AS id, t.title AS title, r.score AS score
      ORDER BY score DESC LIMIT $top_k
```

The agent sees `similar_sessions(session_id, top_k=5)` as a regular MCP tool. Param names in the Cypher (`$session_id`, `$top_k`) bind to same-named values in the call arguments.

The `parameters:` object is published as the tool's MCP input schema, so clients can construct calls. KGLite does not validate that schema, compare it with the template's `$param` references at boot, or perform JSON Schema validation when dispatching a call. Missing or incompatible values surface through the normal Cypher execution error response.

The one keyword the server acts on is `default:` on a top-level property. When the agent omits that argument, the declared value is bound before the template runs. `top_k` above is therefore optional, and `$top_k` is never unbound.

- Only an absent argument is filled. An explicit value wins, and an explicit `null` stays null.
- A parameter with no default that the agent omits reaches the engine unbound and fails with `Missing parameter: $top_k`. `coalesce($top_k, 5)` cannot repair that, because the parameter is absent rather than null.
- Defaults in these uncompiled manifest schemas are bound exactly as written.
- Nested defaults are ignored, since only a top-level property is a `$parameter`.

JSON numeric parameters are still admitted exactly before execution.

- Integer tokens at any nesting depth must fit the signed 64-bit range.
- Decimal and exponent tokens must fit a finite 64-bit float.
- A refusal names its nested array/object path.

The same rule applies to the built-in `cypher_query`, these manifest templates and recipe variables.

Manifest Cypher tools cap output at 15 rows / 2k chars. For full result exports, agents use the bundled `cypher_query` with `FORMAT CSV`.

### `extensions.cypher_recipes` — grouped, structured read queries

Recipe catalogs group exact, repeated, read-only Cypher operations behind two stable tools. This replaces registering one top-level tool per query:

- `list_recipe_queries(recipe?)` — omit `recipe` for compact catalog summaries. Provide it to disclose that recipe's query descriptions and parameter schemas. The listing tool never returns stored Cypher.
- `run_recipe_query(recipe, query, variables, include_cypher=false, valid_at?)` — run one exact operation with strictly validated variables.
  - `variables` is always required; parameter-free queries receive `{}`.
  - `valid_at` (an ISO date or datetime) runs the stored query as of that instant, behind the `FOR VALID_TIME AS OF` prefix that `cypher_query`'s `valid_at` writes. `"all"` reads every version.
  - Without `valid_at`, the stored query reads as of today on a graph that declares validity. A recipe that means history starts with `FOR VALID_TIME ALL`, which does nothing on a graph with no declaration.
  - The result's `diagnostics.temporal` echoes it.

`run_recipe_query`'s own description carries the catalogue. The routing is in `tools/list`, so an agent does not have to discover it. The description holds the static sentence above, then one line per query in catalogue order:

`recipe.query — description; params: name: type [one of …] [=default] (required)`

A parameter-free query shows `none`.

The input schema lists the real names too. `recipe` and `query` each carry an `enum` of what this deployment serves, while `variables` stays an open object pointing at that block. Past 4 000 characters the block degrades to `recipe.query` names and points at `list_recipe_queries` for the parameters. A large catalogue therefore cannot crowd out the rest of the tool list.

A non-empty catalog also adds this hint to a bare `graph_overview()` response:

```xml
<query-catalog recipes="1" queries="3" list-tool="list_recipe_queries"
  run-tool="run_recipe_query" names="code_review.direct_callers, …"/>
```

That is progressive discovery, not a catalog dump.

- If a domain skill already names an operation, call `run_recipe_query` directly.
- Otherwise list compact summaries once, inspect only a plausible recipe, and use it only when its documented scope is an exact match.
- Focused `graph_overview(...)` calls omit the hint.

Each recipe and query has a description. Every query declares a closed root parameter schema and parameterized Cypher:

```yaml
extensions:
  cypher_recipes:
    code_review:
      description: Exact Function-scoped operations for an initial code review.
      queries:
        direct_callers:
          description: Return Function nodes with a direct Function-to-Function CALLS edge to the target Function.
          parameters:
            type: object
            properties:
              qualified_name: {type: string}
            required: [qualified_name]
            additionalProperties: false
          cypher: |
            MATCH (caller:Function)-[:CALLS]->(target:Function)
            WHERE target.qualified_name = $qualified_name
            RETURN DISTINCT caller.qualified_name AS qualified_name,
                            caller.file_path AS file_path
            ORDER BY qualified_name, file_path
```

The full three-query code-review example, including its resolve-first domain skill, is [`examples/local_code_review_mcp.yaml`](https://github.com/kkollsga/kglite/blob/main/examples/local_code_review_mcp.yaml).

Catalogs are immutable after boot. That includes the graph-carried half below and every `tool:` route it registers. A recipe added to the graph, or a `tool:` added to one, is served after a restart, not on the next `reload_graph`. (Skills differ: they *are* re-resolved on a graph swap.)

KGLite checks each catalog at boot:

- It parses every stored query.
- It requires an exact match between `$parameters` and root `properties`.
- It requires `required` to list every property that has no `default`. `required` may be left out when every property has one; absent means none.
- It rejects mutations, `EXPLAIN`, `PROFILE`, `FORMAT CSV`, and `LOAD CSV`.

Supported schema keywords are deliberately limited to `type`, `properties`, `required`, `items`, `enum`, `minimum`, `maximum`, `minItems`, `maxItems`, `additionalProperties`, `description`, and `default`. Unsupported keywords fail boot.

`type` may be a supported type name or an array such as `[string, "null"]`.

- Integer values must fit KGLite's signed 64-bit range.
- Decimal and exponent values must fit a finite 64-bit float.
- Equivalent finite spellings such as `1.0` and `1e0` compare as the same numeric enum value.

A top-level property may declare `default:`. That is what makes a recipe parameter optional. The value is bound before validation when the caller omits the key, so the stored Cypher still sees every `$parameter`. An explicit value wins and an explicit `null` stays null.

Because the default supplies the value, such a property must **not** appear in `required`. Boot refuses three things:

- listing a defaulted property in `required`;
- a default that does not satisfy its own property (wrong type, outside `enum`, past `minimum`/`maximum`);
- a `default` nested below a top-level property, which could never bind a parameter.

Successful execution returns MCP `structuredContent`. The text content is the same serialized JSON, for clients that only expose text:

```json
{
  "recipe": "code_review",
  "query": "direct_callers",
  "result": {
    "columns": ["qualified_name", "file_path"],
    "rows": [["pkg::caller", "src/caller.rs"]],
    "row_count": 1
  }
}
```

Columns plus positional rows are canonical. With `include_cypher=true`, the response adds both the stored parameterized `cypher` and its separate `parameters` map. Both are omitted when false. The pairing also applies to runtime errors after the recipe/query has been resolved.

An empty successful result (`rows: []`, `row_count: 0`) only means the stored operation matched no rows. It cannot by itself distinguish a missing target from an existing target with no matching relationships.

Entity-oriented recipes therefore provide a `resolve_*` query. Their domain skill must run that preflight before interpreting an empty neighborhood. In the example, `resolve_function` is mandatory before `direct_callers` or `affected_tests`.

Recipe payloads are all-or-error.

- Up to 200 returned rows are returned in full.
- 201 or more return `result_limit_exceeded` with exact `details.limit` and `details.observed_count`, and no partial result.

This bounds the MCP return payload, not intermediate query work.

- A literal stored `LIMIT 200` is reserved and rejected, because it would disguise overflow as a complete result.
- Other semantic limits must genuinely define the named operation.
- Example and bundled queries use explicit `ORDER BY`. Without it, a third-party query's row order is undefined.

Errors use structured envelopes with stable codes: `invalid_request`, `unknown_recipe`, `unknown_query`, `invalid_variables`, `no_active_graph`, `stale_graph`, `query_failed`, and `result_limit_exceeded`.

- A `stale_graph` response is a hard failure and contains no graph data. Fix the reported workspace rebuild problem before retrying.
- `query_failed.details.cause` contains a stable category, the closest KGLite error code, a safe message, and the position when available.
- Multi-revision misuse and an unknown revision use the distinct `multi_revision_graph_required` and `unknown_revision` categories. They do not collapse into a generic execution failure.
- Numeric admission failures use `invalid_variables`. Each `details.issues[]` entry retains its `path`, stable `category`, and message.

Recipes are narrow convenience operations. Use raw `cypher_query` for broader entity kinds, different relationships, deeper/unbounded paths, or any question that does not exactly match a stored operation. Use `FORMAT CSV` through raw Cypher (optionally with `extensions.csv_http_server`) for large exports. Recipe queries intentionally reject CSV mode.

#### Recipes carried in the graph

The manifest is not the only source. A `.kgl` can store the same queries as nodes under the `KgliteRecipe` system label. A graph that ships a skill can then ship the exact queries that skill names. The agent runs `run_recipe_query("code_review", "callers_page", {...})` instead of rewriting Cypher the graph's author already got right.

```python
graph.set_recipe(
    "code_review",
    "callers_page",
    "Functions with a direct CALLS edge to the target.",
    "MATCH (c:Function)-[:CALLS]->(t:Function) WHERE t.qualified_name = $qualified_name "
    "RETURN c.qualified_name AS qualified_name ORDER BY qualified_name LIMIT 25",
    {"type": "object", "properties": {"qualified_name": {"type": "string"}},
     "required": ["qualified_name"], "additionalProperties": False},
    "Exact Function-scoped operations for an initial code review.",
)
graph.save("code.kgl")
```

`list_recipes()`, `get_recipe()`, `set_recipe()`, `delete_recipe()`, `import_recipes()` and `export_recipes()` manage the catalogue from Python.

- Every write is held to exactly the rules above. A query that would be skipped at boot is refused when you store it.
- `parameters` is a native nested map, not a JSON string. `r.parameters.type` reads from Cypher.
- Import and export use this same `extensions.cypher_recipes` document shape, as JSON.
- `describe()` lists the groups in a `<recipes count="N">` element.

At boot, a server in `--graph` or `--watch` mode compiles the served graph's catalogue and merges it **under** the manifest's:

- The **manifest wins** per `(recipe, name)` and per group description. Everything the graph alone carries is kept. The operator must be able to correct or replace a query the graph ships without rebuilding the graph.
- A graph with recipes therefore gets `list_recipe_queries` / `run_recipe_query`, the bundled `recipe_queries` methodology and the `<query-catalog/>` overview hint **with no catalogue in the manifest at all**. The hint's `recipes=` / `queries=` counts are the merged totals.
- A manifest query that does not compile still **fails the boot**. A graph record that does not compile is **skipped with a warning** naming it and the rule, and its siblings still serve. Graph content is data; a raw `CREATE` that bypassed validation entirely may have written it.
- **Graph, watch and vault modes only**, for the same reason skills give: the other modes have no graph open when the routes are registered, and the catalogue is immutable afterwards.
- The boot summary reports what the graph contributed, for example `graph recipes: 3 served, 1 overridden by the manifest, 1 skipped: …`.

Unlike skills, the graph recipe layer has **no `skills:`-style opt-in**. A served graph's recipes are read whenever the mode has a graph. They cannot change a tool's description.

They *can* add a tool name. A query that declares `tool:` is registered under it (see `extensions.recipe_tools` below), but only under a name the operator did not already allow to something else. A collision with any registered route refuses the boot, and every query is validated read-only before it is served.

[`examples/code_review_graph_skills.py`](https://github.com/kkollsga/kglite/blob/main/examples/code_review_graph_skills.py) builds a graph carrying three recipes and the skill that names them.

#### Recipes registered by the producer

A third source suits a binary that embeds the server and builds its own graphs. `ServerExtensions::with_recipes` registers one catalogue per server, in **every** mode. The queries belong to the shapes the builder emits, so they apply to every graph the server goes on to serve. That includes a workspace deployment that has no graph at boot and no manifest at all.

```rust
use kglite_mcp_server::{run_with_extensions, RecipeCatalog, ServerExtensions};

let catalog = RecipeCatalog::from_manifest_value(Some(&serde_json::json!({
    "code": {
        "description": "Queries for the shapes this builder emits.",
        "queries": {
            "callers": {
                "description": "Functions with a direct CALLS edge to the target.",
                "parameters": {
                    "type": "object",
                    "properties": {"qualified_name": {"type": "string"}},
                    "required": ["qualified_name"],
                    "additionalProperties": false
                },
                "cypher": "MATCH (c:Function)-[:CALLS]->(t:Function) \
                           WHERE t.qualified_name = $qualified_name \
                           RETURN c.qualified_name AS qualified_name"
            }
        }
    }
})))?;
run_with_extensions(
    std::env::args_os(),
    ServerExtensions::new().with_recipes(catalog),
)?;
```

`RecipeCatalog` is re-exported from `kglite_mcp_server`. The document shape is the same `extensions.cypher_recipes` JSON a manifest carries, so a producer can ship the catalogue as an asset and load it with one call.

- **Merge order is `producer < graph < manifest`**, per `(recipe, name)` and per group description. Queries only one layer carries are all served. It is the same rule as everywhere else: closer to the operator wins.
- **Registered in every mode.** The two fixed route names are mode-blind. A producer catalogue alone gives a manifest-less workspace server `list_recipe_queries` and `run_recipe_query`, plus a route per query that declares a `tool:`.
- **A producer query that does not compile fails the boot**, like a manifest one. It is the embedder's code, not graph data.
- **An operator allowlist is unaffected by a catalogue they did not declare.** `extensions.tools_allow` refuses a boot that hides the recipe routes from a catalogue the *manifest* declares. The operator wrote the queries and the allowlist, and the contradiction is theirs to fix. A graph-carried or producer catalogue never arms that refusal. The operator did not ask for those routes, so an allowlist that omits them is a choice, not a mistake.
- **`--selftest` counts the routes against the catalogue actually served.** A graph-carried or producer catalogue no longer reports "recipe routes registered without a non-empty catalog".
- The boot summary adds a `producer recipes: N served` line beside the graph one.

### `extensions.embedder` — semantic search inside Cypher

Wire bge-m3 (or any fastembed-catalog model) so `text_score()` works inside `cypher_query`. Loading model code is explicit and trust-gated:

```yaml
trust:
  allow_embedder: true
extensions:
  embedder:
    library: sentence-transformers
    model: BAAI/bge-m3
```

A worked example is at {doc}`../examples/manifest_with_embedder`. The reference is under [`extensions:` schema reference](#extensions-schema-reference) below.

### `extensions.value_codecs` — convert literals in/out

A value codec maps the agent's natural input onto your stored types, and reads it back in the form the agent typed. It works for one declared property at a time. It is bound to a property and applied **after parsing**, never as raw-text substitution. It is therefore position-scoped and can't mangle unrelated literals. Three kinds exist:

- **`prefix`** — strip/add a fixed prefix (Wikidata `'Q42'↔42`, `gene:BRCA1`).
- **`map`** — a fixed bijective lookup table (enum `'active'↔1`).
- **`regex`** — full-match rewrite of the literal (date `'31.12.2020'→'2020-12-31'`).

Decode runs on query-side literals in the property's position. Encode runs on direct result-column projections of it. There is no trust gate: a codec is pure declarative data transformation. A worked example is at {doc}`../examples/manifest_value_codecs`. The reference is below.

> Replaces `extensions.cypher_preprocessor` (removed in 0.10.27). That hook
> rewrote raw query *text* before parsing, which could corrupt string
> literals / RETURN aliases. `value_codecs` does the conversion at a safe,
> post-parse, position-scoped site instead.

### Top-level fields

```yaml
name: My Graph                        # Server display name (optional)
instructions: |                       # Replaces default instructions (optional)
  Custom prompt shown to the agent at server-info time.
skills: true                          # Turn on the skill system (see below)
source_root: ./data                   # OR source_roots: [./data, ../alt]
trust:
  allow_embedder: true                # Required when extensions.embedder exists.
builtins:
  save_graph: false                   # Default false — gate write-back tool.
  temp_cleanup: on_overview           # Wipe temp/ on every bare graph_overview().
  github: false                       # Default false — opt in to github_issues /
                                      # github_api / screen_stargazers. A reachable
                                      # token alone never registers them.
  screen_stargazers: true             # Only meaningful when github: true.
extensions:                           # kglite-specific addons (see matrix below).
  embedder:
    library: sentence-transformers    # or fastembed (py) / fastembed-rs (cargo)
    model: BAAI/bge-m3
  csv_http_server:
    dir: temp/                        # port omitted -> OS-assigned. See below.
tools:
  - name: ...                         # See sections above
```

Anything else fails fast at load time, with the offending key listed.

### `skills:` — teach agents how to use the tools

`skills: true` turns on the **skill system**: bundled, graph-carried and operator-authored markdown. It attaches per-tool and cross-tool methodology (and TRIGGER/SKIP routing) to tool descriptions, gated per-graph.

Use skills instead of stuffing everything into `instructions:`. Skills re-surface in `tools/list`, attach to specific tools, and stay silent on graphs they don't fit. Drop files into a `<basename>.skills/` directory beside the manifest, or store them in the graph itself with `graph.set_skill(...)`.

Delivery is **lazy** by default. The routing rides the tool description, and the body is fetched with the `skill(name)` tool, which appears in `tools/list` whenever skills are on. The bundled `cypher_query` skill is the exception and ships eager, because its body shapes the first query written.

The layer order is `bundled < graph-carried < declared dirs <
<basename>.skills/`, one skill per name, higher wins.

- The bundled and graph-carried layers surface only when `skills:` contains `true`.
- A graph's skills are read in `--graph` and `--watch` modes only.
- They are re-read whenever `reload_graph` / `load_graph` / `create_graph` swaps the served graph.

The full authoring spec is its own guide: {doc}`mcp-skills`. It covers the frontmatter schema, `delivery`, `applies_when` gating, the graph-carried node shape, the three text channels, and size limits.

### Common boot errors

The manifest is validated before `mcp.run()` is called. Most configuration mistakes therefore surface as a one-line `ERROR:` to stderr at startup with a non-zero exit code. The recurring ones:

| Error message | What it means | Fix |
|---|---|---|
| `ERROR: <path>: unknown top-level keys: ['foo']` | Typo or unsupported key in manifest. | Compare against the [top-level field list](#top-level-fields). |
| `WARN … declared source_root does not resolve …` plus `source tools: unavailable (unresolved: "./data" → /abs/.../data)` — or `source tools: 2 root(s) serving, unresolved: …` — in the boot summary | The path is relative-to-yaml; it didn't land on a real directory. **The server still boots and serves its graph tools**; other declared roots keep serving, and only the named one is dropped. | Check the path; create the directory; or use `source_roots:` if you have multiple. Then restart. |
| `ERROR: --mcp-config path does not exist: <path>` | Explicit `--mcp-config` value points at a missing file. | Check the path. Sibling auto-detect is `<basename>_mcp.yaml`. |
| `Error: skill path "./pack" (resolved to /abs/.../pack) does not exist or is not a directory` | A `skills:` entry names a directory that isn't there. One bad entry fails the whole registry build, so tolerating it would serve the deployment with **every** skill gone — bundled ones included — while the graph tools answered normally. | Create the directory, or drop the entry. The auto-detected `<basename>.skills/` layer is separate and stays optional. |
| `data advisories: <code> (written by kglite X.Y.Z; see graph_overview)` in the boot summary, and a `<data-advisory>` line in `graph_overview` | The served `.kgl` was written by a build with a known data-shape bug **and** its data shows it (see {ref}`Files written by older versions <files-written-by-older-versions>`). The server serves the graph normally. | Rebuild the graph from its source with the current version. |
| `WARN graph-carried skill skipped` on stderr, plus `graph skills: … skipped: <name>: <rule>` in the boot summary | A `KgliteSkill` node in the served graph failed validation — usually written by a raw `CREATE` that bypassed `set_skill`. **Not a boot error**: graph content is data, one bad node must not take the rest down. Its siblings still load. | Fix the node (`set_skill` applies the same rules and refuses instead of storing), or delete it. |
| `WARN graph-carried recipe query skipped` on stderr, plus `graph recipes: … skipped: <recipe>/<name>: <rule>` in the boot summary | A `KgliteRecipe` node's Cypher does not compile or is not read-only. Same rule as above, and deliberately the *opposite* of the manifest catalogue's, which fails the boot: an operator typo is theirs to fix and they are looking at the file. | Fix or delete the node; `set_recipe` refuses the same content instead of storing it. |
| `ERROR: extensions.value_codecs ... is not bijective` | A `map` codec has two keys mapping to the same value, so encode is ambiguous. | Make the `map:` one-to-one. |
| `ERROR: value_codecs[i].match ... is not a valid regex` | A `regex` codec's `match` doesn't compile. | Fix the regex (anchor it for a full match). |

Every boot failure exits **1**, manifest and validation errors included.

- `2` is clap's own code for an unparseable command line (a misspelled flag).
- `0` is a clean exit.
- The wheel entry point returns `130` on Ctrl-C.

There is no separate code per failure class, so wrapping scripts branch on the message, not the code.

## End-to-end example: a conference catalog graph

This example is a graph indexing conference sessions, speakers, and companies, with embedding-derived similarity edges between sessions. The manifest co-locates with the graph file and the source data:

```
conference/
├── conference.kgl
├── conference_mcp.yaml          ← auto-detected
└── data/
    ├── sessions/
    │   └── classified.json
    └── speakers/
```

```yaml
# conference_mcp.yaml
name: Conference Graph
instructions: |
  Conference catalog — sessions, speakers, companies, plus
  similarity edges between sessions. Use cypher_query for
  structured questions, read_source/grep for raw JSON in ./data,
  similar_sessions for embedding-based recommendations,
  session_detail for the full session record by id.

source_root: ./data

tools:
  - name: similar_sessions
    description: Top-k semantically similar sessions for a session id.
    parameters:
      type: object
      properties:
        session_id: {type: string}
        top_k:      {type: integer, default: 5}
      required: [session_id]
    cypher: |
      MATCH (s:Session {id: $session_id})-[r:SIMILAR_TO]->(t:Session)
      RETURN t.id AS id, t.title AS title, r.score AS score
      ORDER BY score DESC LIMIT $top_k

  - name: session_detail
    description: Full record for a session by id.
    parameters:
      type: object
      properties:
        session_id: {type: string}
      required: [session_id]
    cypher: |
      MATCH (s:Session {id: $session_id})
      OPTIONAL MATCH (s)-[:PRESENTED_BY]->(speaker:Speaker)
      OPTIONAL MATCH (speaker)-[:WORKS_AT]->(company:Company)
      RETURN s, collect(DISTINCT speaker) AS speakers,
             collect(DISTINCT company) AS companies
```

Run with:

```bash
kglite-mcp-server --graph conference.kgl
```

Tools registered (visible in any MCP-aware agent):

- `graph_overview`, `cypher_query`, `ping` — core graph tools
- `read_code_source`, `explore` — code-graph-aware tools
- `read_source`, `grep`, `list_source` — from `source_root`
- `similar_sessions` — inline Cypher
- `session_detail` — inline Cypher

The exact list is mode-dependent. `save_graph` is registered in two cases:

- the manifest opts in with `builtins.save_graph: true`;
- the server is write-enabled (`--writable`, or `extensions.writable: true`).

Only the write-enabled spellings also open `cypher_query` to mutations and add the graph lifecycle tools.

To map the agent's input onto your stored types (Wikidata `'Q42'↔42`, enum codes, date formats), see {doc}`../examples/manifest_value_codecs`. For full Rust integration, see **Building a downstream binary** below.

## Building a downstream binary

When manifest Cypher templates aren't enough, embed the `kglite-mcp-server` library and add tools through `ServerExtensions`. This applies when domain logic needs to share the active graph, materialize files, or conditionally register tools.

KGLite still owns graph/Cypher/source tools, manifests, skills, watchers, and stdio. The downstream binary owns only its domain methods.

The shape:

```rust
use kglite_mcp_server::{run_with_extensions, ServerExtensions};

fn main() -> anyhow::Result<()> {
    let extensions = ServerExtensions::new().with_domain_tools(|registry| {
        let graph = registry.graph_state().clone();
        registry.register_typed_tool::<MyArgs, _>(
            "my_tool",
            "What the domain tool does.",
            move |args| graph.with_context(|context| {
                my_domain_logic(context.graph(), context.root(), args)
            }).unwrap_or_else(|| "no active graph".to_string()),
        )
    });
    run_with_extensions(std::env::args_os(), extensions)
}
```

`ServerExtensions` also carries a read-only pin and two methodology builders:

- `read_only()` pins the server read-only regardless of `--writable` or `extensions.writable: true`. It is the guarantee an embedder that owns argv but not the manifest cannot otherwise make.
- The two *methodology* builders are `with_skills` for the binary's own skill layer (see {doc}`mcp-skills`) and `with_recipes` for its own Cypher catalogue (see [Recipes registered by the producer](#recipes-registered-by-the-producer)).

Both builders apply to every graph the server serves, in every mode, and both lose to the operator. See {doc}`../../rust/building-on-kglite`.

The registry rejects names already owned by KGLite or manifest tools.

- Use `DomainGraphState::with_context` when the result needs both graph data and its identity. The borrowed graph, save target, and source root come from one active-slot snapshot. Keep that callback short and read-only.
- The registry also offers `register_route` for a custom rmcp `ToolRoute`, with the same collision check.

See the compiling [`domain_tools.rs`](https://github.com/kkollsga/kglite/blob/main/crates/kglite-mcp-server/examples/domain_tools.rs) example. If you need to replace KGLite tools or change its stdio transport, build a separate server directly on [`mcp-methods`](https://crates.io/crates/mcp-methods). That is a server fork, not domain-tool composition.

To decide between a manifest and a composed downstream binary:

| Need | Manifest | Downstream binary |
|---|---|---|
| Read-only tools (Cypher templates, source access) | ✅ | overkill |
| Executable Rust/domain logic | ❌ | ✅ |
| Tool registration conditional on active graph | ❌ | ✅ |
| Custom rmcp transports / middleware | ❌ | separate server |
| Replacing `cypher_query` / `graph_overview` | ❌ | ❌ |

Most projects never need a downstream binary.

## Built-in patterns

### `FORMAT CSV` export

When agents need full result sets (not just 15 rows), they append `FORMAT CSV` to the query. The Rust binary saves the result to a temp file and serves it over a localhost HTTP server with CORS. Agents can then generate HTML artifacts that `fetch()` the CSV at runtime, instead of hardcoding thousands of rows into the artifact source.

### Mode banner — tell the agent which conditional tools are registered

Whichever CLI mode the server is in (`--graph` / `--workspace` / `--watch` / `--source-root` / bare / local-workspace via manifest), the server prepends a per-mode **banner** to two surfaces:

- the `instructions` block returned during MCP `initialize` (read once at handshake);
- the bare `graph_overview()` response preamble (re-read on every call, so it survives context aging on long sessions).

The banner names every conditional tool, both the registered ones and the unregistered ones. The agent can see at a glance whether `repo_management`, `set_root_dir`, or `save_graph` are available, without trial-calling each one. Example for workspace mode:

```
[kglite-mode] workspace (clone-and-activate)
- repo_management: registered. Start with:
    repo_management()             — list known repos
    repo_management('org/repo')   — clone + activate
- cypher_query / graph_overview: registered (operate on the active repo's graph).
- save_graph / set_root_dir: not in this mode.
```

The `[kglite-mode]` marker identifies the segment for downstream tooling. Operator-declared `instructions:` / `overview_prefix:` text follows the banner unchanged.

### Multi-revision code graphs

Both activation paths take an optional `revs` argument. It builds the code graph across several git revisions instead of just the latest:

- **github mode:** `repo_management('org/repo', revs=5)` (last 5 release tags + `HEAD`) or `repo_management('org/repo', revs=['v1.0', 'v2.0', 'HEAD'])`.
- **local mode:** `set_root_dir('/path', revs=5)` or `set_root_dir('/path', revs=[…])`.

An integer means "the last N release tags (`git tag --sort=-v:refname`) plus `HEAD`". A list is passed through as explicit revspecs, oldest → newest. Omitting `revs` is the unchanged single-revision build.

The result is **one merged graph**, not N graphs.

- Each entity is stored once, carrying a `revs: [str]` list (the revisions it appears in) and a `rev_fp: [int]` shape fingerprint (positionally aligned with `revs`).
- Ordinary properties report the newest rev an entity appears in (newest-wins), so plain Cypher reads `HEAD`'s value.
- The active-graph header names the loaded set, `<active_graph … revs="v1.0,v2.0,HEAD"/>`, and the activation message teaches the scoping idiom below.

Because the graph spans every rev, an **unscoped** query counts the union across revs (an over-count trap). Scope to a single rev with list membership, and use `CALL rev_diff` for deltas:

```cypher
-- Everything present in v2.0 (scoped — no over-count)
MATCH (n:Function) WHERE 'v2.0' IN n.revs RETURN n.name

-- What changed between two revs (added / removed / changed)
CALL rev_diff({from: 'v1.0', to: 'HEAD'})
YIELD bucket, type, qualified_name, name, file, line
RETURN bucket, type, qualified_name, file, line
```

See the [Cypher reference](../../reference/cypher-reference.md) `rev_diff` entry and the codingest project for the full build semantics.

**Two operator caveats:**

- **Older clones may lack tags.** Pre-0.3.49 mcp-methods cloned depth-1/tag-less, so `revs=N` (tag-based) finds nothing to resolve. `repo_management('org/repo', update=True)` fetches tags, and a full re-clone restores complete history. Fresh clones under 0.3.49+ bring tags automatically.
- **Activation cost scales with rev count.** Each rev is a full parse, so a `revs=N` build costs ≈ N × a single build. On a large repo, start small (`revs=2`–`3`) before loading a deep history.

### Mutable graphs

`save_graph` is built in. In single-graph mode it registers automatically in two cases:

- the manifest sets `builtins.save_graph: true`;
- the server is write-enabled (`--writable`, or `extensions.writable: true`).

It writes the active graph back to the source `.kgl` path. When it is on, the mode banner above flips its `save_graph` line from "not registered (read-only)" to "registered. Call to persist Cypher mutations."

The two keys are not interchangeable. `builtins.save_graph: true` on its own registers the tool and nothing more. `cypher_query` still refuses every mutation, so what such a server has to persist is what it loaded, typically an ontology materialized from `extensions.ontology` at boot. Mutations need `--writable` or `extensions.writable: true`.

A save with nothing unsaved to write is a no-op. It reports `Nothing to save: <path> is clean and carries no unpersisted configuration…` and leaves the file untouched. Other servers bound to the same graph are therefore not made to re-read it for a rewrite of the same bytes.

- Unsaved mutations and a boot-applied manifest ontology both count as something to write.
- A save that carried only the ontology says so (`wrote manifest ontology (N classes, M managed labels); no data changes`).
- To rewrite a clean file deliberately (re-encoding it with the running library version, say), pass `force=true`.
- `force=true` needs the write opt-in. `force` re-encodes the file and moves its identity, so a server holding `builtins.save_graph: true` alone refuses it and says which setting enables it.

### Semantic search (`text_score()`)

`extensions.embedder` in the manifest registers an embedder so `text_score()` works inside Cypher. The agent can then write:

```cypher
MATCH (a:Article)
WHERE text_score(a, 'summary', 'renewable energy') > 0.4
RETURN a.title, text_score(a, 'summary', 'renewable energy') AS score
ORDER BY score DESC LIMIT 10
```

The full schema is in the [`extensions:` schema reference](#extensions-schema-reference) below. A worked example is at {doc}`../examples/manifest_with_embedder`. The embedder protocol itself is in [Semantic Search](semantic-search.md).

### Security

- **Read-only mode** rejects mutations at the Cypher level, and is the default.
  - A server is write-enabled only with `--writable` or `extensions.writable: true`.
  - `graph.read_only(True)` before binding enforces it at the graph itself.
  - `builtins.save_graph: false` (the default) is a separate switch. It decides whether `save_graph` is registered, not whether `cypher_query` accepts writes.
  - A `save_graph`-only server may still publish unsaved changes and boot configuration. `save_graph {force: true}` re-encodes the file and moves its identity, so it is refused without the write opt-in.
- **A Rust binary embedding the server can pin it.** `ServerExtensions::new().read_only()` makes both `--writable` and `extensions.writable: true` inert for the life of the process, and logs which spelling it overrode. Use it when the embedder owns argv but not the manifest. An operator editing the sibling manifest then cannot open writes against a graph the binary regenerates.
- **Path traversal** is blocked by the framework's source tools. The bundled `read_source` / `grep` / `list_source` canonicalise every path against the configured `source_root` before any I/O.

**Query parameters** — when passing user input to Cypher, use `params` to prevent injection:

```python
graph.cypher("MATCH (n) WHERE n.name = $name RETURN n", params={"name": user_input})
```

## Deployment shapes

### Small / medium graphs (`.kgl` file)

This is the Quick Start path. The graph fits in memory. You load it via `kglite.load(path)` or save it via `g.save(path)`, then point the CLI at the resulting `.kgl` file. It uses the default storage and needs no special config. It is suitable up to ~10M nodes on a developer laptop, and larger on a beefier host.

### Large graphs (disk-backed)

A disk-backed graph suits graphs that don't fit in memory or take too long to deserialise on every boot. Build it once and point the CLI at its directory:

```python
# One-off ingestion (e.g. from Wikidata's truthy.nt.bz2 dump):
import kglite

# Streams the dump straight into a disk-backed graph in
# `/data/wikidata-graph/`.
g = kglite.KnowledgeGraph(storage="disk", path="/data/wikidata-graph/")
g.load_ntriples("latest-truthy.nt.bz2", languages=["en"], verbose=True)
```

(The pre-packaged dataset loaders — SEC EDGAR, Sodir, Wikidata — live in the separate kglite-datasets project. They wrap this same `load_ntriples` path with download/cooldown/resume. kglite loads the graphs they produce.)

Then run the MCP server against the directory (not a single file):

```bash
kglite-mcp-server --graph /data/wikidata-graph/
```

The CLI's `--graph` validator accepts both shapes: a `.kgl` file OR a directory containing `disk_graph_meta.json` (the disk-graph sentinel). For your own data, the API is `kglite.KnowledgeGraph(storage="disk", path="/data/graph/")` for the constructor and `g.add_nodes(...) / g.add_relationships(...)` for population.

Manifests work the same way for both shapes. For example, `wikidata_mcp.yaml` sits next to the graph dir (or is pointed at via `--mcp-config`):

```yaml
name: Wikidata
extensions:
  value_codecs:
    - property: id          # integer-keyed column
      kind: prefix
      prefix: "Q"           # decode 'Q42' → 42 ; encode 42 → 'Q42'
      stored_type: int
```

See {doc}`../examples/manifest_value_codecs` for the full Wikidata example (Q-number ↔ integer, plus the `map` and `regex` kinds).

## Troubleshooting

Common post-boot pitfalls, grouped by symptom.

### `github_issues` says "could not auto-detect from git remote"

Either the manifest is missing `builtins.github: true` (the default; an ambient token alone registers nothing), or `GITHUB_TOKEN` (or `GH_TOKEN`) isn't in the server's environment. The token is loaded from:

1. The process environment when the server boots.
2. The manifest's `env_file:` path (explicit).
3. A `.env` file discovered by walking up from the active mode's
   directory.

The `.env` file never overwrites existing process env. To verify, look for `loaded env file: <path>` on the server's stderr. The server logs it when it finds a `.env`. Absence of that line means no `.env` was discovered, and process env is what's in effect.

### `text_score()` returns 0.0 for every node

The embedder isn't bound. Causes, in order of likelihood:

- The manifest didn't declare `extensions.embedder`. See [`extensions:` schema reference](#extensions-schema-reference).
- The model couldn't download (network issue) or load (out-of-memory). Look for tracebacks in the server's stderr at boot.
- The property being scored doesn't exist on the matched nodes. `text_score(n, 'summary', 'query')` returns 0.0 when `n.summary` is null. Use `WHERE n.summary IS NOT NULL` to filter first.

### Warm `text_score()` is slow (seconds, not milliseconds)

bge-m3's cool-down may have released the ONNX session. The default `cooldown` is 900 seconds (15 min). Two fixes:

- Set `extensions.embedder.cooldown: 0` in the manifest to keep the session resident forever (heavy-use mode).
- Pick a larger value matching your usage pattern.

See {doc}`../examples/manifest_with_embedder` for the tradeoff table.

### Conda environment lifts an old `kglite-mcp-server`

If `which kglite-mcp-server` resolves outside your active env, your shell PATH is finding an older install (typically from a prior `cargo install` or a different conda env). Drop the old install (`rm $(which kglite-mcp-server)` from outside the active env), or activate the right env explicitly.

### Server boots but `tools/list` shows fewer tools than expected

The [tool gating matrix](#tool-gating) shows the conditions each tool needs to register. The most common cases:

- `repo_management` missing — repository cloning is a `codingest-mcp` workspace feature. `set_root_dir` requires `workspace.kind: local`.
- `read_source` / `grep` / `list_source` answering "no active source root" — they are always listed, but no root is bound. Either of two causes:
  - no `source_root:` in the manifest, no `--source-root` CLI flag, and `--graph` parent auto-bind didn't fire;
  - *every* declared `source_root:` failed to resolve (check the boot summary for `source tools: unavailable`).
- `grep` / `read_source` succeeding but missing files you expect — with several `source_roots:`, the ones that resolved serve and the rest are dropped, with no per-call warning. The boot summary (`source tools: N root(s) serving, unresolved: …`) and the agent's `instructions` name the dropped roots.
- `github_issues` / `github_api` missing — the manifest doesn't set `builtins.github: true` (the default), or there is no `GITHUB_TOKEN` in env.
- `save_graph` missing — you're not in `--graph` mode, OR the manifest sets neither `builtins.save_graph: true` nor `extensions.writable: true` and the server was not started with `--writable`.
- `load_graph` / `create_graph` / `save_graph_as` missing, or `cypher_query` refusing a `CREATE` — the server is not write-enabled.
  - Those need `--writable` or `extensions.writable: true`. `builtins.save_graph: true` alone registers `save_graph` and nothing else.
  - Check the boot log for an unknown-`extensions:`-key warning first. A misspelled `writable` leaves a silently read-only server.

### PyPI says "No matching distribution found" immediately after a release

PyPI's `simple/` index lags the JSON metadata by ~few minutes after publish. Workaround:

```bash
pip install --index-url https://pypi.org/simple/ --no-cache-dir 'kglite==X.Y.Z'
```

Or wait a few minutes. This is a PyPI mirror-cache behaviour, not a kglite packaging issue.

## Reference

This section is the full programmable surface of `kglite-mcp-server`. Its stance is "what's documented enough that an agent or operator can rely on it?": anything in this section is treated as a contract.

### Mode × YAML-field acceptance matrix

The matrix shows which manifest key takes effect in which CLI mode. "—" means the key parses cleanly but has no behavioural effect in that mode, so the same YAML can move between modes without edits. The graph file is the discriminator for `--graph` / `--workspace` / `--watch` / `--source-root` / bare:

| Manifest key | `--graph` | `--workspace` | `--watch` | `--source-root` | bare (no graph) |
|---|---|---|---|---|---|
| `name`, `instructions`, `overview_prefix` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `source_root` / `source_roots` | ✓ (overrides parent-of-`.kgl`; roots are resolved per entry — the ones that exist serve, and a declaration that resolves to nothing still does not fall back to the parent) | — | — | ✓ (canonical) | ✓ |
| `env_file` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `workspace.kind: local` + `workspace.root: <dir>` | — | — | — | — | promotes into local-workspace mode |
| `workspace.watch: true` | — | — | ✓ (auto-rebuild) | — | ✓ when `workspace.kind: local` |
| `workspace.sandbox_root: <dir>` | — | — | — | — | ✓ bounds `set_root_dir`. **Opt-in; without it swaps are unbounded.** kglite 0.15.5+ / mcp-methods 0.4.3+. Requires `workspace.kind: local`. |
| `workspace.adopt_client_roots: true` | refused at boot | refused at boot | refused at boot | refused at boot | refused at boot. Local-workspace mode always binds the explicit `workspace.root`, so a client-advertised root would never be adopted. Set `workspace.root` and switch roots with `set_root_dir`. See the note below. |
| `tools[].cypher` | ✓ | ✓ (per active repo) | ✓ | — (no graph) | — |
| `trust.allow_embedder` | parsed, required by `extensions.embedder` | parsed, required by matching extension | parsed, required by matching extension | parsed (no graph) | parsed (no graph) |
| `builtins.save_graph: true` | ✓ (registers `save_graph` only; `cypher_query` stays read-only) | — (multiple graphs) | — | — | — |
| `extensions.writable: true` | ✓ (write-enables: mutation + `save_graph` + lifecycle tools; same as `--writable`) | — (multiple graphs) | — | — | — |
| `builtins.temp_cleanup: on_overview` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `extensions.embedder` | ✓ | ✓ (per active repo) | ✓ | — (no graph) | — |
| `extensions.cypher_recipes` | ✓ (merged with the graph's own `KgliteRecipe` records) | ✓ (per active repo) | ✓ (merged, as `--graph`) | registers discovery/run tools, but execution needs an active graph | registers discovery/run tools, but execution needs an active graph |
| `extensions.csv_http_server` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `extensions.value_codecs` | ✓ | ✓ | ✓ | — (no graph) | — |
| `extensions.<other>` (passthrough) | parsed, opaque to framework | parsed, opaque | parsed, opaque | parsed, opaque | parsed, opaque |

> **kglite-mcp-server refuses `adopt_client_roots`.** The key is an
> mcp-methods fallback: it adopts a client-advertised root only when no root
> is configured. This server's local-workspace mode always binds the explicit
> `workspace.root`, so the fallback can never fire. The server exits at boot
> with a message naming the key, rooted or rootless.
>
> The mechanism behind it is also deprecated. MCP `roots` was deprecated in
> protocol revision `2026-07-28` (SEP-2577). The spec's migration path is to
> pass directories as tool parameters, resource URIs or server configuration.
> Here that means `workspace.root` plus `set_root_dir`.
>
> `workspace.sandbox_root` is **not** affected. It is a local containment
> boundary with no protocol dependency.

Unknown keys at the top level (or under `builtins:` / `workspace:` / `trust:` / `tools[]`) fail validation at boot. The process exits non-zero with an `ERROR: <path>: unknown ... keys: [...]` message.

Keys under `extensions:` are unvalidated *by the framework*, because they're the downstream-binary passthrough zone. kglite validates the ones it reads. It warns at boot about any it does not, naming the key and listing the known set (`cypher_recipes`, `value_codecs`, `ontology`, `graph_watch`, `parallel`, `tools_allow`, `write_scope`, `csv_http_server`, `embedder`, `writable`). It is a warning, not an error, because a skill's `applies_when: {extension_enabled: …}` predicate may legitimately name a key no reader knows.

### Tool gating

This table shows which tool registers, under what conditions. `tools/list` only ever shows what's registered, so it also answers "what set of tools will my agent see?"

| Tool | Registered when | Notes |
|---|---|---|
| `cypher_query` | always | Returns inline rows or CSV URL — see "Tool response formats". Accepts mutations only on a write-enabled server (`--writable` or `extensions.writable: true`); otherwise they are refused naming both spellings. |
| `graph_overview` | always | Always available even with no graph: returns the no-graph message. |
| `ping` | always | Liveness probe. |
| `read_code_source` | always | Requires an active graph at call time (returns the no-graph message otherwise). |
| `save_graph` | `--graph` mode AND (`builtins.save_graph: true` OR write-enabled) | Other modes have no single graph to save back to. `builtins.save_graph: true` alone registers just this tool — it does not make `cypher_query` writable. A save with nothing unsaved is a no-op unless `force=true`, and `force` itself needs the write opt-in. |
| `load_graph` / `create_graph` / `save_graph_as` | write-enabled: `--writable` OR `extensions.writable: true` | The graph-lifecycle tools. `builtins.save_graph: true` does not register them. |
| `read_source` / `grep` / `list_source` | always | All three register together. They *serve* only with a bound root (`--source-root`, `--graph` parent auto-bind, a manifest `source_root:` that resolves, or an active workspace repo); otherwise each call answers "no active source root". |
| `repo_management` | `codingest-mcp --workspace` clone-tracker mode | Not registered in local-workspace mode; use `set_root_dir` there. |
| `set_root_dir` | `workspace.kind: local` only | **Unbounded unless `workspace.sandbox_root` is set** (kglite 0.15.5+, mcp-methods 0.4.3+). Without that key a swap may point the server at any readable directory; `workspace.root` is the *starting* root, not a boundary. |
| `github_issues` / `github_api` | `builtins.github: true` in the manifest **and** `GITHUB_TOKEN` (or `GH_TOKEN`) reachable at boot | Opt-in is required as of mcp-methods 0.4.5 — an ambient token no longer registers anything on its own. Token loaded from process env, walk-up `.env`, or explicit `env_file:`. Tools are registered together; never one without the other. |
| Manifest `tools[].cypher` entries | the manifest declares them AND the mode supports cypher (anything but `--source-root` and bare) | Tool names cannot collide with the built-ins above. |
| `list_recipe_queries` / `run_recipe_query` | the **merged** catalog is non-empty — `extensions.cypher_recipes`, the served graph's own `KgliteRecipe` records (`--graph` / `--watch` only), or both | Both fixed names register together. Listing works without a graph; running requires an active, fresh graph. |

### Tool response formats

Bundled-tool response shapes are treated as version-stable contracts across patch releases. They're tagged below per stability. Manifest `tools[].cypher` responses inherit `cypher_query`'s format.

| Tool | Response shape | Stability |
|---|---|---|
| `ping` | `<message>` (default `pong`) | Stable. |
| `cypher_query` (inline) | `<N> row(s)[ (showing first 15)]:\n<TAB-joined column names>\n<TAB-joined repr'd values per row>\n` | Stable post-0.9.22 (the 0.9.21 row-formatter regression is the canonical "this is now a contract" event). |
| `cypher_query FORMAT CSV` with `csv_http_server` | `FORMAT CSV: <N> row(s) written to <url>\nFetch with: curl <url>`; `<N>` counts logical RFC CSV records, excluding the header. | Stable. |
| `cypher_query FORMAT CSV` without `csv_http_server` | Inline CSV body, capped at 200 complete logical RFC CSV records. Quoted fields containing CR or LF stay intact; a capped response names the full record count and byte size. | Stable. |
| `cypher_query` errors | `Cypher error: <engine message>` | Stable. |
| `list_recipe_queries` / `run_recipe_query` | Structured JSON success/error envelope in MCP `structuredContent`; text fallback is the same serialized JSON. | Stable v1 contract. |
| `graph_overview` | XML schema (see `describe()` output) — types / connections / cypher panes depending on args. | Stable; the XML shape is the canonical agent-facing format. |
| `read_source` | First line: `<path>  (lines X-Y of Z)`, body lines: `   <lineno>: <text>`. Truncation footer when `max_chars` trips: `... (truncated)`. | Stable. |
| `read_source` (path errors) | `Error: path '<path>' does not exist or access denied.` | Stable. |
| `grep` | `<path>:<line>:<text>` for matches, `<path>-<line>-<text>` for context lines. | Stable. |
| `list_source` | Tree-formatted directory listing relative to the primary source root. | Stable. |
| `read_code_source` | First line: `// <qualified_name> (<path>:<start>-<end>)`, body lines: `   <lineno>: <text>`. | Stable. |
| `save_graph` | `Saved <path> (<N> nodes, <M> edges).` (or `Saved <path>.` when schema unavailable). | Stable. |
| `save_graph` (no graph) | `save_graph requires --graph mode (no source path bound).` | Stable. |
| `repo_management` (list) | `<N> live repo(s):\n  <repo>[ [active]]  (<count> access[es], last <when>)` | Stable. |
| `repo_management` (activate) | `Cloned 'org/repo' at <path>.` / `Updated 'org/repo' at <path>.` / `Activated (already up to date) 'org/repo' at <path>.` | Stable. |
| `set_root_dir` (success) | `Active root set to <absolute_path>.` | Stable. |
| `set_root_dir` (outside `workspace.sandbox_root`) | An error naming `sandbox_root` and the boundary path; the active root does not change. | Stable. |
| `github_issues` (FETCH) | Issue/PR/discussion body with `cb_N` / `patch_N` / `comment_N` / `review_N` placeholders for collapsed elements. Drill down with `element_id=<placeholder>`. | Stable. |
| `github_issues` (LIST/SEARCH) | `<N> discussions in org/repo (<state>):` then per-line summary. | Stable. |
| `github_api` | Pretty-printed JSON body, truncated to `truncate_at` chars (default 80 000). | Stable. |
| (any tool, no active graph) | `No active graph. Pass --graph X.kgl, or activate one via repo_management('org/repo').` | Stable. |

If a future release needs to change a stable shape, that's a breaking change. It is tracked in the `CHANGELOG.md` "Changed" section (not "Fixed"), and the version bumps minor, not patch.

### `extensions:` schema reference

The `extensions:` block is the kglite-specific addon namespace. The keys validated below are first-class: they have parser-level validation, default values, and contracts. Anything else under `extensions.*` is opaque passthrough.

`extensions.graph_watch` is **retired**. A `--graph` server now re-reads a changed `.kgl` automatically on the next tool call, so the key arms nothing. It is still parsed, and a non-boolean value still fails boot. Any boolean value is accepted with a retirement warning at boot.

Machine-readable JSON Schema (Draft 2020-12) for each first-class block lives under [`docs/schemas/extensions/`][schemas-dir] in the repo:

- [`csv_http_server.json`][schema-csv]
- [`cypher_recipes.json`][schema-cypher-recipes]
- [`embedder.json`][schema-embedder]
- [`recipe_catalog.json`][schema-recipe-catalog]
- [`recipe_tools.json`][schema-recipe-tools]
- [`valid_time.json`][schema-valid-time]
- [`value_codecs.json`][schema-value-codecs]

The schemas are anchored to the Python parsers by the regression test `tests/test_extensions_schemas.py`. Any drift between "what the parser accepts" and "what the schema accepts" surfaces as a test failure on the next CI run.

[schemas-dir]: https://github.com/kkollsga/kglite/tree/main/docs/schemas/extensions
[schema-csv]: https://github.com/kkollsga/kglite/blob/main/docs/schemas/extensions/csv_http_server.json
[schema-cypher-recipes]: https://github.com/kkollsga/kglite/blob/main/docs/schemas/extensions/cypher_recipes.json
[schema-embedder]: https://github.com/kkollsga/kglite/blob/main/docs/schemas/extensions/embedder.json
[schema-recipe-catalog]: https://github.com/kkollsga/kglite/blob/main/docs/schemas/extensions/recipe_catalog.json
[schema-recipe-tools]: https://github.com/kkollsga/kglite/blob/main/docs/schemas/extensions/recipe_tools.json
[schema-valid-time]: https://github.com/kkollsga/kglite/blob/main/docs/schemas/extensions/valid_time.json
[schema-value-codecs]: https://github.com/kkollsga/kglite/blob/main/docs/schemas/extensions/value_codecs.json

#### `extensions.cypher_recipes`

This key is a mapping from recipe identifier to `{description, queries}`. Each query is a mapping with exactly `description`, `parameters`, and `cypher`.

- Identifiers match `^[A-Za-z_][A-Za-z0-9_]*$`.
- Descriptions and Cypher must contain a non-whitespace character.
- Every recipe has at least one query.
- Both recipe tools are disabled only when the catalog is empty *after* the served graph's own records are merged under it.

`parameters` uses the strict closed root schema described in the grouped, structured read-query section above. Catalog validation happens at server boot, and any violation exits before MCP serving starts. The schema file is [`docs/schemas/extensions/cypher_recipes.json`][schema-cypher-recipes].

#### `extensions.recipe_catalog`

These are budgets for the catalogue block `run_recipe_query` publishes inside its `tools/list` description. The key is a sibling of `extensions.cypher_recipes`, not a member of it, because every key *there* is a recipe name.

```yaml
extensions:
  recipe_catalog:
    block_budget: 16000       # bytes of rendered block; default 16000
    description_budget: 600   # characters per query description; default 600
```

Both keys are optional, and an omitted one keeps its default. The budgets buy these things, in order:

- **Whole while it fits.** Under `block_budget` the block carries every query's `recipe.query` name, its full description and its parameter line (types, enums, defaults, required flags).
- **Prose gives way first.** Past the ceiling, descriptions are shortened largest-first. They are never cut below `description_budget`, and only as far as the block needs. Each one that was cut gets a trailing `…`.
  - Names and parameter lines are never dropped for prose. The schema is what saves the agent a `list_recipe_queries` round trip.
  - `description_budget: 0` keeps names and parameters and drops the prose entirely.
- **Names alone are the last resort.** They are reached only when names and parameters *together* already exceed `block_budget`. That means a catalogue of hundreds of queries, not of seven.
- `list_recipe_queries`' own description states which of the three forms was rendered, so the two tools cannot contradict each other.

The skill bodies the framework appends **after** this block are charged to the skill registry's session budget, not to `block_budget`. They are the bundled `recipe_queries` methodology and any graph-carried skill that references these tools. Set `skills: false` to drop them; it does not change this block. The schema file is [`docs/schemas/extensions/recipe_catalog.json`][schema-recipe-catalog].

#### `extensions.recipe_tools`

```yaml
extensions:
  recipe_tools: false   # default: true
```

A recipe query may declare `tool: <name>`. It can do so in the manifest's own catalogue, in a `.kgl`'s `KgliteRecipe` records (`set_recipe(..., tool=...)`) or in a vault's `.kglite/recipes/*.md` frontmatter. Every such query is registered as an MCP tool of that name, in addition to the fixed pair:

- **Description** = the query's description.
- **Input schema** = the query's `parameters`, so the arguments *are* the variables. It adds an optional `valid_at` that runs the query as of an instant, unless the query declares a `valid_at` parameter itself. That parameter then stays its variable.
- **Output schema and annotations** = `run_recipe_query`'s, so the rows and the error envelope are byte-for-byte the ones the fixed route returns. `include_cypher` is not reachable from a named tool; audit through `run_recipe_query`.
- **Names** match `^[A-Za-z_][A-Za-z0-9_-]{0,63}$`.
  - Two queries claiming one name is a catalogue error.
  - A name any already-registered route owns (a built-in, a manifest `tools:` entry, a domain tool, the fixed pair) refuses the boot and names the owner. Nothing is ever replaced.
- **Ordinary allowlist members.** An `extensions.tools_allow` that omits a named recipe tool simply drops it. The two fixed routes differ: a manifest catalogue cannot half-declare them.
- **Boot-time only**, like the rest of the catalogue. Adding or removing a `tool:` needs a restart, not a `reload_graph`.
- The catalogue block in `run_recipe_query`'s description marks these entries `→ tool: <name>`. The bundled `recipe_queries` skill tells an agent to prefer the direct call where the marker is present.

Set `recipe_tools: false` to serve the two fixed routes only. Author guidance: expose a curated few. Each named tool costs its description and schema in every `tools/list`, which every session pays for.

#### `extensions.embedder`

Registers an embedder so `text_score()` works inside Cypher.

```yaml
extensions:
  embedder:
    library: sentence-transformers  # the engine; host (Python/Rust) inferred from it
    model: BAAI/bge-m3              # required (passed to the library)
    # cooldown: 900                 # fastembed-rs only; seconds (default 900). 0 = never release.
```

| Field | Type | Default | Constraint |
|---|---|---|---|
| `library` | string | `fastembed` | `fastembed` / `sentence-transformers` (Python, wheel) · `fastembed-rs` (Rust, cargo). |
| `model` | string | (required) | Passed to the chosen library; must be in *its* catalog. |
| `factory` | string | — | `module:attr` returning an `EmbeddingModel` — any custom Python embedder. |
| `cooldown` | int | 900 | `fastembed-rs` only; `0` disables auto-release. |

The legacy `embedder:` block (top-level, 0.9.17 and earlier) is parsed by the framework but ignored. Use `extensions.embedder:` with an in-catalog model (the server's Rust fastembed backend, built via `--features fastembed`).

#### `extensions.csv_http_server`

This key spawns a localhost HTTP listener (loopback only) that serves CSV exports produced by `cypher_query ... FORMAT CSV`.

```yaml
extensions:
  csv_http_server:
    dir: temp/                      # optional; default temp/ (relative to manifest)
    cors_origin: "*"                # optional; default "*"
    # port:                         # optional; omit for an OS-assigned port
```

It also accepts shorthand:

```yaml
extensions:
  csv_http_server: true             # defaults — OS-assigned port, dir temp/
  # or
  csv_http_server: false            # explicitly disabled (same as absent)
```

| Field | Type | Default | Constraint |
|---|---|---|---|
| `port` | int | `0` (OS-assigned) | `0 ≤ port ≤ 65535`. |
| `dir` | string | `temp` | Path; resolved against the manifest's parent directory. |
| `cors_origin` | string | `"*"` | Sent in `Access-Control-Allow-Origin`. Use a specific origin for tighter security. |

**Leave `port` out unless something outside the server needs the number.** One machine commonly runs several MCP clients (Claude Desktop, two Claude Code frontends, Codex) against the *same* manifest by absolute path. A pinned port is claimed by whichever server boots first and refused to every other.

With `port` omitted the kernel hands each server a free port. The server reports what it got in its stderr boot summary (`csv_http: http://127.0.0.1:54321`) and builds every `FORMAT CSV` URL from it.

A listener that cannot start (pinned port already taken, `dir` not creatable) logs a warning, disables the CSV extension, and lets the server carry on serving. The boot summary then reads `csv_http: disabled (<reason>)`, and `FORMAT CSV` answers come back inline, naming the failure. A *malformed* `csv_http_server:` value is a manifest syntax error and still fails the boot.

Only GETs of flat filenames inside `dir` are served. There are no directory listings and no write surface from the HTTP layer. Writes only come from the Cypher executor via `FORMAT CSV`.

#### `extensions.value_codecs` (0.10.27+)

This key is a list of operator-declared literal codecs, each bound to a stored property. Query-side literals in that property's position are decoded before execution. Direct result-column projections of it are encoded back. Codecs are applied **after parsing** (never as raw-text substitution), for `cypher_query` and `tools[].cypher` only. They do not apply to `graph_overview`, `read_source`, etc.

```yaml
extensions:
  value_codecs:
    - property: id
      kind: prefix            # prefix | map | regex
      prefix: "Q"             # 'Q42' ↔ 42
      stored_type: int        # int (default) | float | str
    - property: status
      kind: map
      map: { active: 1, archived: 2 }    # must be bijective
    - property: event_date
      kind: regex
      match: '^(\d{2})\.(\d{2})\.(\d{4})$'
      decode: '$3-$2-$1'
      encode: { match: '^(\d{4})-(\d{2})-(\d{2})$', replace: '$3.$2.$1' }  # optional
```

| Field | Type | Default | Constraint |
|---|---|---|---|
| `property` | string | (required) | Stored column the codec governs. |
| `kind` | string | (required) | `prefix` \| `map` \| `regex`. |
| `prefix` | string | (required for `prefix`) | Stripped on decode, added on encode. |
| `stored_type` | string | `int` | `int` \| `float` \| `str` (for `prefix`). |
| `map` | mapping | (required for `map`) | string → value; must be bijective. |
| `match` / `decode` | string | (required for `regex`) | Full-match regex + replacement template. |
| `encode` | `{match, replace}` | none | Optional reverse for `regex`. |

There is no trust gate: a codec is pure declarative data transformation (no code execution). A malformed block (bad regex, non-bijective map) is a boot error.

#### `extensions.writable`

```yaml
extensions:
  writable: true
```

This key is a single boolean, and the manifest half of the write opt-in. `true` is the same statement `--writable` makes on the command line, and either alone write-enables the server. That means mutation through `cypher_query`, plus `save_graph` and the `load_graph` / `create_graph` / `save_graph_as` lifecycle tools. Absent means off.

A non-boolean value fails the boot rather than being ignored. The reasoning is the same as for `extensions.tools_allow`: an escalation key that silently fails open is worse than none.

It exists for the wrapper that owns the manifest but not the argv of the server it spawns. `builtins.save_graph: true` is not an alternative spelling. That key registers `save_graph` alone and leaves `cypher_query` read-only.

#### `extensions.valid_time`

```yaml
extensions:
  valid_time:
    default: today   # 'today' (built in), 'all', or a fixed YYYY-MM-DD day
```

This key sets the instant a statement, recipe tool or fluent step reads when it names none, on a graph with validity declarations.

- `--valid-time-default {today|all|YYYY-MM-DD}` overrides the manifest.
- The setting is runtime only. It is never written into a `.kgl` file, and every graph the server installs (boot, reload, activation) takes it.
- A per-call `valid_at` (a date, or `"all"` for every version) and a `FOR VALID_TIME` prefix still win over it.
- A malformed value fails the boot.

#### `extensions.<other>` (passthrough)

Any other key under `extensions:` parses cleanly and is preserved on the loaded `Manifest.extensions` dict. The framework does not validate inner shape. Downstream consumers (kglite-mcp-server, your own server binaries) read whatever they need from this map.

kglite's own server warns at boot about any `extensions:` key it does not read, listing the ones it does. A misspelling is therefore visible in the log instead of quietly doing nothing.

### `tools[].cypher` template reference

Manifest entries shaped like

```yaml
tools:
  - name: <identifier>
    description: <agent-visible explanation>
    parameters: <JSON Schema object>
    cypher: |
      <Cypher template with $param placeholders>
```

become first-class MCP tools. Behaviour:

**Name** — must match `^[a-zA-Z_][a-zA-Z0-9_]*$`. It cannot collide with built-in tool names (`cypher_query` / `graph_overview` etc.).

**`$param` substitution** — Cypher templates pass through to `graph.cypher(query, params=args)` unchanged. The kglite Cypher engine does typed parameter binding (no string interpolation). The JSON value of `args[$name]` becomes a typed value at the `MATCH (n {field: $name})` site, so injection is impossible by construction. The agent supplies values per the JSON Schema, and kglite binds them in-engine.

**JSON Schema publication** — `parameters:` is published unchanged as the tool's MCP input schema. Use a root `type: object` schema with `properties` and `required`, so MCP clients can describe and construct calls consistently. KGLite does not validate the schema at boot or enforce it at dispatch. Client behavior varies, and callers can send arguments without client-side validation.

**Parameter binding** — every supplied argument is passed to KGLite as a named Cypher parameter. Values are bound in the engine and never interpolated into query text. A missing `$param`, an incompatible value, or another query problem surfaces through the normal Cypher error response.

**Tool errors** — if `graph.cypher()` raises, the response body is `Cypher error: <engine message>` (the same envelope as `cypher_query`). Empty result sets render as `No results.`.

**`FORMAT CSV` inheritance** — manifest cypher tools share the formatting path with `cypher_query`. Append `FORMAT CSV` inside the template (or `$_csv_format` if you want to gate it on a parameter). The tool's output then follows the same inline-vs-URL behaviour documented under "Tool response formats."

**Boot-time behavior** — the legacy manifest-tool path does not parse the template, compare `$param` names with `parameters.properties`, or validate the JSON Schema. Those problems are observed only if a client validates the published schema, or when the template executes.

Worked examples: see the `docs/python/examples/manifest_*.md` pages (`manifest_cypher_tool`, `manifest_value_codecs`, `manifest_with_embedder`, `manifest_workspace`).

### Embedder `library` × model catalog

The valid `model:` values depend on the `library:` you pick. The catalogs are **not** shared:

| `library:` | Catalog | `bge-m3`? |
|---|---|---|
| `sentence-transformers` (pip) | any HuggingFace embedding model | ✅ |
| `fastembed` (pip, fastembed-py) | `bge-*-en-v1.5`, `bge-small-zh-v1.5`, `multilingual-e5-large`, `all-MiniLM-L6-v2`, … (`TextEmbedding.list_supported_models()`) | ❌ |
| `fastembed-rs` (cargo) | `bge-m3`, `bge-{small,base,large}-en-v1.5`, `multilingual-e5-{large,base}`, `all-MiniLM-L6-v2` | ✅ |
| `factory: mod:attr` | whatever your builder loads | — |

**`bge-m3` is in fastembed-rs and sentence-transformers, but not fastembed-py.** If you set `library: fastembed, model: BAAI/bge-m3`, the server fails to boot (fastembed-py rejects the unknown model). The runtime model must also match the one the graph was embedded with, or `text_score()` rankings are meaningless.

fastembed (both ports) caches ONNX weights at `~/.cache/fastembed/`. sentence-transformers uses the HuggingFace cache. The first call downloads.

Adding support for a model outside the curated Python libraries doesn't need a kglite change. Use `factory: module:attr` pointing at your own builder.

### Path resolution and manifest discovery

**Relative paths in manifests resolve against the manifest's own directory.** This applies to `source_root`, `env_file`, every entry in `source_roots`, `workspace.root`, and `extensions.csv_http_server.dir`. The rule is unconditional: no path is interpreted relative to `cwd` unless explicitly absolute.

Manifest discovery order:

1. `--mcp-config <path>` — explicit path; absolute or
   resolved against cwd.
2. `--graph X.kgl` — auto-detects `<dirname>/<basename>_mcp.yaml`
   next to the graph file (the "sibling" pattern).
3. `--workspace DIR` / `--watch DIR` — auto-detects
   `DIR/workspace_mcp.yaml`.
4. `--source-root` / bare — no auto-detection. Pass `--mcp-config`
   explicitly if you want a manifest.

`.env` discovery order:

1. Manifest `env_file: <path>` — explicit; absolute or relative to
   manifest dir.
2. Otherwise walks upward from the mode path (or cwd in bare mode)
   looking for a `.env` file. Loads the first one found.

Existing process env vars are never overwritten by `.env`. `GITHUB_TOKEN=...` in your shell wins over the file.

### Operator notes

#### PyPI simple-index lag after publish

After a `kglite` release publishes to PyPI, the `simple/` index that `pip install` consults can lag the JSON metadata by ~few minutes. The first `pip install kglite==X.Y.Z` after publish may return `No matching distribution found`. Workaround:

```bash
pip install --index-url https://pypi.org/simple/ --no-cache-dir 'kglite==X.Y.Z'
```

The `--index-url` forces a direct fetch (some mirrors cache longer). `--no-cache-dir` bypasses pip's local cache. If you'd rather not pass flags, wait a few minutes; the lag is consistent.

This is a PyPI / mirror-cache behaviour, not a kglite packaging problem.

#### Conda + multiple Pythons

`pip install kglite` against a conda env's Python (`conda activate myenv && pip install kglite`) Just Works, with no `PYO3_PYTHON=` and no `install_name_tool` patching. It also installs the `kglite-mcp-server` command into that env (the bundled Rust server).

If you *also* ran `cargo install kglite-mcp-server`, both land on PATH. `which kglite-mcp-server` confirms which install you're running. They run the same server, so it rarely matters.

#### Watch mode rebuild costs

`workspace.watch: true` + `--watch DIR` rebuilds the code graph under `codingest-mcp` on every debounced file change (500 ms default debounce). The generic `kglite-mcp-server` intentionally has no builder.

- For source trees over 100k LoC this costs a few seconds per rebuild.
- The rebuild runs on a background thread. Queries against the previous graph keep working until the new graph atomically swaps in.

## Migrations

Pre-0.9.20 operators upgrading from a bundled-binary install: see {doc}`../migrations/mcp-pre-0.9.20`. It holds the migration notes for 0.9.17→0.9.18 (Python embedder + tools[].python removal, csv_http_server introduction) and 0.9.19→0.9.20 (bundled-binary → Python entry point).

## Worked examples

These are end-to-end manifest snippets, each focused on one feature:

```{toctree}
:maxdepth: 1

../examples/manifest_cypher_tool
../examples/manifest_with_embedder
../examples/manifest_workspace
../examples/manifest_value_codecs
```
