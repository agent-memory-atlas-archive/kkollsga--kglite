# OKF Ingestion

Load **Open Knowledge Format** bundles — directories of markdown files with YAML
frontmatter, cross-linked by markdown links — into KGLite knowledge graphs. This
is Google's [Open Knowledge Format](https://github.com/GoogleCloudPlatform/knowledge-catalog),
and just as usefully your **Claude memory directory**, a **skills folder**, an
**Obsidian vault**, or a **GraphRAG corpus** — they all have the same shape.

OKF deliberately ships *no* query engine. KGLite supplies the missing half:
once a bundle is a graph, you get Cypher, `CALL leiden` / `pagerank`, the
`orphan_node` rule, and temporal filters over it for free.

> **Ingesting a repo's docs?** The same parser powers codingest's
> `include_docs=True` option, which ingests a codebase's markdown as `:Doc`
> nodes and links them to the code they describe. See
> docs pass (`include_docs=True` in codingest builds).

The YAML parser is bundled in the wheel — no extra needed:

```bash
pip install kglite
```

## Quick Start

```python
from kglite import okf

# Strict OKF (bundle-relative markdown links)
g = okf.build("path/to/bundle")

# Loose: also resolve [[wikilinks]], tolerate missing `type`
g = okf.build("path/to/memory", dialect="loose")

# Obsidian vault: folder labels, stem ids, stored bodies, attachments
g = okf.build("path/to/vault", dialect="obsidian")

# Now query it like any graph
g.cypher("MATCH (n) RETURN labels(n)[0] AS type, count(*) ORDER BY type")
```

### Sweep many projects in one pass

By default `build` only ingests `.md` files that have a YAML frontmatter block
(`require_frontmatter=True`) — the discriminator between *structured* knowledge
(OKF concepts, Claude memories) and plain markdown (READMEs, notes). So you can
point at a **parent of many projects** and extract only the structured knowledge
across all of them in one sweep — plain docs are skipped, each project's tree
becomes `Folder` nodes, and concept ids stay path-relative so they don't collide:

```python
g = okf.build("~/code", dialect="loose")   # require_frontmatter=True
g.cypher("MATCH (f:Folder)-[:CONTAINS]->(m) "
         "RETURN split(m.concept_id, '/')[0] AS project, count(m) AS memories "
         "ORDER BY memories DESC")
```

Node labels fall back `type` → `metadata.type` → `Concept`, so Claude memories
(which carry `metadata.type`, not a top-level `type`) land as `:feedback` /
`:project` / `:user` / `:reference`, with their `name` as the title. Pass
`require_frontmatter=False` to ingest every `.md` (vault-style).

To exclude an individual file from sweeps, add `kg_skip: true` to its
frontmatter — it's honored by default (pass `respect_skip=False` to ingest
skip-marked files anyway). To exclude whole directories you don't own (cloned /
vendored trees), pass `skip_dirs` — gitignore-style: a bare name matches a
directory at any depth, a `path/with/slashes` is an anchored bundle-relative
subtree:

```python
g = okf.build("~/code", skip_dirs=["node_modules", "vendor/repos", "mistral.rs"])
```

## How a bundle maps to a graph

Ingestion is **read-only and partial** — conceptually a code-graph build
for prose instead of source code. The directory stays the source of truth; the
graph is a rebuildable lens over it.

| Bundle element | Graph element |
|---|---|
| A concept (`.md` file) | A node — label from frontmatter `type` (or `Concept`), id = path minus `.md` |
| Frontmatter keys | Node properties (`tags`/lists → a JSON string in the `okf` and `loose` dialects, a native list under `obsidian`; nested `metadata:` → dotted keys `metadata.type`) |
| The markdown body | **Not stored** — a `file_path` pointer is kept; read on demand with `okf.source()` (or pass `with_body=True`) |
| A markdown link | A typed directed edge (see the ladder below) |
| `tags:` entries | `(:Concept)-[:TAGGED]->(:Tag)` — a Tag hub per distinct tag |
| External `http(s)` links | `(:Concept)-[:CITES\|REFERENCES]->(:Source {url})` |
| Each directory | `(:Folder)-[:CONTAINS]->` its concepts and subfolders; `index.md` enriches the Folder's title/description |
| A link to a not-yet-written concept | A `_provisional` stub node |
| `log.md` | Reserved — skipped |

Tag, Source, and Folder nodes are synthesized by default — they turn the sparse
author-link graph into a dense, well-clustering one (the hubs connect otherwise-
disconnected concepts). Disable per kind via `BuildOptions` if you want a bare
concept graph.

### The edge-type ladder

OKF links are untyped (the relationship lives in prose), so the connection type
is inferred most-specific-first:

1. an explicit link **title** that looks like a type — `[customers](/tables/customers.md "JOINS_WITH")`
2. the enclosing **section header** — `# Joins` → `JOINS_WITH`, `# Citations` → `CITES`, `# References` → `REFERENCES`
3. the generic fallback — `LINKS_TO`

Plus structural `CONTAINS` edges from the directory hierarchy.

Link resolution is forgiving: a `[[wikilink]]` or path resolves by exact id →
file stem → normalized slug (case- and `_`/`-`-insensitive) → title, so
`[[my-note]]`, `[[My Note]]`, and `my_note.md` all reach the same concept.

## Obsidian vaults

`dialect="obsidian"` is its own contract, not a synonym for `"loose"`: labels
come from the top-level folder (or a declared `default_label`), ids are
filename stems so a folder move keeps a note's identity, bodies are stored,
frontmatter lists stay lists, wikilink-valued frontmatter keys become typed
edges, a note's `aliases:` answer link resolution, every body link carries its
enclosing heading as a `section` edge property, inline `#tags` join the same
`Tag` hub as `tags:` (case-insensitively, as in Obsidian — `#Seismic` and
`#seismic` are one tag), Obsidian's own `cssclasses:` is ignored, folder notes build a hierarchy, and referenced images
become `Image` nodes you fetch as files rather than bytes in the graph. A `.kglite/vault.yaml`
in the vault root declares property types, indexes, text indexes, an ontology
and embed targets, and is re-applied on every rebuild.

**The structure profile.** A note is one node with one body until
`.kglite/vault.yaml` declares a `structure:` block — then the note's own
markdown becomes nodes too: headings are `Section`s with parent and next edges,
paragraphs pack into `Chunk`s you embed and search, callouts become `Note`s,
fenced blocks `Example`s, numbered lists `Procedure`/`ProcedureStep` chains, and
a table under a declared heading becomes one node per row (or, with
`edges: true`, one *edge* per row carrying the other columns as edge
properties). The ids are Obsidian's own — `[[Page#Heading]]`, `[[Page#^block]]`
— so every derived node stays linkable from any note and navigable in Obsidian.
A heading that is really a symbol name is relabelled in place by
`key_from_heading:`; `inherit:` copies the note's own properties onto every
derived node, `embed_text:` renders the string those nodes are embedded and
searched on, and `edge_defaults:` stamps a constant property on every edge of a
type. Nothing is rewritten and no file is added; the block is opt-in, and a
vault without one builds exactly as before. VAULT.md §7.1 specifies the keys
and §13 is the modelling guide for a converter deciding what to emit —
including the loop to convert by: a fifty-page sample, `kglite okf check <dir>
--strict`, the per-label counts read against what the sample holds, then fix
the converter rather than the vault.

The format is specified in
[VAULT.md](https://kglite.readthedocs.io/en/latest/reference/vault-format.html),
which is also the checklist to follow when writing a converter from HTML or any
other source into a vault. Check a vault with `kglite okf check <dir>`.

**Converting HTML help into a vault.** A vendor help corpus — thousands of HTML
pages, a JSON table of contents, `<meta>` metadata and an image directory — is
the case the format was shaped around, and
[`examples/html_to_vault.py`](https://github.com/kkollsga/kglite/blob/main/examples/html_to_vault.py)
converts one end to end: the table of contents becomes the folder-note layout,
each `<meta name=…>` a frontmatter key, internal anchors `[[wikilinks]]`, the
cross-reference block a `parent:` key plus a `## Related topics` section, and
the images are copied in beside the notes. It writes the `.kglite/vault.yaml`
for you from `--hub` / `--index` / `--embed` flags and ends by running
`okf.validate` over its own output, so a converter run that leaves the vault
broken exits non-zero. It needs `beautifulsoup4` and `markdownify`
(`requirements/examples.txt`) — neither is a kglite dependency.

## Opening a vault

`okf.build` reads every note every time. For a vault you open repeatedly —
a long-running server, a CLI you run all day, a corpus you ship to other
machines — `okf.open` is the same graph without the repetition:

```python
from kglite import okf

g = okf.open("vault", dialect="obsidian")   # builds it, and caches the graph
g = okf.open("vault", dialect="obsidian")   # loads the cache; no note is read
```

The cache is an ordinary `.kgl` at `.kglite/graph.kgl` inside the vault, so it
travels with it: commit it and the first open on a new machine costs a `stat`
of each file instead of a build. `okf.open` rebuilds when the directory has
moved on, when the cache was written by another version of kglite, when it was
built with different keywords, or when it belongs to a vault at another path —
and writes back whatever it had to build, so the next open starts from there.
Declared `embed:` targets run on the build path too, which is why a cache hit
still comes back with its vectors.

**A cache problem never fails an open.** An unreadable cache is a silent miss;
a cache that cannot be *written* — a read-only vault, a full volume, another
process mid-write — is skipped just as quietly and costs one more rebuild next
time. `cache="path/to.kgl"` puts it elsewhere (keep it *outside* the vault, or
it becomes a file of the vault and invalidates itself) and `cache=False`
switches it off.

From a terminal the same thing says which of the two happened:

```console
$ kglite okf open vault
files scanned: 412
...
rebuilt  vault/.kglite/graph.kgl
$ kglite okf open vault
loaded   vault/.kglite/graph.kgl
```

The MCP server's `--vault` mode boots through the same path, so a restart over
an untouched vault serves immediately; `--vault-cache PATH|none` is the same
two controls. VAULT.md §12 specifies the stamps the cache is validated against.

## Annotating a vault in place

A converted help corpus says things in prose that no query can reach: which
task pane opens a dialog, what a paragraph is *for*, where one topic ends and
the next begins. Four annotations state those facts **in the note itself**, in
syntax Obsidian either renders normally or hides, so the vault stays the source
of truth — nothing is rewritten, and an export writes every file back byte for
byte. They are specified in VAULT.md §5.3, §5.5 and §5.8; what follows is one
page using all four.

```markdown
---
type: Article
---
## Annotation table

To open the **Annotation table** dialog box, click the button on the
[[Wells]]{opens-dialog} task pane. #intent/annotate-wells

<!-- kglite address: Data tree -> Wells | Task pane: Wells -> Annotations table -->

<!-- kglite chunk -->

The datum is not checked when the table is imported. #warning
```

```yaml
# .kglite/vault.yaml
kglite_vault: 1
default_label: Article

tag_labels:
  "intent/*": {label: Intent, edge: HAS_INTENT}

structure:
  sections: {label: Section, edge: HAS_SECTION}
  chunks: {label: Chunk, edge: HAS_CHUNK, max_words: 120}
```

**1. A typed link — `[[Wells]]{opens-dialog}`.** The brace directly after `]]`
names that one link's edge type, above the heading it sits under and above
`heading_edges:`. It renders as an ordinary wikilink plus the literal text
`{opens-dialog}`, and it normalises the way a frontmatter key does
(`opens-dialog` → `OPENS_DIALOG`).

```python
g.cypher("MATCH (a:Article)-[:OPENS_DIALOG]->(b) RETURN a.title, b.concept_id")
```

**2. A directive property — `<!-- kglite address: … -->`.** An HTML comment on
a line of its own states a key on the **enclosing section** (on the note above
the first heading, or where no `sections:` rule is declared). A value naming
wikilinks becomes typed edges instead; anything else is a property typed the
way frontmatter is. Obsidian hides it, and it is cut out of every derived
`text` and `embed_text`, so retrieval still sees only the prose.

```python
g.cypher("MATCH (s:Section) WHERE s.address CONTAINS 'Task pane' "
         "RETURN s.title, s.address")
```

**3. A modelled tag — `#intent/annotate-wells`.** With a `tag_labels:` rule the
tag stops being a `Tag` and becomes an `Intent` node keyed on the text after
the prefix, joined from the innermost derived node that holds it — here the
chunk. Tags no rule matches are unchanged, and *every* inline tag also lands in
a `tags` list on that same node, which is what makes a one-word marker
selectable per paragraph rather than per page.

```python
g.cypher("MATCH (c:Chunk)-[:HAS_INTENT]->(i:Intent) RETURN i.title, c.text")
g.cypher("MATCH (c:Chunk) WHERE 'warning' IN c.tags RETURN c.concept_id, c.text")
```

**4. A chunk boundary — `<!-- kglite chunk -->`.** The packer fills a chunk to
`max_words` / `max_chars` and knows nothing about where a topic ends; this
marker closes the open chunk at that point. It is the fix for the caveat above:
without it the caution and the procedure pack together and `#warning` marks
both. A `^block-id` (Obsidian's own anchor) does the same and additionally
gives the chunk a stable id.

```python
g.cypher("MATCH (:Section {title:'Annotation table'})-[:HAS_CHUNK]->(c) "
         "RETURN c.ordinal, c.text ORDER BY c.ordinal")
```

`kglite okf check <dir> --strict` reports a marker that promoted nothing, a
`tag_labels:` rule no tag matched, and a directive naming a key a note or a
derived node defines itself — the three ways an annotation can be written and
silently do nothing.

A fifth annotation, `<!-- kglite heading -->`, promotes the line below it to a
heading: it is for a converter that emitted a bold signature line where a
`####` belonged, and it is documented with the rest in VAULT.md §5.8.

## Maintaining agent memory & skills

Because the result is a normal graph, "tooling for memories and skills" is just
queries — no new API:

```python
g = okf.build("~/.claude/.../memory", dialect="loose")

# Orphaned memories: no *semantic* edge (every concept has a structural
# CONTAINS from its Folder and TAGGED edges, so exclude those).
g.cypher("MATCH (n) WHERE n.concept_id IS NOT NULL "
         "OPTIONAL MATCH (n)-[r]-() WHERE NOT type(r) IN ['CONTAINS', 'TAGGED'] "
         "WITH n, count(r) AS d WHERE d = 0 RETURN n.concept_id")

# Dangling [[links]] — references to knowledge not yet written
g.cypher("MATCH (n {_provisional: true}) RETURN n.concept_id")

# Most-referenced sources, and memories grouped by tag
g.cypher("MATCH (:Concept)-[:CITES]->(s:Source) "
         "RETURN s.id, count(*) AS cited ORDER BY cited DESC")
g.cypher("MATCH (c)-[:TAGGED]->(t:Tag) RETURN t.id, collect(c.title)")

# Cluster memories into themes (the OKF → GraphRAG indexing story)
g.cypher("CALL leiden() YIELD node, community "
         "RETURN community, collect(node.title) ORDER BY community")

# Read one concept's prose once a query has narrowed to it
body = okf.source("~/.claude/.../memory/some-fact.md")
```

## API

The generated API reference documents {func}`kglite.okf.build`,
{func}`kglite.okf.open` and {func}`kglite.okf.source` from the package stubs.
`build(path, *, dialect="okf", with_body=False)` returns a
{class}`~kglite.KnowledgeGraph`; `open(path, *, cache=None, …)` returns the
same thing through a cached `.kgl` (see above). `dialect` is `"okf"` (default), `"loose"`
(wikilinks, no `type` required), or `"obsidian"` (the vault format — see
above). `source(path)` returns a concept's markdown body with the frontmatter
stripped.
