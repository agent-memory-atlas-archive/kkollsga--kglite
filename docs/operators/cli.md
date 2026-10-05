# CLI

The main `kglite` wheel includes the `kglite` command for working with `.kgl`
graph files without starting a server. It has two modes:

- one-shot commands for scripts and agents
- an interactive Cypher shell for humans

Install the Python API and CLI together:

```bash
pip install kglite
```

For a standalone CLI-only installation:

```bash
pip install kglite-cli
# or build the libpython-free binary from crates.io
cargo install kglite-cli
```

Both routes expose the same Rust CLI implementation. Do not install both into
one environment, because they provide the same `kglite` command name.

## Code-Review Skill

The code-review Agent Skill ships with
[codingest](https://github.com/kkollsga/codingest), the project that builds the
code graphs it queries:

```bash
codingest skill install
```

The skill drives this CLI for querying. Build a working-tree graph, or a graph
that spans a committed base and head revision:

```bash
# Code-graph builds moved to the codingest project (its CLI builds the .kgl):
# see the codingest README. Example shape:
#   codingest build . --output .kglite/code-review.kgl
#   codingest status --output .kglite/code-review.kgl
```

- `build` writes a metadata sidecar with the source/revision fingerprint.
- `status` reports `fresh`, `stale`, or `missing` without loading the graph.
- The review workflow still calls `describe` before Cypher. It verifies
  structural results against exact source lines.

## One-Shot Commands

`kglite query` runs a read-only Cypher query and exits:

```bash
kglite query app.kgl "MATCH (n:Person) RETURN n.name AS name" --format json
```

`--format json` emits one object per row. The keys follow the query's own
column order: `RETURN 1 AS zz, 2 AS aa` yields `{"zz": 1, "aa": 2}`. That is
the same order `--format csv` writes its header in. The top-level shape is a
plain array, so `jq '.[0].name'` addresses the first row's column.

### Valid time

On a graph that declares validity intervals, a statement with no
`FOR VALID_TIME` prefix reads **as of today** (UTC). Prefix it with
`FOR VALID_TIME ALL` to read every version.

`--valid-time-default {today|all|YYYY-MM-DD}` on `query`, `write` and `session`
changes what an unprefixed statement reads for that run. A statement's own
prefix still wins. Without the flag, a default the graph stored in its file
applies, else today.

### Query deadlines

**The CLI applies no query deadline by default.** That is a declared
divergence, not an oversight. The Python API and the MCP server both default to
180,000 ms. The CLI does not, because `Ctrl-C` cancels a running read here and
a batch query over a Wikidata-scale graph legitimately runs for hours. A silent
three-minute kill would be the regression.

Bound one call with `--timeout-ms`, on `query` and on `write`:

```bash
kglite query app.kgl "MATCH (a)-[*1..6]-(b) RETURN count(*) AS n" --timeout-ms 30000
```

The query exits non-zero with the engine's timeout message. `--timeout-ms 0` is
the same as omitting the flag.

`row_limit` has no CLI spelling. In Python it caps the rows a call *retains*.
Write `LIMIT n` in the query, which is what a one-shot command wants anyway.

### Writing and saving

`kglite write` runs a write statement and saves the graph:

```bash
kglite write app.kgl "CREATE (:Task {id:'t1', status:'todo'})" \
  --save \
  --write-scope Task \
  --git-sha abc123 \
  --modified-by agent
```

`--write-scope` restricts a statement's writes to the listed **node types**.
`--git-sha` and `--modified-by` stamp provenance on `auto_timestamp` types.

Scope rules:

- **Node writes** are judged by the node's *stored* type, so a pattern label
  cannot widen the scope. Node writes are `CREATE`, `INSERT`, `MERGE`'s create
  arm, `SET`, `REMOVE`, `DELETE`, `NODETACH DELETE`, `DETACH DELETE`, and
  index/constraint DDL.
- **Relationship writes** are allowed when **at least one endpoint's** stored
  type is listed. Relationship writes are a `CREATE`/`INSERT` relationship
  pattern, `DELETE r`, `SET r.p`, and `REMOVE r.p`. Linking to a matched
  out-of-scope node is permitted. An edge between two out-of-scope nodes is
  not.
- **Not scoped:** relationship types themselves, and
  `db.cdc.enable`/`db.cdc.disable`.
- Deleting a node in scope removes its relationships whatever they point at.

### Dependency frontier

`kglite ready-set` inspects a dependency frontier:

```bash
kglite ready-set app.kgl \
  --done 'n.status = "done"' \
  --node-type Task \
  --format csv
```

### Describing a graph

`kglite describe` prints the agent-oriented graph description:

```bash
kglite describe app.kgl
kglite describe app.kgl --types Task
kglite describe app.kgl --cypher
kglite describe app.kgl --connections
```

`describe` returns the same XML schema document exposed by the Python API and
MCP server. It includes focused views for labels, Cypher support, and
connection types.

When the graph carries skills or recipe queries, the document indexes them in
`<skills>` and `<recipes>` elements. Each entry has a name, a one-line
description, and the call that reads the full text. A graph carrying neither
renders neither element, so nothing about an ordinary graph's description
changes.

### Reading skills

`kglite skill` reads the skills a graph carries:

```bash
kglite skill app.kgl                  # name + description of each skill
kglite skill app.kgl --format json    # or csv; the listing honours --format
kglite skill app.kgl wells            # the body, raw markdown on stdout
kglite skill app.kgl wells > wells.md
```

- **With no name**, the command lists every skill by name and description,
  sorted, in the `--format` you ask for (`table` by default, plus `csv` and
  `json`).
- **With a name**, it prints that skill's body byte for byte: no reformatting,
  nothing appended. You can pipe or redirect it. A body is always raw;
  `--format` applies to the listing only.
- A graph carrying no skills lists zero rows and exits **0**.
- A name the graph does not carry is an error naming both the name and the
  graph, and exits **non-zero**.

A skill is markdown methodology stored inside the `.kgl` itself, which an MCP
server serves to an agent. `kglite skill` is the offline check on what that
server would serve. The server merges the graph's skills with its own bundled
and operator layers, and re-reads the graph's on every `reload_graph`, so the
two can differ. See [Authoring MCP skills](../python/guides/mcp-skills.md).

The command is read-only. It takes no writer lease, so it is safe against a
graph another process owns. Writing skills is the Python API's (`set_skill`,
`import_skills`), as is writing recipe queries (`set_recipe`,
`import_recipes`). There is no `kglite recipe` subcommand, because a CLI user
writes Cypher.

## Vault Directories

`kglite okf` reads a **vault** without Python. A vault is a directory of
frontmatter-markdown notes in the format `VAULT.md` specifies. The subcommands
are:

- `check` reports what a build would find and sets the exit code.
- `build` keeps the result as a `.kgl`.
- `export` runs the other way, writing a `.kgl` back out as a vault.
- `status` answers the cheap question: has the vault moved since the graph was
  built?

```bash
kglite okf check vault/                       # counts, then errors, then warnings
kglite okf check vault/ --strict              # warnings fail too
kglite okf check vault/ --json                # the same report, machine-readable
kglite okf build vault/ -o vault.kgl          # build and save
kglite okf export vault.kgl out/ \
    --source-root vault/                      # write the graph back out
kglite okf status vault/                      # the vault's fingerprint
kglite okf status vault/ --graph vault.kgl    # 0 = current, 1 = stale
```

### `okf check`

`check` runs the same read `build` runs and throws the graph away. It reports
what a build does rather than a second opinion about it.

- It exits **0** when the report carries no error, and **non-zero** otherwise.
- `--strict` counts warnings as errors too, which is what a converter's own
  test suite wants.
- The classification is `VAULT.md` §9. An id collision or a reference climbing
  out of the vault is an error. A dangling link or a missing attachment is a
  warning.
- `--json` prints `{ok, strict, counts, errors, warnings}`, with the same keys
  the Python `okf.validate()` report carries.

### `okf build`

`build` writes the graph to `-o`. It prints the same report on **stderr**,
leaving stdout for the path it wrote.

`--dialect` takes `obsidian` (the default), `okf` or `loose`. An unrecognised
spelling is refused rather than quietly read as something else.

Everything else about a vault happens in the vault itself, not in flags:
declaring indexes, hubs and embed targets in `.kglite/vault.yaml`, and carrying
skills and recipes in `.kglite/`.

### `okf status`

`status` reads no note. It `stat`s the files a build would read and folds them
into the fingerprint `VAULT.md` §12 specifies. Alone it prints that number and
the directory, reading it as a vault.

With `--graph` it compares the fingerprint against the one stamped in the
`.kgl` when it was built:

- It prints `current` and exits **0**, or
- it prints `stale` with both fingerprints and exits **non-zero**.

That is the verdict a scheduled rebuild checks before doing any work.

- **With `--graph`, the dialect comes from the graph**, because the fingerprint depends on it.
  A `.kgl` built with `--dialect okf` is compared as an OKF bundle without
  being asked. A `--dialect` that contradicts the stamp is refused instead of
  reported as `stale`.
- **A `.kgl` with no provenance** (one not built by `okf build`) is an error
  rather than a verdict, because there is nothing to compare.
- **Keep the `.kgl` *outside* the vault.** Every non-hidden file under the root
  is a candidate attachment, so a graph written into the vault changes the
  vault.

### `okf export`

`export` writes one `.md` file per node under a folder named for its label. It
copies the graph's attachments when `--source-root` names the directory they
were read from. It prints its report on **stderr**, with the directory it wrote
on stdout (`VAULT.md` §10).

**It never replaces a file it did not write.** `.kglite/export-manifest.json`
records a hash per exported file. A file missing from it, or edited since, is
refused and named on stderr, and the command exits non-zero. `--force` lifts
exactly those two refusals. Exporting the same graph twice is byte-identical,
so the output is worth committing.

#### Edge tables

A frontmatter list carries an edge's target and not its properties, so those
properties are a documented loss, unless the type is **declared as an edge
table**. There are two ways to declare one:

- The source vault declares it in its own `.kglite/vault.yaml`
  (`export: {edge_tables: {WORKED_ON_BY: "Worked on by"}}`, `VAULT.md` §7.3).
  The export finds it through the graph's provenance.
- `--edge-table` says the same thing from the command line. It is repeatable
  and wins per type.

```bash
kglite okf export vault.kgl out/ \
    --source-root vault/ \
    --edge-table 'WORKED_ON_BY=Worked on by'
```

Each declared type's edges are written as a GFM table under that heading in the
source note's body. That is the only prose an export ever adds. The table has
one column per property, and the export owns it on the next round.

Reading the table back needs the matching `structure.tables … edges: true`
rule. That rule lives in `vault.yaml`, which no export writes, so copy that
file across. The report warns on stderr when the rule is missing or when no
exported note emits the type.

## Agent Sessions

For byte-bounded output with executable retrieval commands, use explicit
`--format agent` on one-shot `query` or `write`. The default JSONL session
contract remains complete. Individual requests opt in with `"format":"agent"`
and discover retained expansion through `{"op":"help"}`. See
[Bounded agent responses](agent-responses.md) for controls, expansion, cache
lifecycle, and the distinction between a response budget and Cypher `LIMIT`.

Use `session` when an agent needs multiple operations against the same graph.
The process keeps one graph loaded in memory and accepts JSONL requests on
stdin:

```bash
kglite session app.kgl --format json
```

Example request stream:

```json
{"op":"help"}
{"op":"describe","types":["Task"]}
{"id":"w1","op":"write","query":"CREATE (:Task {id:'t1', status:'todo'})"}
{"id":"q1","op":"query","query":"MATCH (t:Task) RETURN count(t) AS n","format":"json"}
{"op":"save"}
{"op":"exit"}
```

Responses echo `id` when provided. In JSON mode, `query` and `write` return
typed `rows`. Table and CSV modes return rendered `output`.

### Output formats

CSV is a machine format. Integers, floats, timestamps, and values nested in
lists or maps retain their available text precision. RFC quoting preserves
commas, quotes, CR, and LF. As in normal CSV, an empty string and NULL are both
empty fields; use JSON when that distinction matters. Table mode remains
compact for people.

### Discovering ops

`{"op":"help"}` answers with the op table: every op and its request shape. A
driver that only has the pipe can discover the protocol from inside it. An
unknown op names the valid ops in its error.

For focused descriptions, agents can use compact or explicit object forms:

```json
{"op":"describe","connections":true}
{"op":"describe","connections":["KNOWS"]}
{"op":"describe","connections":{"detail":"overview"}}
{"op":"describe","connections":{"types":["KNOWS"]}}
```

The same object style works for `types`, `cypher`, and `fluent` detail
selectors where applicable.

## Interactive Shell

Open the shell with a graph path:

```bash
kglite app.kgl
```

Run with no path for a scratch in-memory graph:

```bash
kglite
```

Cypher statements execute when terminated by `;`, so a query can span multiple
lines. Dot-commands execute on Enter. Tab completion covers dot-commands and
graph labels.

### Piped input

Piped input runs the same way, with two allowances for scripts. A dot-command
line terminates a statement still waiting for its `;`, and so does the end of
input. So `kglite app.kgl <<< 'MATCH (n) RETURN count(n)'` prints its result
instead of exiting silently.

A tail left unbalanced by an unclosed quote or bracket runs nothing, names
itself on stderr, and exits non-zero. Table output is only width-capped on a
terminal (honoring `COLUMNS`). Piped output renders every value in full.

### Dot-commands

Common dot-commands:

- `.help` — list commands
- `.quit` / `.exit` — leave the shell
- `.labels` / `.rels` / `.schema` / `.indexes` — inspect schema
- `.mode table|csv|json` — set output format
- `.import <file.csv> <NodeType> [--id <col>] [--title <col>]` — import CSV rows as nodes
- `.dump <dir>` — export CSV files plus a `blueprint.json`
- `.read <file>` — run Cypher statements from a file
- `.save [path]` — save the graph to a `.kgl` file
- `.timing on|off` — show query wall time

`Ctrl-C` cancels a running query. `Ctrl-D` exits.

## Other Commands

### `export-text` and `diff`

`export-text` prints the deterministic text projection used by git textconv:

```bash
kglite export-text app.kgl
```

`diff` compares two graph projections:

```bash
kglite diff before.kgl after.kgl
```

### `export-sqlite`

`export-sqlite` writes a SQLite-dialect SQL script, so the graph can leave
KGLite entirely. Node types become tables, and connection types become link
tables. Give it an output path, or omit one to write to stdout:

```bash
kglite export-sqlite app.kgl dump.sql
sqlite3 app.db < dump.sql

kglite export-sqlite app.kgl | sqlite3 app.db     # or pipe it straight through
```

The output is deterministic: the same graph always produces byte-identical SQL.
It is dependency-free, because no SQLite library is linked into KGLite. The
full mapping and its trade-offs are in the
[import/export guide](../python/guides/import-export.md).

### `export`

`export` writes the open formats: a lossless CSV tree with a re-import
blueprint, or RDF 1.2. The format is inferred from a `.nq` / `.trig` output
path, or named with `--format csv|nq|trig`. `--base` and `--schema-org` apply
to the RDF formats:

```bash
kglite export org.kgl org.nq
kglite export org.kgl org.trig --base https://hr.example.org/
kglite export org.kgl org-csv --format csv    # a directory
```

The formats, the `kg:` vocabulary and the limits are in the
[open exports guide](../python/guides/open-exports.md).

### `schema-version` and `migrate`

`schema-version` reads the graph's **user-schema version**, and with `--set`
writes it. That version is your own data-model revision. The engine stores it but never interprets it. It is distinct from the `.kgl`
format version:

```bash
kglite schema-version app.kgl            # prints e.g. 3
kglite schema-version app.kgl --set 2    # stamp without running anything
```

`migrate` applies pending Cypher migrations and advances that stamp. Migrations
are `<version>_<name>.cypher` files in one directory, applied in ascending
version order:

```bash
kglite migrate app.kgl migrations --dry-run   # show the plan, change nothing
kglite migrate app.kgl migrations             # apply
```

Migration guarantees:

- Re-running is a no-op.
- Statements run against an in-memory copy, so a failure part-way leaves the
  `.kgl` byte-identical.
- A stamp the migration set cannot explain is refused rather than guessed at.

See the [schema-migrations guide](../python/guides/schema-migrations.md).
