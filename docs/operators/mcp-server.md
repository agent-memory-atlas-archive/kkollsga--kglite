# MCP server

`kglite-mcp-server` exposes a KGLite graph over MCP stdio. The same Rust server
is available from `cargo install kglite-mcp-server` and inside the `kglite`
Python wheel.

Large tool results use discoverable byte budgets. They retain complete
structured evidence for targeted expansion. See
[Bounded agent responses](agent-responses.md) for:

- the response controls;
- collision-safe action composition;
- the lifecycle;
- the distinction between presentation truncation and query limits.

```bash
kglite-mcp-server --graph /data/graph.kgl
kglite-mcp-server --selftest --graph /data/graph.kgl
```

The default server is read-only and registers `ping`, `graph_overview`, and
`cypher_query`. A manifest can add:

- source-root tools;
- parameterized Cypher;
- skills;
- value codecs;
- an embedder;
- CSV-over-localhost export.

A served `.kgl` can also carry its own skills and recipe queries. An opted-in
server merges them with the manifest's. A binary that *embeds* this server can
register a third set of its own (`ServerExtensions::with_skills` /
`with_recipes`) that applies to every graph it serves. The manifest outranks
both. See the [MCP servers guide](../python/guides/mcp-servers.md) and
[Authoring MCP skills](../python/guides/mcp-skills.md).

Point MCP clients at the absolute executable path. This avoids an older
installation that shadows the new one on `PATH`.

## Pinning the tool surface

`extensions.tools_allow` names the tools a deployment exposes and hides
everything else.

Without it, a server exposes the union of everything that registered:

- framework builtins;
- the source tools the mode binds;
- KGLite's graph tools;
- manifest Cypher tools;
- routes that appear from a dependency or a mode change without the manifest
  ever naming them.

One long-standing case of this was closed upstream in mcp-methods 0.4.5. An
ambient `GITHUB_TOKEN` exported for unrelated reasons used to add the GitHub
tools to a server whose manifest never mentions GitHub. They now register only
when the manifest opts in with `builtins.github: true`. That removes one route
at the source; the allowlist bounds the rest.

```yaml
# /data/graph_mcp.yaml
name: My Graph
extensions:
  tools_allow:
    - cypher_query
    - graph_overview
    - ping
```

That server lists exactly those three tools in every environment. A route
arriving later cannot widen the surface without an edit to the list. Such a
route can come from a new dependency, an exported credential, or a mode
change. Hidden tools are unlisted and rejected when called by name.

Details worth knowing before writing an allowlist:

- **Names are the final, agent-visible ones.** If a `tools:` override renames
  `ping` to `domain_ping`, the allowlist must say `domain_ping`.
- **Naming a tool that is not registered in this boot is harmless.** You can
  list conditional routes safely, so one manifest works across environments.
  Conditional routes include:
  - `github_api` without the `builtins.github` opt-in or without a token;
  - `load_graph` on a read-only server;
  - `explore` on a non-code graph.
- **It only removes.** Listing a tool that another rule hid does not bring it
  back. Examples are `repo_management` in a local workspace and a
  `hidden: true` override.
- **The list is the whole surface**, not an addition to a default set. If you
  omit `ping`, the server has no `ping`. An explicit `tools_allow: []` is taken
  literally and leaves no tools at all.
- A manifest that configures `extensions.cypher_recipes` must list
  `list_recipe_queries` and `run_recipe_query`. Omitting them is refused at
  boot rather than serving a catalog no agent can reach.
- **The `skill` loader is exempt.** With skills on, the framework registers
  `skill(name)` after the allowlist has been applied, so it is served whether
  or not the list names it. That is deliberate. It is a read-only fetch of
  methodology the deployment already chose to serve. Hiding it would leave
  every lazy skill's `skill("<name>")` pointer aimed at a tool the agent cannot
  call. Turn skills off if you do not want it.
- A malformed value fails boot instead of being ignored. A value is malformed
  if it is not a list, or if an element is not a string. An allowlist that
  silently fails open is worse than none.

## Optional parameters (`default:`)

A parameter an agent may omit declares a JSON-Schema `default:`. The server
binds it before the query runs. This applies to manifest `tools[].cypher`
entries and to `extensions.cypher_recipes` queries alike:

```yaml
tools:
  - name: search_docs
    parameters:
      type: object
      properties:
        query: {type: string}
        limit: {type: integer, default: 5}
      required: [query]
    cypher: MATCH (d:Doc) WHERE d.title CONTAINS $query RETURN d LIMIT $limit
```

Only an *absent* argument is filled. An explicit value wins, and an explicit
`null` stays null.

Without a default, an omitted parameter reaches the engine unbound. The call
then fails with `Missing parameter: $limit`. For that reason `coalesce($limit,
5)` in the query does not help. A parameter with no default is effectively
required, however the schema describes it.

### Defaults in a recipe's closed schema

In a recipe's closed schema, the `default` and `required` halves must agree:

- A property with a `default` is the one thing `required` may leave out.
  Listing it as required anyway is refused at boot.
- When every property has a default, `required` itself may be left out. Absent
  means none, as in JSON Schema.
- A default must satisfy its own property: its type, `enum`, and
  `minimum`/`maximum`. Otherwise the catalogue fails to compile. A wrong
  default is therefore an operator's boot failure rather than an agent's
  call-time error.

Manifest `tools:` schemas are published as written and never compiled, so their
defaults are bound as-is.

## Query deadlines

**Every query this server runs has a 180,000 ms (three-minute) deadline.** It
is the same default the Python API applies, shared as one constant in the
engine.

The deadline is a liveness property, not a preference. An agent has no cancel
channel once a tool call is in flight. A runaway read holds the active graph's
read lock, which stalls the single-flight rebuild gate that every later tool
call enters. One bad query would otherwise take the whole server with it.

`cypher_query` takes an optional `timeout_ms` argument that overrides the
deadline for one call. `timeout_ms: 0` runs without a deadline. It is the
escape hatch for the long analytical query the default exists to bound; use it
deliberately rather than by accident. The deadline applies to every route that
reaches the engine: the built-in tool, manifest `tools[].cypher` templates, and
recipe queries.

`row_limit` is deliberately **not** exposed here. In Python it caps the rows a
call *retains*. This server already bounds its own output, with a 15-row inline
preview and a cap on served CSV. A `row_limit` on top of that would silently
truncate a `FORMAT CSV` export, which is the opposite of what that route
guarantees. Write `LIMIT n` in the query.

## Refreshing a rebuilt graph

A `--graph` server serving a regular `.kgl` file re-reads it by itself. There
is nothing to configure and no manifest key.

Every graph tool call first `stat`s the served path. The server re-reads the
file through the normal open path before answering when the file's identity
differs from the one the in-memory graph was loaded or last saved from. The
identity is the length, mtime, and device/inode. An agent can rely on the
strong property: a clean server never answers from, and never writes onto, a
snapshot older than the file was at the time of the call.

`reload_graph` is still registered in `--graph` mode, read-only servers
included. It forces the re-read instead of waiting for the next call, and it
reports the new node/edge counts and this server's load count (`Load N on this
server.`). It is the refresh path for the cases the automatic one declines. A
failed re-read keeps the current graph serving and returns the error.

What the automatic refresh costs, and where it stops:

- **Every save by another process costs each other server one full re-read** on
  its next tool call. That is seconds on a ~100 MB graph, paid inside whichever
  tool call happens to be first, with concurrent calls waiting behind it. The
  re-read is single-flight and lazy. Calls that all saw the change queue
  behind one load rather than starting several. A producer that writes ten
  times between two queries costs one re-read, not ten.
- **The `stat` runs on the calling thread.** A `.kgl` on a hung network volume
  stalls tool calls in the freshness check itself. Serve graphs from local
  storage.
- **A failed re-read keeps the previously loaded graph serving** and attaches a
  warning to tool results. It is retried only when both conditions hold: the
  file's identity changes *again* **and** at least five seconds have passed
  since the failure. A producer republishing torn bytes therefore cannot cost
  every call a doomed load, and a file that stays broken is never retried
  automatically at all. An explicit `reload_graph` always tries.
- **A file written by a newer kglite than this binary** cannot be read at all.
  The warning says to restart this server on a newer kglite rather than
  offering a retry that can never succeed.
- **A server holding unsaved changes never auto-reloads**, because the re-read
  would discard them silently. It warns on every response instead and leaves
  the choice to `save_graph_as` or `reload_graph(discard_unsaved=true)`. See
  *The writer lease* below.
- **Disk-graph *directories* carrying a `CURRENT` pointer are refreshed too.**
  A disk publish is atomic in the same way a `.kgl` rename is. It stages a
  fresh generation and swings `CURRENT` to it. It never rewrites the generation
  another server has mapped, and never deletes one. The pointer *is* the
  identity, so a peer's publish is noticed on the next tool call exactly as a
  republished file is. The cost is one open and read per call rather than a
  bare `stat`.
- **A legacy flat directory is not refreshed.** It has CSR files at the root
  and no `CURRENT`. Its files are rewritten in place, so there is no pointer to
  compare, and `reload_graph` remains its refresh path.
- **The automatic re-read refreshes skills.** When it installs new bytes, it
  re-resolves the skill layer from the graph's `KgliteSkill` records. It tells
  the client its tool list changed, as `reload_graph` does. Graph-carried
  recipe queries are fixed for the session either way.
- **`extensions.graph_watch` is retired.** The key is still parsed, and a
  non-boolean value still fails boot. Any boolean now only logs a retirement
  warning and arms nothing, because the refresh it used to opt into is
  unconditional. Remove it from the manifest.

## Serving a markdown vault (`--vault`)

`--vault DIR` serves a directory of frontmatter-markdown notes as a knowledge
graph built from the files themselves. The format is the one
[`VAULT.md`](../reference/vault-format.md) specifies.

```bash
kglite-mcp-server --vault /notes/handbook
```

There is no `.kgl` to produce or keep in step. The notes are canonical, the
graph is a derived view, and the server rebuilds it as the files change. A
manifest is optional; the common invocation is the one above.

Boot does the following, in order:

1. Opens the graph for `DIR`. It loads the vault's cached graph and rebuilds
   only if the directory has moved since. A build takes well under a second for
   a 7 000-note vault.
2. Binds `DIR` as the source root, so `read_source` / `grep` / `list_source`
   serve the notes as files too.
3. Registers `rebuild_graph`.
4. Installs the vault's own skills and recipe queries.

Behaviours to know:

- **`.kglite/vault.yaml`** configures the build: profile overrides, declared
  property types, indexes, text indexes, an ontology, and `embed:` targets. It
  is re-read on every rebuild, so it is the one piece of state a rebuild
  re-applies. A file that will not parse **fails the build**. The previously
  built graph keeps serving, and the error names the file. It never degrades to
  an empty graph.
- **`.kglite/skills/*.md` and `.kglite/recipes/*.md`** become the graph's own
  agent guidance, re-read on every rebuild. Editing one is the whole update
  procedure. The served skill set is re-resolved after the rebuild that picked
  it up. A file that fails validation is skipped with a warning naming it, and
  its siblings still load.
- **Rebuilds are lazy.** The watcher tags the graph dirty (debounced 500 ms
  upstream), and the rebuild happens on the next tool call. Fifty saves cost
  one rebuild. A path inside a hidden directory other than `.kglite/` is
  ignored, which keeps `.obsidian/` and `.git/` churn from rebuilding anything.
- **The graph is cached at `.kglite/graph.kgl`.** Boot writes it and the next
  boot loads it, so a restart over an untouched vault answers immediately
  instead of re-reading every note. Each rebuild refreshes it. The file is
  excluded from the vault's own fingerprint and from the watcher, so writing it
  never looks like an edit. It is an ordinary `.kgl`, so a vault can be
  *shipped* with its graph: build it once, commit it, and the first boot
  anywhere costs a `stat` pass.
  - `--vault-cache PATH` keeps the cache elsewhere, for a read-only vault or a
    vault whose git history should not carry it.
  - `--vault-cache none` switches it off.
  - **A cache never fails the boot.** One that cannot be read is rebuilt. One
    that cannot be written is skipped with a warning on stderr. The boot log
    always says which of "vault built from its notes" and "vault served from
    its cached graph" happened.
- **`rebuild_graph`** forces a rebuild now and returns the build report: notes
  scanned, nodes by label, edges by type, and the errors and warnings
  `kglite okf check` would print. It is the vault counterpart of
  `reload_graph`, which is not registered here, because a producer-backed graph
  has no served file to re-read.
- **Vectors are carried, not recomputed.** With `extensions.embedder` (and
  `trust.allow_embedder: true`) bound, each `embed:` target declared in
  `vault.yaml` is embedded at boot and after each rebuild. Only notes whose
  text actually changed are re-embedded. A note that keeps its label and id
  keeps its vector across a rebuild. A note moved to another folder is
  relabelled, so it is re-embedded. Without a bound embedder the vault still
  serves, and a declared `embed:` target logs a warning at boot rather than
  failing.
- **`--vault` and an injected producer are mutually exclusive.** A binary that
  embeds this server and injects `WorkspaceGraphHooks` is refused at boot. The
  message names `--watch` as the mode for its own producer, because only one
  producer can own the graph.

## Serving images (`fetch_images`)

`fetch_images` is the only route that returns the bytes of `Image` nodes, and only for the
paths an agent names. Every `![…](…)` reference in a note becomes an `Image`
node whose id is its vault-relative path.

```json
{"items": ["img/faults.png", "img/section-map.png"], "max_bytes": 1048576}
```

The reply is one image content block per delivered file, plus one text block.
The text block lists each item as delivered (path, MIME, byte count) or refused
(path, reason). A call where some items are refused still succeeds. A call
where every item is refused is an error carrying the same list.

- **The sandbox is the source tools'.** Paths resolve through the same
  `source_root` binding `read_source` uses, so nothing outside the served
  directory is reachable. Absolute paths, `~`, `file:` URLs and `..` segments
  are refused before resolution. The message names the contract rather than
  reporting a miss.
- **Registered everywhere, enabled where a root exists.** `--graph` auto-binds
  the graph's parent directory (see below), so an ordinary `--graph` server
  serves images sitting beside the `.kgl` with no configuration. A server that
  binds no root at all carries the route disabled and logs the reason at boot
  ("no source root: images are served from the vault root or `source_root`").
  That covers `kglite-mcp-server` with no mode flag, and a manifest whose
  declared `source_roots` do not resolve. An operator sees why, and a manifest
  that overrides the tool still resolves.
- **png, jpeg, gif and webp only.** Every other type, SVG and PDF included, is
  refused with its MIME named. Those files remain queryable as `Attachment`
  nodes; only their contents are unavailable. Convert diagrams to PNG when
  building the vault.
- **Caps, never resizing.** Defaults are 4 images per call, 4 MiB per image and
  12 MiB per call. An over-cap item is refused with its byte count named. The
  call's own `max_bytes` can only lower the per-image ceiling. Override the
  defaults in the manifest:

```yaml
extensions:
  fetch_images:
    max_items: 2
    max_bytes_per_image: 1048576
    max_total_bytes: 2097152
```

  All three keys are optional and must be positive integers. Anything else
  fails the boot rather than falling back to a cap you did not choose.
- **One stderr line per delivered image** (path and byte count), because this
  route moves more bytes per call than any other.

## Writable workbench

A server is **write-enabled** when either `--writable` is passed on the command
line or the manifest sets `extensions.writable: true`.

```bash
kglite-mcp-server --graph /data/work.kgl --writable
kglite-mcp-server --graph /data/new.kgl --storage memory --writable
```

The two are one statement made two ways. Either alone enables mutation through
`cypher_query`. It also registers `save_graph` plus the `load_graph`,
`create_graph`, and `save_graph_as` lifecycle tools:

```yaml
extensions:
  writable: true
```

### What outranks or fails to enable writes

`builtins.save_graph: true` is **not** a third spelling. On its own it
registers `save_graph` and nothing else, and it leaves `cypher_query`
read-only. It exists so a server can persist what it loaded, such as an
ontology materialized from `extensions.ontology` at boot. A mutation refused on
such a server names both write-enabling spellings and says so.

One thing can outrank both spellings. A Rust binary that embeds this server as
a library may pin it read-only (`ServerExtensions::read_only()`), and an
operator cannot lift that from either surface. That is deliberate: the embedder
owns argv but not the manifest, and regenerates the graph from its own source
of truth. Such a server logs one warning at boot naming the write opt-in it
overrode, so `--writable` or `extensions.writable: true` doing nothing is
visible in the log rather than a mystery. The stock `kglite-mcp-server` binary
sets no pin. Check the log if a wrapper binary refuses mutations you enabled.

### Misspelled keys

A misspelled key is the one failure the manifest spelling (`extensions.writable`)
adds, and it fails safe. The server
comes up read-only and the first mutation is refused.

Boot also warns about any `extensions:` key this server does not read, and the
warning lists the ones it does: `cypher_recipes`, `value_codecs`, `ontology`,
`graph_watch`, `parallel`, `tools_allow`, `write_scope`, `csv_http_server`,
`embedder`, `writable`. So `extensions.writeable: true` shows up in the log
instead of silently doing nothing. It is a warning rather than a boot error
because a skill's `applies_when: {extension_enabled: …}` predicate reads the
same block. That predicate may legitimately name a key no reader here knows.

### Storage mode

`--storage memory|mapped|disk` is required when the `--graph` target does not
yet exist. On an existing graph it *converts*: a memory-saved graph booted with
`--storage mapped` comes up mapped. A disk graph is a directory rather than a
file, so converting into or out of disk mode has no in-place form. It is
refused at boot, naming `enable_disk_mode()`. Omit the flag to serve whatever
mode the graph recorded.

Keep read-only mode for untrusted agents. Scope filesystem access with
manifest `source_root`/`source_roots`.

### The writer lease, and several servers on one file

A `.kgl` has one writer at a time, guarded by an advisory lock on a
`<name>.kgl.lock` sidecar. A server takes that lease **at its first unsaved
change**, not at boot:

- A **read-only** server serving a `.kgl` that already exists never takes it.
  Any number of them can serve one file while a rebuilder republishes it in
  place.
- A **write-enabled** server (`--writable`, or `extensions.writable: true`)
  boots lease-free as well. The first mutating `cypher_query` acquires the
  lease. It is held until one of these happens:
  - `save_graph` writes the changes back;
  - `save_graph_as` moves them elsewhere;
  - `reload_graph(discard_unsaved=true)` drops them;
  - the process exits.

  Outside that window the server is an ordinary reader.
- A server with **`builtins.save_graph: true` alone** has a read-only
  `cypher_query`, so it never holds unsaved mutations and never opens a lease
  window for them. It still owns the file. It takes the lease for the moment a
  `save_graph` publishes, and hands it straight back. That publish is the
  boot-time ontology materialization the key exists to persist.

Several write-enabled servers can therefore serve the same graph and arbitrate
per *write* rather than per process. Four MCP clients booted from one manifest
are an example. The first to mutate holds the lease. A peer that writes while it
is held waits about a quarter of a second and is then refused, by name:

```
cypher_query refused: /data/work.kgl is open for writing by "Claude Desktop"
(pid 4711, since 2026-09-01T09:12:04+02:00); only one process may write a graph
at a time. […] Nothing was changed here, and this graph is still readable —
keep querying it.
```

The name comes from the first of these that is set:

1. `--lease-label`;
2. the `KGLITE_LEASE_LABEL` environment variable;
3. the name of the process that spawned this server, usually the MCP client
   itself. This is how four clients sharing one manifest still name themselves
   apart.

A refused write changes nothing and the graph stays readable. This server picks
up what the holder wrote on its next call.

#### Writes that reach disk

Writes that reach disk cannot silently overwrite each other either:

- `save_graph` refuses if the file changed on disk since this server loaded or
  last saved it. There is no merge between the two versions.
  - `save_graph_as` to another path keeps this server's work.
  - `reload_graph(discard_unsaved=true)` drops it and serves the file as it is.
- `save_graph` with nothing unsaved is a **no-op**. It answers `Nothing to
  save: <path> is clean and carries no unpersisted configuration, so the file
  was not touched.` It takes no lease and leaves the file's identity alone, so
  peers serving the same graph are not made to pay a full re-read for a save
  that would have written the same graph back. Two things still get written:
  - Unsaved mutations.
  - Configuration the version counter cannot see, which today means a manifest
    ontology applied at boot (`extensions.ontology`, declared or materialized).
    Such a server's **first** save persists it and its second is the no-op. Its
    response names what it wrote (`wrote manifest ontology (N classes, M
    managed labels); no data changes`) rather than a node count nothing moved.

  For a deliberate rewrite that neither explains, pass `force=true`. An example
  is re-encoding the file with the running library version. `force` works
  **only on a write-enabled server**. It re-encodes the file and moves its
  identity, so it is offered where mutations are (`--writable` /
  `extensions.writable: true`) and refused on a server that registers
  `save_graph` alone.
- `save_graph_as` **to the bound path** is `save_graph` under another name,
  that lost-update check included. To a *different* path it also releases the
  source file's lease. The graph is not going back there, and this is the call
  an agent reaches for to get out of the jam.
- `reload_graph` refuses to discard unsaved changes silently, and `load_graph`
  / `create_graph` refuse outright while the server is dirty. All three name
  `reload_graph(discard_unsaved=true)`: throwing work away has one spelling.
- Every `cypher_query` result footer carries `file saved <T>`, `load N`, and
  either `clean` or `unsaved changes — lease held since <T>`. This holds for
  reads and writes alike. The `<active_graph>` header on `graph_overview`
  (`file_saved="…" load="…" state="…"`) and the activation summary carry the
  same fields. A lease
  parked by a write that died mid-call is therefore visible on every query
  instead of only to whoever writes next.
- **`load` is server-local; `file saved` is the shared identity.**
  - `load` counts the graphs *this server process* has installed since boot.
    It is how you tell a re-read from a skipped freshness check on one server. Two
    servers on the same path report different numbers for the same bytes, and a
    server's own save does not move its own.
  - `file saved` is the served path's publish time taken off the filesystem.
    Every server on the path agrees on it once refreshed. Compare it when you
    ask whether two clients are serving the same graph.
  - A server holding unsaved changes reports the moment it loaded rather than
    the file's current one. That is correct by design: it is the identity its
    `save_graph` will be checked against.
  - The field is omitted entirely for a graph with no file behind it (a
    workspace graph) and for a legacy flat directory, which has no publish
    moment.

#### Disk-graph directories

The same applies to a **disk-graph directory carrying a `CURRENT` pointer**. It
is a graph republished atomically, so it is served lease-free and locked only
between a first unsaved change and the `save_graph` that publishes it. Several
servers can therefore serve one directory and arbitrate per write.

Reading one lock-free is safe because a publish never touches the generation a
reader has mapped. It stages a new generation and swings the pointer. The
writer deletes only generations older than the one before the generation it
just published, so a reader that re-reads `CURRENT` per call always finds its
generation. The retention setting is `KGLITE_KEEP_GENERATIONS`: default one
previous generation, `all` to keep every one.

These `generations/` directories are the disk mode's own on-disk versions. They
are unrelated to the `load` counter in the footer. `load` counts one server's
installs and is not written anywhere. A generation is a published artifact
every process sees.

Two targets keep the lock from the open instead, because waiting is not safe
for them:

- A path that does not exist yet. This open is creating it, and locking first
  is what stops two servers from both creating it. A created path joins the
  lazy lifecycle once its first `save_graph` has published it.
- A **legacy flat directory**: a pre-generations disk graph whose CSR files sit
  at the root with no `CURRENT` beside them. A rebuild rewrites it in place
  under this server's live mappings. It keeps its lock for as long as the
  server serves it.

Budget for the directory's growth before you enable `save_graph` on one. Every
disk save publishes a new generation. The generation shares (by hard link) the
column file of every node type it did not change. A disk save keeps the current
generation plus one previous, and older ones are deleted. That is the
`KGLITE_KEEP_GENERATIONS` setting described under *Durability* in the
[durable-apps guide](../python/guides/durable-apps.md). Set it to `all` if a
reader in another process can stay more than one generation behind.

#### Operating notes

- **Never delete `<name>.kgl.lock` from a build script or a cleanup job.**
  Deleting it does not release a live lock and does nothing for a dead one. The
  operating system releases the lease when the holder exits, crash included.
  All the deletion removes is the `<name>.kgl.lock-owner` record that lets the
  next refusal name the holder.
  - That record also says how the last holder left. A lease handed back
    cleanly appends a `released=<timestamp>` line to it. A record with no such
    line was left by a holder that died still holding one.
  - It is forensics, not liveness. The lock decides whether a write waits. A
    `released=`-less record beside a file nothing holds means the last writer
    crashed, not that the graph is locked.
- **A peer that merely *inspects* the graph with `kglite.open(path)` rewrites
  it.** `open()` is the writer's entry point. It takes the lease, and its
  `close()` (or `with`-block exit) writes the whole graph back even when
  nothing was mutated. That rewrite costs every serving server one full re-read
  on its next call. A server that was holding unsaved changes has its
  `save_graph` refused from then on. Inspect with `kglite.load(path)` or
  `kglite.open_session(path)`, which take neither the lease nor the save-back
  binding.
- **An unlocked library save can replace this server's checkpoint.** Python
  handles opened with locking transfer their lease on save-as. These still rely
  on the caller to coordinate writers:
  - handles from `kglite.load()`;
  - explicit `lock=False`;
  - the raw Rust `save_graph`;
  - the C save entry points.

  A script that does `kglite.load(path)`, mutates in memory and calls
  `save(path)` therefore publishes over a path this server is mid-write on.
  Nothing is lost from the *file*. It holds a complete graph, and this server's
  own `save_graph` then refuses because the file changed on disk. But the
  agent's unsaved work is not in it and has to be redone. Any caller that may
  save to a path must hold the lease across the whole read-modify-save
  interval. Reach for `kglite.open(path)`, not `load()` + `save()`.

### The source root `--graph` binds by default

In `--graph` mode, a manifest that declares no `source_root`/`source_roots`
still gets a source root. The parent directory of the `.kgl` file is
auto-bound as the sole static source root, so the file-reading tools serve the
files sitting next to the graph with no configuration. That is the default, not
a fallback for a missing manifest. A manifest that configures Cypher tools and
skills but says nothing about roots still gets it.

Reads stay confined to the bound root. The directory the graph lives in is
therefore exactly the blast radius: a `.kgl` at the top of a home directory or
a shared volume binds all of it.

An explicit declaration wins outright. The auto-bind applies only when the
manifest names no roots at all:

```yaml
# serve the graph from /data but read files only from /srv/project
source_roots: [/srv/project]
```

- To scope it, name the narrower directory.
- To move it, name a different one.
- To serve no files from a wide graph directory, keep the graph in a directory
  of its own, or drop the source tools from `extensions.tools_allow` (above),
  which is the closed-by-default surface.

`--source-root`/`--watch`/`--vault` mode has no auto-bind question: the
directory is the argument.

### Valid time: the default instant

On a graph that declares validity intervals, a `cypher_query` or recipe call
with no `valid_at` and no `FOR VALID_TIME` prefix reads **as of today** (UTC).
The `temporal:` line says so (`default`).

- An agent that needs history sends `valid_at: "all"` or starts the query with
  `FOR VALID_TIME ALL`.
- A recipe that means history starts with the prefix. The prefix does nothing
  on a graph with no declaration.
- The operator moves the default with `--valid-time-default
  {today|all|YYYY-MM-DD}` or `extensions.valid_time.default`. The flag wins.
- With neither set, the served graph's own stored default (saved in its `.kgl`)
  applies, else today.
- `graph_overview` shows a `<valid-time-default>` line when the default is not
  today.

See {doc}`/python/guides/valid-time`.

### Pinning the write scope

`cypher_query`'s `write_scope` argument is set by the agent, so by itself it is
role hygiene rather than access control. The operator's counterpart is
`--write-scope` (comma-separated) or `extensions.write_scope`:

```bash
kglite-mcp-server --graph /data/work.kgl --writable --write-scope Plan,Task
```

```yaml
extensions:
  write_scope: [Plan, Task]
```

The pin is a ceiling, and it never falls open:

- the agent omits `write_scope` → the pinned scope applies (not unrestricted);
- the agent supplies one → the two are **intersected**, so it can narrow but
  never widen;
- nothing left in scope → the write is refused, with a message naming the
  server's scope so the agent can tell a policy refusal from a typo;
- flag *and* manifest key set → those two are intersected as well, and the
  effective scope is logged at boot.

A malformed `extensions.write_scope` fails the boot rather than being dropped.
Malformed means anything but a list of strings. The reasoning is the same as
for `extensions.tools_allow`. An explicit `[]` is honoured literally: a
write-enabled server that permits no writes.

The scope covers:

- node writes, by the node's **stored** type, so a pattern label cannot widen
  it;
- relationship writes, where at least one endpoint's stored type is in scope.

The scope deliberately does not cover:

- relationship *constraint* DDL;
- `db.cdc.*`;
- the graph-lifecycle tools, which replace or persist the whole graph rather
  than writing nodes in it. An agent that must not swap the served graph should
  not have `load_graph`/`create_graph`/`save_graph_as` in
  `extensions.tools_allow`.

Those lifecycle tools are also the ones that *end* a lease window. Suppose an
allowlist hides both `save_graph` and `reload_graph` from a server that can
still mutate. That server holds the writer lease from its first write until the
process exits. It locks every peer out of the graph (a `.kgl` or a generation
directory alike) for the session. Hide the mutation route (`cypher_query`
write scope, or read-only mode) rather than the way back out of one.

## Code intelligence

The generic KGLite server serves and queries code graphs but does not build
them. Use **codingest-mcp** for repository cloning, parsing, local watch mode,
and multi-revision code-graph construction. It embeds this same graph-serving
surface with the builder injected.

The complete manifest, skill, tool-gating, and client-registration reference is
the [MCP servers guide](../python/guides/mcp-servers.md).
