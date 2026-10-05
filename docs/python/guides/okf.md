# OKF Ingestion

You can load **Open Knowledge Format** bundles into KGLite knowledge graphs. A bundle is a directory of markdown files with YAML frontmatter, cross-linked by markdown links. This is Google's [Open Knowledge Format](https://github.com/GoogleCloudPlatform/knowledge-catalog).

The same shape fits your **Claude memory directory**, a **skills folder**, an **Obsidian vault** and a **GraphRAG corpus**: they all have the same shape.

OKF deliberately ships *no* query engine. KGLite supplies the missing half. Once a bundle is a graph, you get these over it for free:

- Cypher
- `CALL leiden` / `pagerank`
- the `orphan_node` rule
- temporal filters

> **Ingesting a repo's docs?** The same parser powers codingest's
> `include_docs=True` option, which ingests a codebase's markdown as `:Doc`
> nodes and links them to the code they describe.

The YAML parser is bundled in the wheel, so no extra is needed:

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

`build` ingests only `.md` files that have a YAML frontmatter block by default (`require_frontmatter=True`). That block separates *structured* knowledge (OKF concepts, Claude memories) from plain markdown (READMEs, notes).

You can therefore point `build` at a **parent of many projects** and extract only the structured knowledge across all of them in one sweep:

- Plain docs are skipped.
- Each project's tree becomes `Folder` nodes.
- Concept ids stay path-relative, so they don't collide.

```python
g = okf.build("~/code", dialect="loose")   # require_frontmatter=True
g.cypher("MATCH (f:Folder)-[:CONTAINS]->(m) "
         "RETURN split(m.concept_id, '/')[0] AS project, count(m) AS memories "
         "ORDER BY memories DESC")
```

Node labels fall back `type` → `metadata.type` → `Concept`. Claude memories carry `metadata.type`, not a top-level `type`, so they land as `:feedback` / `:project` / `:user` / `:reference`, with their `name` as the title. Pass `require_frontmatter=False` to ingest every `.md` (vault-style).

#### Excluding files and directories

To exclude one file from sweeps, add `kg_skip: true` to its frontmatter. `build` honors it by default; pass `respect_skip=False` to ingest skip-marked files anyway.

To exclude whole directories you don't own (cloned or vendored trees), pass `skip_dirs`. It is gitignore-style:

- A bare name matches a directory at any depth.
- A `path/with/slashes` is an anchored bundle-relative subtree.

```python
g = okf.build("~/code", skip_dirs=["node_modules", "vendor/repos", "mistral.rs"])
```

## How a bundle maps to a graph

Ingestion is **read-only and partial**. It is conceptually a code-graph build for prose instead of source code. The directory stays the source of truth, and the graph is a rebuildable lens over it.

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

`Tag`, `Source` and `Folder` nodes are synthesized by default. They turn the sparse author-link graph into a dense, well-clustering one, because the hubs connect otherwise-disconnected concepts. Disable any kind through `BuildOptions` if you want a bare concept graph.

### The edge-type ladder

OKF links are untyped, because the relationship lives in prose. `build` infers the connection type most-specific-first:

1. An explicit link **title** that looks like a type — `[customers](/tables/customers.md "JOINS_WITH")`.
2. The enclosing **section header** — `# Joins` → `JOINS_WITH`, `# Citations` → `CITES`, `# References` → `REFERENCES`.
3. The generic fallback — `LINKS_TO`.

Structural `CONTAINS` edges come in addition, from the directory hierarchy.

Link resolution is forgiving. A `[[wikilink]]` or path resolves in this order:

1. exact id
2. file stem
3. normalized slug (case- and `_`/`-`-insensitive)
4. title

So `[[my-note]]`, `[[My Note]]`, and `my_note.md` all reach the same concept.

## Obsidian vaults

`dialect="obsidian"` is its own contract, not a synonym for `"loose"`. It differs in these ways:

- Labels come from the top-level folder (or a declared `default_label`).
- Ids are filename stems, so a folder move keeps a note's identity.
- Bodies are stored.
- Frontmatter lists stay lists.
- Wikilink-valued frontmatter keys become typed edges.
- A note's `aliases:` answer link resolution.
- Every body link carries its enclosing heading as a `section` edge property.
- Inline `#tags` join the same `Tag` hub as `tags:`, case-insensitively as in Obsidian: `#Seismic` and `#seismic` are one tag.
- Obsidian's own `cssclasses:` is ignored.
- Folder notes build a hierarchy.
- Referenced images become `Image` nodes you fetch as files, rather than bytes in the graph.

A `.kglite/vault.yaml` in the vault root declares property types, indexes, text indexes, an ontology and embed targets. KGLite re-applies it on every rebuild.

### The structure profile

A note is one node with one body until `.kglite/vault.yaml` declares a `structure:` block. Then the note's own markdown becomes nodes too:

- Headings are `Section`s with parent and next edges.
- Paragraphs pack into `Chunk`s you embed and search.
- Callouts become `Note`s.
- Fenced blocks become `Example`s.
- Numbered lists become `Procedure`/`ProcedureStep` chains.
- A table under a declared heading becomes one node per row. With `edges: true`, it becomes one *edge* per row, carrying the other columns as edge properties.

The ids are Obsidian's own, `[[Page#Heading]]` and `[[Page#^block]]`, so every derived node stays linkable from any note and navigable in Obsidian.

Four more keys:

- `key_from_heading:` relabels in place a heading that is really a symbol name.
- `inherit:` copies the note's own properties onto every derived node.
- `embed_text:` renders the string those nodes are embedded and searched on.
- `edge_defaults:` stamps a constant property on every edge of a type.

Nothing is rewritten and no file is added. The block is opt-in, and a vault without one builds exactly as before.

VAULT.md §7.1 specifies the keys. §13 is the modelling guide for a converter deciding what to emit. It includes the loop to convert by:

1. Convert a fifty-page sample.
2. Run `kglite okf check <dir> --strict`.
3. Read the per-label counts against what the sample holds.
4. Fix the converter, not the vault.

### The vault format and converters

[VAULT.md](https://kglite.readthedocs.io/en/latest/reference/vault-format.html) specifies the format. It is also the checklist to follow when you write a converter from HTML or any other source into a vault. Check a vault with `kglite okf check <dir>`.

Two further guides:

- For a portable design guide that does not require KGLite, start with [Knowledge Bases](https://github.com/kkollsga/kglite/blob/main/KNOWLEDGE_BASES.md).
- For a complete runnable conversion, reconciliation, serving and sharing workflow, follow {doc}`help-vault`.

### Converting HTML help into a vault

A vendor help corpus is the case the format was shaped around. It has thousands of HTML pages, a JSON table of contents, `<meta>` metadata and an image directory.

[`examples/html_to_vault.py`](https://github.com/kkollsga/kglite/blob/main/examples/html_to_vault.py) converts one end to end:

- The table of contents becomes the folder-note layout.
- Each `<meta name=…>` becomes a frontmatter key.
- Internal anchors become `[[wikilinks]]`.
- The cross-reference block becomes a `parent:` key plus a `## Related topics` section.
- The images are copied in beside the notes.

The script writes the `.kglite/vault.yaml` for you from `--hub` / `--index` / `--embed` flags. It ends by running `okf.validate` over its own output, so a converter run that leaves the vault broken exits non-zero. It needs `beautifulsoup4` and `markdownify` (`requirements/examples.txt`); neither is a kglite dependency.

## Opening a vault

`okf.build` reads every note every time. For a vault you open repeatedly, `okf.open` gives the same graph without the repetition. Typical cases are a long-running server, a CLI you run all day, and a corpus you ship to other machines.

```python
from kglite import okf

g = okf.open("vault", dialect="obsidian")   # builds it, and caches the graph
g = okf.open("vault", dialect="obsidian")   # loads the cache; no note is read
```

The default cache is an ordinary `.kgl` at `.kglite/graph.kgl` inside the vault. It may travel with a vault as an optional accelerator. It is stamped with the canonical source root, so a vault copied or moved to another path rebuilds once. Subsequent unchanged opens then reuse the refreshed cache when it is writable.

`okf.open` rebuilds when any of these holds:

- The directory contents have changed.
- Another version of kglite wrote the cache.
- The cache was built with different keywords.
- The cache belongs to a vault at another path.

It writes back whatever it had to build, so the next open starts from there. Declared `embed:` targets run on the build path too, which is why a cache hit still comes back with its vectors.

**A cache problem never fails an open.** An unreadable cache is a silent miss. A cache that cannot be *written* is skipped just as quietly and costs one more rebuild next time. Examples are a read-only vault, a full volume, or another process mid-write.

Two keywords control the cache:

- `cache="path/to.kgl"` puts it elsewhere. An outside path is recommended when the source tree should stay clean or read-only. Keep it *outside* the vault, or it becomes a file of the vault and invalidates itself.
- `cache=False` switches it off.

From a terminal the same command says which of the two happened:

```console
$ kglite okf open vault
files scanned: 412
...
rebuilt  vault/.kglite/graph.kgl
$ kglite okf open vault
loaded   vault/.kglite/graph.kgl
```

The MCP server's `--vault` mode boots through the same path, so a restart over an untouched vault serves immediately. `--vault-cache PATH|none` gives the same two controls. VAULT.md §12 specifies the stamps the cache is validated against.

## Annotating a vault in place

A converted help corpus says things in prose that no query can reach. Examples are which task pane opens a dialog, what a paragraph is *for*, and where one topic ends and the next begins.

Four annotations state those facts **in the note itself**, in syntax Obsidian either renders normally or hides. The vault stays the source of truth: nothing is rewritten, and an export writes every file back byte for byte. VAULT.md §5.3, §5.5 and §5.8 specify them. What follows is one page using all four.

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

### 1. A typed link

`[[Wells]]{opens-dialog}`: the brace directly after `]]` names that one link's edge type. It takes precedence over the heading the link sits under and over `heading_edges:`.

It renders as an ordinary wikilink plus the literal text `{opens-dialog}`. It normalises the way a frontmatter key does (`opens-dialog` → `OPENS_DIALOG`).

```python
g.cypher("MATCH (a:Article)-[:OPENS_DIALOG]->(b) RETURN a.title, b.concept_id")
```

### 2. A directive property

`<!-- kglite address: … -->` is an HTML comment on a line of its own. It states a key on the **enclosing section**, or on the note when the line is above the first heading or no `sections:` rule is declared.

The value decides what the key becomes:

- A value naming wikilinks becomes typed edges.
- Anything else is a property, typed the way frontmatter is.

Obsidian hides the comment. KGLite cuts it out of every derived `text` and `embed_text`, so retrieval still sees only the prose.

```python
g.cypher("MATCH (s:Section) WHERE s.address CONTAINS 'Task pane' "
         "RETURN s.title, s.address")
```

### 3. A modelled tag

`#intent/annotate-wells`: with a `tag_labels:` rule, the tag stops being a `Tag`. It becomes an `Intent` node keyed on the text after the prefix. The node joins from the innermost derived node that holds the tag, here the chunk.

Tags no rule matches are unchanged. *Every* inline tag also lands in a `tags` list on that same node. That list makes a one-word marker selectable per paragraph rather than per page.

```python
g.cypher("MATCH (c:Chunk)-[:HAS_INTENT]->(i:Intent) RETURN i.title, c.text")
g.cypher("MATCH (c:Chunk) WHERE 'warning' IN c.tags RETURN c.concept_id, c.text")
```

### 4. A chunk boundary

`<!-- kglite chunk -->` closes the open chunk at that point. The packer fills a chunk to `max_words` / `max_chars` and knows nothing about where a topic ends, so the marker supplies that.

It is the fix for the caveat above. Without it, the caution and the procedure pack together and `#warning` marks both. A `^block-id` (Obsidian's own anchor) does the same and additionally gives the chunk a stable id.

```python
g.cypher("MATCH (:Section {title:'Annotation table'})-[:HAS_CHUNK]->(c) "
         "RETURN c.ordinal, c.text ORDER BY c.ordinal")
```

### Checking annotations

`kglite okf check <dir> --strict` reports the three ways an annotation can be written and silently do nothing:

- A marker that promoted nothing.
- A `tag_labels:` rule no tag matched.
- A directive naming a key that a note or a derived node defines itself.

### A fifth annotation

`<!-- kglite heading -->` promotes the line below it to a heading. It is for a converter that emitted a bold signature line where a `####` belonged. VAULT.md §5.8 documents it with the rest.

## Maintaining agent memory & skills

The result is a normal graph, so tooling for memories and skills is just queries, with no new API:

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

The generated API reference documents {func}`kglite.okf.build`, {func}`kglite.okf.open` and {func}`kglite.okf.source` from the package stubs.

| Function | Returns |
|---|---|
| `build(path, *, dialect="okf", with_body=False)` | A {class}`~kglite.KnowledgeGraph` |
| `open(path, *, cache=None, …)` | The same thing through a cached `.kgl` (see above) |
| `source(path)` | A concept's markdown body with the frontmatter stripped |

`dialect` takes one of three values:

- `"okf"` (default).
- `"loose"`: wikilinks, no `type` required.
- `"obsidian"`: the vault format (see above).
