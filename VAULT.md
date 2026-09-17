# KGLite Vault Format

The normative specification of the **vault** format — a directory of
frontmatter-markdown notes that KGLite loads as a knowledge graph, and writes
back out again. It is the Obsidian convention as KGLite reads it, and it is
the format a converter (HTML, XML, a CMS export) should target.

Load a vault with `okf.build(path, dialect="obsidian")` in Python, or
`kglite::okf::build` in Rust. Check one with `kglite okf check <dir>`.

Everything below is normative for the `"obsidian"` dialect. The `"okf"` and
`"loose"` dialects are a different contract and are unchanged by this document;
where they differ, this spec says so.

## 1. Scope and versioning

1. A **vault** is a directory tree of UTF-8 markdown files. The files are
   canonical; the graph is a derived, rebuildable lens over them. A build never
   writes into the vault — writing markdown *from* a graph is a separate,
   explicit operation (§10). The dialect string is exactly `"obsidian"`, and it
   is not an alias for `"loose"`.
2. Only `.md` files become notes. A non-`.md` file becomes a node only when a
   note references it (§6); unreferenced files are not in the graph.
3. The format version is `kglite_vault:` in `.kglite/vault.yaml`. The current
   version is `1`, and so is a vault with no such file or key. Any other value
   is an error.
4. Never interpreted: HTML, canvas files, Dataview inline fields
   (`key:: value`), Logseq properties, and heading-level splitting of a note.

## 2. Directory layout and labels

### 2.1 Label

Each note gets exactly one primary label, chosen by the first rung that yields
a non-empty value:

1. frontmatter `type:`
2. `default_label:` from `.kglite/vault.yaml`
3. the note's **top-level folder name**, verbatim — no singularising, no case
   change
4. `Note`

Setting `label_from: folder` moves rung 3 to the front, so a folder move
relabels a note even when it carries a `type:`. A note directly in the vault
root has no top-level folder and falls through to rung 4 (or rung 2).

### 2.2 Folders

Every directory becomes a `Folder` node with `CONTAINS` edges to the notes and
subfolders inside it (opt-out at build time). A vault's first folder level is
therefore both a label and a `Folder` node; the redundancy is intentional.

### 2.3 Folder notes

A **folder note** is `X.md` beside a directory `X/`, or `X/X.md`. The note
takes the folder's place: no `Folder` node is created for `X/`, and the notes
inside `X/` are joined to the folder note by the edge declared in
`folder_notes:` — by default `CHILD_OF`, directed child → parent:

```
Geology.md            # the folder note for Geology/
Geology/
  Faults.md           # (:Faults)-[:CHILD_OF]->(:Geology)
  Horizons.md
  Horizons/
    Autotracking.md   # (:Autotracking)-[:CHILD_OF]->(:Horizons)
```

This is how a table-of-contents hierarchy is expressed on disk. Further
parents are declared with `parent:` (§4.3).

### 2.4 Ignored paths

Pruned from the walk, with their whole subtree:

- any directory whose name begins with `.` — `.obsidian`, `.trash`, `.git`,
  and `.kglite` itself
- `node_modules`, `target`, `__pycache__`, `venv`, `env`, `site-packages`
- anything matched by `skip_dirs`: a bare name matches a directory at any
  depth, an entry containing `/` is an anchored vault-relative subtree

`.kglite/` is read by explicit path (§7, §8), never by the walk.

A single file opts out with `kg_skip: true` in its frontmatter.

`index.md` and `log.md` are **ordinary notes** in this dialect. (In the `okf`
and `loose` dialects `index.md` is folder metadata and `log.md` is skipped.)

Frontmatter is not required: a plain `.md` file with no `---` block is a note.

## 3. Identity

| What | Rule |
|---|---|
| **id** | frontmatter `id:` → the filename stem |
| **title** | frontmatter `title:` → frontmatter `name:` → the first `# H1` in the body → the filename stem |
| **file path** | vault-relative, forward-slashed, stored as `file_path` |

The id is the node's `concept_id` property and the target of every link. Ids
are deliberately **not** path-derived: moving a note between folders must not
change its identity, because embedding carry across a rebuild, `.kgl`
snapshots and external references all key on it.

**Stem collisions.** When two or more notes share a stem and neither declares
an `id:`, every colliding note falls back to its vault-relative path minus
`.md` (`projects/alpha`, `archive/alpha`), and the collision is reported as an
error. Notes that do not collide keep their stems. The same fallback settles
two notes that declare the *same* `id:`, and a fallback that collides in turn
(a note declaring `id: archive/alpha` while `archive/alpha.md` exists) — the
alternative is merging two notes into one node, which loses one of them
silently.

**Case-insensitive collisions.** `Foo.md` and `foo.md` in one directory, or two
ids differing only in case, are reported as a warning on every host — a vault
built on Linux must still open on macOS and Windows.

## 4. Frontmatter

A frontmatter block is YAML 1.2 between `---` fences, starting at the first
byte of the file. Unparseable YAML is an error; the note still becomes a node
with no frontmatter properties, so one bad file never costs the rest.

### 4.1 Reserved keys

| Key | Meaning |
|---|---|
| `id` | The node id (§3). Not stored as a property. |
| `type` | The label (§2.1). Not stored as a property. |
| `title` | The display title (§3). Stored as `title`. |
| `aliases` | Alternative names this note answers to in link resolution (§5.2). Stored as a list property. |
| `tags` | Tag hub membership (§5.5). Stored as a list property. |
| `kg_skip` | `true` excludes the file from the build. |
| `parent` | Additional parents (§4.3). Not stored as a property. |

Every other key becomes a node property, or edges under the rule in §4.3.

### 4.2 Value typing

- Scalars keep their YAML type: string, integer, float, boolean.
- Sequences become **native list properties** (`tags: [a, b]` is a list, not a
  JSON string). This is dialect-specific: `okf` and `loose` keep JSON strings.
- Nested maps flatten to dotted keys: `metadata: {source: vendor}` becomes the
  property `metadata.source`.
- A **top-level** string matching `YYYY-MM-DD` becomes a **date**; one matching
  RFC 3339 becomes a **datetime**, normalised to UTC. A value that must stay
  text despite matching is declared `string` in `types:` (§7) — quoting alone
  does not stop inference. Inference does not reach inside a sequence: a tag
  literally named `2026-01-15` stays the string the tag hub needs.
- `types:` overrides inference per label and property. Declared beats inferred.
- The note's prose is stored under the property named by `body:` (default
  `body`), verbatim, frontmatter stripped.

### 4.3 The typed-edge rule

A frontmatter key whose value is a **wikilink string**, or a list in which
**every** element is a wikilink string, becomes edges instead of a property:

```yaml
---
depends_on: "[[Seismic interpretation]]"
see_also: ["[[Faults]]", "[[Horizons]]"]
---
```

produces `DEPENDS_ON` and `SEE_ALSO` edges from this note to those targets. The
edge type is `UPPER_SNAKE(key)`. The raw property is **not** stored — keeping
both would duplicate it on export.

A list mixing wikilinks and plain strings stays an ordinary list property — the
rule never splits a key. Targets resolve as body links do (§5.2); an unresolved
one becomes a stub (§5.6).

`parent:` is the one reserved key that follows this rule with a fixed edge
type: it emits the `folder_notes.edge` type in the `folder_notes.direction`,
so a note cross-listed under several parents contributes the same edges as the
folder layout would have.

## 5. Links

### 5.1 Syntax

| Written | Meaning |
|---|---|
| `[[Note]]` | Link to `Note`. |
| `[[Note\|display text]]` | Same link; the display text is not stored. |
| `[[Note#Heading]]`, `[[Note#^block-id]]` | Link to `Note`, `anchor` = the fragment. |
| `[[Label/Name]]` | Folder-qualified — use when a stem is ambiguous. |
| `[text](path.md)`, `[text](path.md "EDGE_TYPE")` | Path link, resolved relative to the linking note; the title is an explicit edge type. |
| `![[Note]]` | Embed → an `EMBEDS` edge. |
| `![[image.png]]`, `![alt](img/x.png)` | Attachment (§6), never a note link. |
| `https://…` | An external `Source` node keyed by the URL. |

Links inside fenced code blocks (``` or `~~~`) are ignored.

An embed is read as a note when its target has no file extension or ends in
`.md`, and as an attachment otherwise — so a note whose *filename* contains a
dot is embedded as `![[Release 1.2.md]]`, with the extension written out.

### 5.2 Resolution ladder

A link target resolves against the first rung that matches exactly one note:

1. an exact id
2. a filename stem
3. an entry in some note's `aliases:`
4. a normalized slug — case-insensitive, `_` and `-` equivalent
5. a title
6. otherwise: a stub (§5.6)

A target containing `/` is tried first as a vault-relative id, then as a path
relative to the linking note. A trailing `.md` is stripped first.

### 5.3 Edge type

For a body link, the edge type is the first that applies:

1. an explicit link title that looks like a type — `[x](y.md "JOINS_WITH")`
2. an entry in `heading_edges:` matching the enclosing heading text exactly
3. the built-in heading ladder, matched case-insensitively as a substring of
   the enclosing heading: *citation* → `CITES`, *join* → `JOINS_WITH`,
   *reference* → `REFERENCES`, *related* → `RELATED`, *depend* → `DEPENDS_ON`
4. `LINKS_TO`

`heading_edges:` wins over the built-in ladder — which is why a corpus writes
`heading_edges: {"Related topics": RELATED_TO}` instead of accepting `RELATED`.

### 5.4 Edge properties

Every body link carries `section`, the enclosing heading's text verbatim
(absent above the first heading). A fragment link also carries `anchor`, the
fragment without its leading `#`. The fragment never affects resolution:
`[[Note#Heading]]` and `[[Note]]` reach the same node.

Two links from one note to one target are **two edges** when they differ in
`section` or `anchor`, and one when they do not — repeating a link inside a
section is one relationship, linking from two sections is two. Edges emitted
from frontmatter (§4.3) carry neither property.

### 5.5 Tags

Both forms feed one `Tag` hub per distinct tag, joined by `TAGGED`:

- `tags:` in frontmatter, which also stays a list property on the note
- inline `#tag` in the body

Inline extraction skips fenced code, inline code spans, a `#` inside a URL or a
wikilink anchor, and a `#` that begins a line and is followed by a space (that
is a heading — though a `#tag` written *in* the heading's text is still a tag).
A tag name runs over letters, digits, `_`, `-` and `/`, and must contain at
least one letter: `#2026` is not a tag.

### 5.6 Unresolved targets

An unresolved link target becomes a `_provisional: true` stub node, labelled
`Concept` and keyed by the unresolved name, so "referenced but not written" is
one Cypher query and the stubs never mix with the notes' own labels. Stubs
are counted in the build report and are never exported as files (§10).

## 6. Attachments

1. **Accepted syntax:** `![alt](rel/path.png)`, `![[image.png]]` and
   `![[image.png|alt]]`. The body keeps the original syntax verbatim.
2. **Resolution ladder:** note-relative → vault-root-relative → a unique
   filename anywhere in the vault. The stored value is always the
   **vault-relative** resolved path, so every consumer resolves from one root.
   A filename occurring twice does not resolve on the third rung; qualify it.
3. **Nodes:** one node per distinct resolved file, labelled `Image` when the
   extension maps to an image MIME type and `Attachment` otherwise. The id is
   the vault-relative path, repeated as `path`; `mime` comes from the extension
   table, `size_bytes` and `mtime` (a UTC datetime) from `stat`. An `Image`
   also carries `text`: the distinct alt texts and the titles of the notes
   using it, newline-separated in first-use order, so captions stay
   text-searchable.
4. **Edges:** `HAS_IMAGE` or `HAS_ATTACHMENT` from the note, with edge
   properties `alt` (the alt text, if any), `section` (enclosing heading) and
   `ordinal` (0-based position among that note's references of the same kind).
5. **Bytes are never read at build time.** Metadata comes from `stat` only: no
   content hash, no dimensions, so build cost is independent of image volume.
6. **A missing target** becomes a `_provisional: true` node with
   `missing: true`, counted in the build report.

**Recommendation for converters: emit PNG, JPEG, GIF or WebP** — the four types
the bundled MCP server delivers as images. SVG is stored as an `Attachment` and
is not delivered; rasterise it when you build the source.

## 7. `.kglite/vault.yaml`

Optional. Read by explicit path (the walk ignores dot-directories), applied to
every build and re-applied to every rebuild — with carried embeddings, it is
the only state that survives one. An unknown top-level key, or a value of the
wrong shape, is an error.

| Key | Shape | Meaning |
|---|---|---|
| `kglite_vault` | integer | Format version. `1`. |
| `default_label` | string | Label for notes with no `type:` (§2.1, rung 2). |
| `label_from` | `type` \| `folder` | `type` (default) uses the ladder in §2.1; `folder` puts the folder rung first. |
| `body` | string | Property name for the prose. Default `body`. |
| `skip_dirs` | list of strings | Extra directories to prune (§2.4). |
| `folder_notes` | `{edge, direction}` | `edge` default `CHILD_OF`; `direction` is `child_to_parent` (default) or `parent_to_child`. |
| `hubs` | `{<frontmatter key>: {label, edge, case_insensitive}}` | Turn a list-valued key into hub nodes. `case_insensitive: true` folds the id and displays the most frequent casing as the title. |
| `heading_edges` | `{<heading text>: EDGE_TYPE}` | Merged over the built-in ladder (§5.3). |
| `types` | `{<Label>: {<property>: <type>}}` | Declared property types: `string`, `int`, `float`, `bool`, `date`, `datetime`, `list`. Overrides inference. |
| `indexes` | `{<Label>: [ <prop> \| {range: <prop>} \| {composite: [<prop>, …]} ]}` | Equality, range and composite index declarations. |
| `text_indexes` | `{<Label>: [<prop>]}` | BM25 lexical indexes. |
| `ontology` | mapping | Passed verbatim to the ontology declaration API — same document `define_ontology` accepts; see the [ontology guide](https://kglite.readthedocs.io/en/latest/python/guides/ontology.html). |
| `embed` | `{<Label>: <prop>}` | Which text property to embed per label. Reported as a build target; the vectors are computed when an embedder is bound. |

A complete example — a vendor help corpus of ~7k articles:

```yaml
# .kglite/vault.yaml
kglite_vault: 1
default_label: Article
body: body

folder_notes:
  edge: CHILD_OF
  direction: child_to_parent

hubs:
  keywords: {label: Keyword, edge: HAS_KEYWORD, case_insensitive: true}
  component: {label: Component, edge: USES_COMPONENT, case_insensitive: true}

heading_edges:
  "Related topics": RELATED_TO

types:
  Article: {description: string, toc_depth: int, updated: date}

indexes:
  Article:
    - concept_id
    - title
    - {range: toc_depth}
  Keyword: [concept_id]
  Component: [concept_id]

text_indexes:
  Article: [body]

embed:
  Article: description
```

## 8. Skills and recipes carried in the vault

A vault can carry its own agent guidance, so a server built from it explains
how to query itself. Both directories are re-read on every build, so editing a
file is the whole update procedure.

- `.kglite/skills/*.md` become `KgliteSkill` nodes. The frontmatter dialect is
  exactly the one an MCP skills directory uses — `name`, `description`,
  `references_tools`, `delivery`, then the markdown body. See
  [Authoring MCP skills](https://kglite.readthedocs.io/en/latest/python/guides/mcp-skills.html).
- `.kglite/recipes/*.md` become `KgliteRecipe` nodes — one stored Cypher query
  per file. Frontmatter carries `recipe` (the group id), `name`, `description`,
  optional `recipe_description` (what the group is for) and optional
  `parameters` (the JSON Schema for the query's `$parameters`, as a nested
  map). The body is the statement, in a fenced ` ```cypher ` block:

  ````markdown
  ---
  recipe: help
  name: children_of
  description: The articles directly below one article in the TOC.
  recipe_description: Navigating the help hierarchy.
  parameters:
    {type: object, properties: {id: {type: string}},
     required: [id], additionalProperties: false}
  ---

  ```cypher
  MATCH (c:Article)-[:CHILD_OF]->(p:Article {concept_id: $id})
  RETURN c.concept_id AS id, c.title AS title ORDER BY title LIMIT 50
  ```
  ````

  The statement must parse and must be read-only. A file that fails validation
  is skipped with a warning naming the file and the rule; its siblings load.

## 9. Build report and validation

Every build produces a structured report. `okf.validate(path)` returns it
without keeping the graph; `kglite okf check <dir>` prints it and sets the exit
code. It carries: files walked, files skipped and why, notes per label, edges
per type, hub and attachment counts, dangling links, missing attachments, id
collisions, case-insensitive collisions, unparseable frontmatter, and the
`embed:` targets declared in `vault.yaml`.

Findings are classified, and the classification is the contract:

**Errors** — a vault with any of these does not meet this spec: unparseable
frontmatter; misuse of a reserved key (§4.1), such as a non-string `id:` or a
scalar `tags:`; id collisions (§3); `.kglite/vault.yaml` schema errors,
including an unknown `kglite_vault` version; an absolute path, or a path
escaping the vault root, in a link or attachment reference.

**Warnings** — legitimate in a real vault, worth seeing: dangling links
(stubs), missing attachments, case-insensitive collisions, and alias clashes
(an `aliases:` entry that is another note's filename stem, or that two notes
both claim — the link resolves to exactly one of them).

`kglite okf check` exits non-zero when any error is present. `--strict`
promotes every warning to an error — the setting a converter's own test suite
should use.

## 10. Export

Export writes a vault from a graph: `okf.export(graph, dir)` in Python,
`kglite okf export` from the CLI. It targets this format only.

1. **Which nodes.** Every node except the synthesized ones: `Tag`, `Source`,
   `Folder`, `Image`, `Attachment` and anything `_provisional` is skipped —
   they regenerate on the next import.
2. **File path.** The node's `file_path` is preserved when it has one and its
   top-level folder still matches its label; otherwise `<Label>/<title or
   id>.md`. `/ \ : * ? " < > |` and control characters are replaced; a
   case-insensitive path collision appends `-<id>`.
3. **Frontmatter.** `type:` is never emitted — the folder carries the label, so
   emitting it would make a later folder move a no-op. `id:` is emitted only
   when the id differs from the filename stem. Keys are sorted; dotted keys
   expand back into nested maps; lists become YAML sequences; dates and
   datetimes become ISO strings; points become WKT; embeddings are never
   emitted.
4. **Quoting.** A string that would parse back as an integer, float, boolean,
   date, datetime or wikilink is quoted. `[[A]]` unquoted is a YAML flow
   sequence, and a round trip must not change a value's type.
5. **Body.** The `body` property verbatim. A node without one produces a
   frontmatter-only file. Human-owned prose is never rewritten.
6. **Edges** become frontmatter lists keyed `lower_snake(TYPE)`, with wikilink
   values: `depends_on: ["[[Seismic interpretation]]"]`. An edge already
   present in the body as a link or embed is not duplicated. `CONTAINS`,
   `TAGGED`, `HAS_IMAGE` and `HAS_ATTACHMENT` are structural and are not
   emitted. An ambiguous target is written folder-qualified,
   `[[Label/Name]]`.
7. **Overwrite safety.** `.kglite/export-manifest.json` records every file the
   export wrote: `{"kglite_vault": 1, "files": {"<vault-relative path>":
   "<sha256 hex>"}}`. On the next export a file whose current hash differs from
   its manifest entry was edited by a human, and the export refuses unless
   `force` is set. A file absent from the manifest is never overwritten and
   never deleted; a node deleted from the graph removes its file only when the
   manifest owns it.
8. **Determinism.** Frontmatter keys sorted, edge lists sorted, file order
   stable: exporting the same graph twice is byte-identical.
9. **Documented losses.** Edge properties (`section`, `anchor`, `alt`,
   `ordinal`) are dropped and counted in the export report. Attachment bytes
   are copied only when the graph knows its source root; otherwise the
   references are reported as unresolvable. Synthesized nodes are not files.
   Round-tripping is defined against exactly those losses: importing an
   exported vault reproduces the imported graph apart from them, and exporting
   an imported vault twice is byte-identical.

## 11. Converter checklist

What a converter must emit, in order:

1. **One `.md` file per source document**, UTF-8, under a directory tree that
   mirrors the hierarchy you want. Use the folder-note layout (§2.3):
   `X.md` beside `X/`.
2. **A stable `id:`** whenever the source has a durable identifier (a GUID, an
   accession number). Without one the filename stem is the id — fine for a
   hand-kept vault, fragile for a generated one.
3. **A `title:`**, unless the filename stem is already the title.
4. **Properties as frontmatter keys**, typed naturally: numbers unquoted, lists
   as YAML sequences, dates as `YYYY-MM-DD`. Do not JSON-encode anything.
5. **Typed edges as wikilink-valued keys** (§4.3), and extra parents as
   `parent:`. Do not invent a property that duplicates an edge.
6. **Body links as `[[Stem]]`**, folder-qualified `[[Label/Stem]]` when a stem
   is ambiguous. Group links under a heading and map that heading in
   `heading_edges:` when they all mean one relationship.
7. **Images as note-relative or vault-relative references**, and **copy the
   files into the vault** — a reference that resolved in the source tree does
   not after you reorganise the output. Convert to PNG, JPEG, GIF or WebP (§6).
8. **`.kglite/vault.yaml`** with `kglite_vault: 1`, your `default_label`,
   `folder_notes`, `hubs`, `heading_edges`, `types`, `indexes`, `text_indexes`
   and `embed` (§7). It replaces the graph-building script: everything
   declarative lives here and is re-applied on every rebuild.
9. **`.kglite/skills/` and `.kglite/recipes/`** when the vault is served to an
   agent (§8).
10. **Run `kglite okf check <dir>`.** Zero errors is the bar; add `--strict`
    to your own test suite once the warnings are down to the ones you accept.
