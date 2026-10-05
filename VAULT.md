# KGLite Vault Format

This is the normative specification of the **vault** format. A vault is a directory of frontmatter-markdown notes. KGLite loads it as a knowledge graph and writes it back out again. The format is the Obsidian convention as KGLite reads it. A converter (HTML, XML, a CMS export) should target it.

Load a vault with `okf.build(path, dialect="obsidian")` in Python, or
`kglite::okf::build` in Rust. Check one with `kglite okf check <dir>`.

Sections 1–10 define the **format requirements** for the `"obsidian"` dialect.
Section 11 is a **recommended converter checklist**. §12 describes
**operational behavior** for rebuilds and caches. §13 gives **recommended
modelling conventions**. A vault need not follow the recommendations to be a
valid vault. The `"okf"` and `"loose"` dialects are a different contract and
are unchanged by this document. Where they differ, this spec says so.

Choose the shortest route for the job:

- author one note: start with the quick reference below, then use §§2–6;
- look up exact syntax or precedence: use §§2–10;
- convert a corpus: use §§11 and 13, then the
  [worked help-vault tutorial](https://kglite.readthedocs.io/en/latest/python/guides/help-vault.html);
- design a portable knowledge base without depending on KGLite: start with
  [Knowledge Bases](https://github.com/kkollsga/kglite/blob/main/KNOWLEDGE_BASES.md), then use this document when targeting
  the KGLite vault format;
- serve, update or share a vault: use §12 and the worked tutorial.

## Quick reference for authors

A minimal valid note, `Geology/Faults.md`:

```markdown
---
title: Fault interpretation
depends_on: "[[Horizons]]"
---
Picked on the 2024 survey. See [[Horizons]] for the surfaces.

![Fault map](img/faults.png)
```

A minimal `.kglite/vault.yaml` (optional — a vault without one is still valid):

```yaml
kglite_vault: 1
default_label: Note
```

Five rules decide what a hand-written note becomes:

1. **The folder is the label.** `Geology/Faults.md` is a `:Geology` node unless
   its frontmatter says `type:` (§2.1).
2. **The filename stem is the id, and the link target.** `Faults.md` is reached
   as `[[Faults]]` from anywhere in the vault. A `id:` in frontmatter overrides
   it (§3).
3. **A wikilink-valued key is an edge, not a property.**
   `depends_on: "[[Horizons]]"` makes a `DEPENDS_ON` edge and stores nothing
   (§4.3).
4. **Images are note-relative and live in the vault.** `![alt](img/x.png)`
   becomes an `Image` node. Copy the file in. A path out of the vault is an
   error (§6).
5. **The body is prose.** KGLite stores it whole and searches it. Nothing in it
   is ever rewritten or reformatted (§4.2). A body is *split* into nodes of its
   own (sections, chunks, callouts, steps, table rows) only where
   `.kglite/vault.yaml` declares a `structure:` block (§7.1). With no such block
   a note is one node holding one body.

Then run `kglite okf check <dir>`.

- **Errors** mean the vault does not meet this spec.
- **Warnings** (a dangling link, a missing image) are normal in a vault being
  written.
- `--strict` fails on warnings too (§9).

**Writing a converter, or authoring for one?** §13 is the modelling guide. It
lists what to emit for each shape a source already has. It also lists the
anti-patterns that cost a corpus its structure on the way in.

## 1. Scope and versioning

1. A **vault** is a directory tree of UTF-8 markdown files. The files are
   canonical. The graph is a derived, rebuildable lens over them. A build never
   writes into the vault. Writing markdown *from* a graph is a separate,
   explicit operation (§10). The dialect string is exactly `"obsidian"`, and it
   is not an alias for `"loose"`.
2. Only `.md` files become notes. A non-`.md` file becomes a node only when a
   note references it (§6). Unreferenced files are not in the graph.
3. The format version is `kglite_vault:` in `.kglite/vault.yaml`. The current
   version is `1`, and so is a vault with no such file or key. Any other value
   is an error.
4. These are never interpreted:
   - HTML **tags**, so an `<a href>` is not a link and an `<img src>` is not an
     attachment reference;
   - canvas files;
   - Dataview inline fields (`key:: value`);
   - Logseq properties.

   Heading-level splitting is not done either, unless the vault asks for it. A
   `structure:` block in `.kglite/vault.yaml` (§7.1) derives nodes from a note's
   own headings, paragraphs, callouts, lists, fences and tables. Without one, a
   note stays a single node holding a single body.

   A line holding HTML is still prose. The markdown link and image syntax
   written *inside* an HTML block is scanned exactly as it is anywhere else
   (§5.1). Nothing here is a block-level exemption, because the reader has no
   HTML parser to give it one.

## 2. Directory layout and labels

### 2.1 Label

Each note gets exactly one primary label. The first rung that yields a
non-empty value decides it:

1. frontmatter `type:`
2. `default_label:` from `.kglite/vault.yaml`
3. the note's **top-level folder name**, verbatim — no singularising, no case
   change
4. `Note`

If you set `label_from: folder`, rung 3 moves to the front. A folder move then
relabels a note even when it carries a `type:`. A note directly in the vault
root has no top-level folder, so it falls through to rung 4 (or rung 2).

### 2.2 Folders

Every directory becomes a `Folder` node. It has `CONTAINS` edges to the notes
and subfolders inside it (opt-out at build time). A vault's first folder level
is therefore both a label and a `Folder` node. The redundancy is intentional.

### 2.3 Folder notes

A **folder note** is `X.md` beside a directory `X/`, or `X/X.md`. The note
takes the folder's place, and no `Folder` node is created for `X/`. The edge
declared in `folder_notes:` joins the notes inside `X/` to the folder note. By
default that edge is `CHILD_OF`, directed child → parent:

```
Geology.md            # the folder note for Geology/
Geology/
  Faults.md           # (:Faults)-[:CHILD_OF]->(:Geology)
  Horizons.md
  Horizons/
    Autotracking.md   # (:Autotracking)-[:CHILD_OF]->(:Horizons)
```

This is how a table-of-contents hierarchy is expressed on disk. You declare
further parents with `parent:` (§4.3). If a `parent:` names the folder note the
layout already joined this note to, it is the same relationship and produces one
edge.

#### Labelling a folder note

A folder note is **labelled from where its folder sits**, not from inside it.
`X/X.md` takes the label rung 3 (§2.1) gives its directory's *parent*, so both
spellings of one folder note produce the same label. `Geology.md` and
`Geology/Geology.md` at the vault root are therefore both `Note`, not
`Geology`.

Converters often miss this, so here it is as a tree. A corpus split into two
top-level sections writes

```
Api.md                # the folder note for Api/
Api/
  rmsapi.md           # (:Api)
Software.md           # the folder note for Software/
Software/
  panels.md           # (:Software)
```

The notes *inside* the sections get `:Api` and `:Software`. `Api.md` and
`Software.md` themselves get `Note`. They sit at the vault root, which has no
top-level folder for rung 3 to read. The ladder is doing what it says. The fix
is one line per root: `type: Api` in `Api.md`. `default_label:` is not the fix,
because rung 2 sits ahead of rung 3 and would relabel every note in the vault.

#### What the folder-note edge joins

The folder-note edge joins **notes**. A plain subdirectory below a folder note
keeps its `Folder` node and its `CONTAINS` edge from the note. The note stands
in for a directory, so it contains what that directory contained.

#### Both spellings for one directory

Declaring **both** spellings for one directory is an error (§9), because two
notes cannot both stand for `X/`. The build uses `X.md`, so it still produces a
hierarchy.

These two are the *only* folder-note spellings. `X/index.md` is not one,
because `index.md` is an ordinary note in this dialect (§2.4). A tree whose
section pages are all called `index.md` therefore gets no folder notes at all.
Any two of those pages collide on the stem `index` (§3) instead.

A generated tree can hit the both-spellings error without meaning to, because
`X/X.md` is a name a converter writes for its own reasons. Take a Python API
mirror. It lays out `api/rmsapi.md` for the package and `api/rmsapi/` for its
members. One of those members is the module `rmsapi` itself, so it writes
`api/rmsapi/rmsapi.md`. Both files now claim `api/rmsapi/`, the second by
accident.

The fix is to rename the member page (`api/rmsapi/rmsapi_module.md`). An `id:`
will not settle it, because the layout reads paths, not ids.

Renaming the member page moves nothing else.

- `api/rmsapi/` is still the directory beside `api/rmsapi.md`.
- `api/rmsapi.md` is still its folder note.
- Every note inside keeps it as their folder-note parent. That includes the
  renamed page, which becomes an ordinary child of the package page, which is
  what it is.

**Do not rename the directory instead.** `X.md` is a folder note only while
`X/` sits beside it. Renaming the directory dissolves the folder note. The
directory gets its `Folder` node back, and every `CHILD_OF` under it becomes a
`CONTAINS` from that node. That loses the hierarchy the layout was there to
express.

The other spelling is a valid fix too. You rename the page *beside* the
directory and let `api/rmsapi/rmsapi.md` stand for it. The package page is then
the parent of nothing: it and the folder note end up siblings under `api/`.
Rename the member.

### 2.4 Ignored paths

The walk prunes these paths with their whole subtree:

- any directory whose name begins with `.` — `.obsidian`, `.trash`, `.git`,
  and `.kglite` itself
- `node_modules`, `target`, `__pycache__`, `venv`, `env`, `site-packages`
- anything matched by `skip_dirs`: a bare name matches a directory at any
  depth, and an entry containing `/` is an anchored vault-relative subtree

`.kglite/` is read by explicit path (§7, §8), never by the walk. It also holds
what kglite writes back. That is the graph cache `.kglite/graph.kgl` (§12) and
the export manifest `.kglite/export-manifest.json` (§10). Neither is a build
input.

A single file opts out with `kg_skip: true` in its frontmatter.

`index.md` and `log.md` are **ordinary notes** in this dialect. (In the `okf`
and `loose` dialects `index.md` is folder metadata and `log.md` is skipped.)

Frontmatter is not required. A plain `.md` file with no `---` block is a note.

## 3. Identity

| What | Rule |
|---|---|
| **id** | frontmatter `id:` → the filename stem |
| **title** | frontmatter `title:` → frontmatter `name:` → the body's first heading, of any level → the filename stem |
| **file path** | vault-relative, forward-slashed, stored as `file_path` |

The id is the node's `concept_id` property and the target of every link. Ids
are deliberately **not** path-derived. Moving a note between folders must not
change its identity, because embedding carry across a rebuild, `.kgl`
snapshots and external references all key on it.

**Stem collisions.** If two or more notes share a stem and neither declares an
`id:`, every colliding note falls back to its vault-relative path minus `.md`
(`projects/alpha`, `archive/alpha`). The collision is reported as an error.
Notes that do not collide keep their stems.

The same fallback settles two other cases:

- two notes that declare the *same* `id:`;
- a fallback that collides in turn (a note declaring `id: archive/alpha` while
  `archive/alpha.md` exists).

The alternative is merging two notes into one node, which loses one of them
silently.

**Case-insensitive collisions.** `Foo.md` and `foo.md` in one directory, or two
ids differing only in case, are reported as a warning on every host. A vault
built on Linux must still open on macOS and Windows.

## 4. Frontmatter

A frontmatter block is YAML 1.2 between `---` fences, starting at the first
byte of the file. Unparseable YAML is an error. The note still becomes a node
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
| `cssclasses` | Obsidian's own styling key: which CSS snippets render this note. Ignored — no property, no edge (§4.3), no hub (§7). |

Every other key becomes a node property, or edges under the rule in §4.3.

### 4.2 Value typing

- Scalars keep their YAML type: string, integer, float, boolean.
- Sequences become **native list properties** (`tags: [a, b]` is a list, not a
  JSON string). This is dialect-specific: `okf` and `loose` keep JSON strings.
- Nested maps flatten to dotted keys: `metadata: {source: vendor}` becomes the
  property `metadata.source`.
- A **top-level** string matching `YYYY-MM-DD` becomes a **date**. One matching
  RFC 3339 becomes a **datetime**, normalised to UTC.
  - To keep a matching value as text, declare it `string` in `types:` (§7).
    Quoting alone does not stop inference.
  - Inference does not reach inside a sequence. A tag literally named
    `2026-01-15` stays the string the tag hub needs.
- `types:` overrides inference per label and property. Declared beats inferred.
- The note's prose is stored under the property named by `body:` (default
  `body`), verbatim, frontmatter stripped.

### 4.3 The typed-edge rule

If a frontmatter key's value is a **wikilink string**, or a list in which
**every** element is a wikilink string, the key becomes edges instead of a
property:

```yaml
---
depends_on: "[[Seismic interpretation]]"
see_also: ["[[Faults]]", "[[Horizons]]"]
---
```

produces `DEPENDS_ON` and `SEE_ALSO` edges from this note to those targets. The
edge type is `UPPER_SNAKE(key)`. The raw property is **not** stored, because
keeping both would duplicate it on export.

A list mixing wikilinks and plain strings stays an ordinary list property. The
rule never splits a key. Targets resolve as body links do (§5.2). An unresolved
target becomes a stub (§5.6). A target naming a place the vault does not own is
the same §9 error here as in the prose.

`parent:` is the one reserved key that follows this rule with a fixed edge
type. It emits the `folder_notes.edge` type in the `folder_notes.direction`. A
note cross-listed under several parents therefore contributes the same edges as
the folder layout would have.

## 5. Links

### 5.1 Syntax

| Written | Meaning |
|---|---|
| `[[Note]]` | Link to `Note`. |
| `[[Note\|display text]]` | Same link. With `structure:` declared (§7.1), the display text is stored as the edge's `label` property. Without it, the text is dropped. |
| `[[Note\\\|display text]]` *(in a table cell)* | The same link. `\\\|` is how Obsidian writes a pipe inside a cell, so the escape belongs to the separator and never to the note's name. A cell's `\\\|` is unescaped wherever it is read, in a property value as much as in a link. |
| `[[Note#Heading]]`, `[[Note#A#B]]`, `[[Note#^block-id]]` | Link to `Note`, `anchor` = the fragment. You address a nested heading by joining the levels with further `#`. With `structure:`, the edge retargets to the section or chunk the fragment names (§5.4, §7.1). |
| `[[Label/Name]]` | Folder-qualified. Use it when a stem is ambiguous. |
| `[[Note]]{type}`, `[[Note\|display text]]{type}` | The same link, **typed**: the brace names the edge type and outranks every heading rule (§5.3 rung 0). The brace must follow the `]]` with no space, hold no whitespace and close on the same line. |
| `[text](path.md)`, `[text](path.md "EDGE_TYPE")` | Path link, resolved relative to the linking note. The title is an explicit edge type. |
| `[text](path.md#Heading)` | Same link, `anchor` = the fragment. A path link carries the fragment exactly as a wikilink does, and the fragment is never part of the target. |
| `[text](file.ext)` | A plain link to a non-`.md` file is an attachment reference (§6), with the link text as its `alt`. |
| `[text](#fragment)`, `[text](sub/dir/)`, `[text](mailto:…)` | An in-page anchor, a directory link and any other URI scheme name no node: silent no-ops. |
| `![[Note]]` | Embed → an `EMBEDS` edge. |
| `![[image.png]]`, `![alt](img/x.png)` | Attachment (§6), never a note link. |
| `[![alt](thumb.png)](full.png)` | A thumbnail linking to the full picture: **both halves count**. The inner image is a reference to `thumb.png`. The outer link is a reference to `full.png`, or an ordinary note link when it names a `.md` file. The outer reference wears the inner `alt`. |
| `https://…` | An external `Source` node keyed by the URL. |

#### Which regions are scanned

Only four regions are not scanned:

- a fenced code block (``` or `~~~`)
- a `%%comment%%` (§5.7)
- a `<!-- kglite … -->` directive (§5.8)
- the inside of an inline `` `code span` ``

Two of them need detail.

- **Fences.** A fence ends at its own delimiter, so a `~~~` line written inside a ``` block is code like everything else between them.
- **Code spans.** A code span is rendered literally in every dialect, so `` `[[Note]]` `` states no link, `` `#tag` `` no tag and `` `![x](y.png)` `` no picture. The span is still ordinary inline text to what is written *around* it, so ``[`file.md`](file.md)`` is one link whose display text happens to be code.

Every other region is scanned, including the following cases.

- **Indented code and HTML blocks.** Indented four-space code is **not** exempt, and neither is an HTML block that is not a directive (§5.8). Honouring CommonMark's indented-code rule would also swallow every list continuation line, which is where a converter writes most of its links.
- **Heading lines.** A **heading line is scanned like any other line**: `## Overview ![map](img/x.png)` states a picture and `## See also [[Alice]]` states a link. §5.4 says which section they carry.

To keep text from being read, fence it. Write `\[\[` for a literal `[[`, which is the escape Obsidian uses.

#### Blocks and line wrapping

The reader parses a body's **blocks** once. It scans each block as one region: a heading line, a paragraph, a list item or a table cell. Two consequences follow.

- The text half of a `[…](…)` link may be hard-wrapped across lines. `[Binary\nExtensions](url)` is one link, not none.
- A bracket left open at the end of a paragraph can never swallow the next paragraph.

A `[[wikilink]]` is the exception: it stays on one line, as it does in Obsidian.

#### Percent-decoding

The target of a **markdown-style** link is percent-decoded before it is resolved. That target is the `(…)` half of `[text](…)` and `![alt](…)`, and `%20` is the spelling a tool writes there. `[report](sales%20report.md)` reaches `sales report.md`. A `%` not followed by two hex digits is itself, so a filename containing one survives.

A **wikilink** target is a name, not a URL, and is never decoded. `[[a%20b]]` names a note spelled that way.

#### Targets the vault does not own

A target naming a place the vault does not own is an error (§9), not a link. Two kinds of target qualify:

- an absolute filesystem path: `C:/…`, `\\server\share`, `~/…` or `file:…`
- a path climbing above the vault root with `../`

A leading `/` is **not** one of these. §6.2 fixes it as vault-root-relative, which is what lets you copy a vault somewhere else.

#### Notes versus attachments in embeds

An embed is read as a note when its target has no file extension or ends in `.md`. Otherwise it is read as an attachment. A note whose *filename* contains a dot is therefore embedded as `![[Release 1.2.md]]`, with the extension written out.

### 5.2 Resolution ladder

A link target resolves against the first rung that matches exactly one note:

1. an exact id
2. a filename stem
3. an entry in some note's `aliases:`
4. a normalized slug — case-insensitive, `_` and `-` equivalent
5. a title
6. otherwise: a stub (§5.6)

A trailing `.md` is stripped first. A target containing `/` is tried first as a vault-relative id, then as a path relative to the linking note.

### 5.3 Edge type

For a body link, the edge type is the first rung that applies:

0. a `{type}` written straight after a wikilink's `]]` — `[[Customers]]{joins-with}`
1. an explicit link title that looks like a type — `[x](y.md "JOINS_WITH")`
2. an entry in `heading_edges:` matching the enclosing heading text in full, case-insensitively
3. the built-in heading ladder, matched case-insensitively as a substring of the enclosing heading: *citation* → `CITES`, *join* → `JOINS_WITH`, *reference* → `REFERENCES`, *related* → `RELATED`, *depend* → `DEPENDS_ON`
4. `LINKS_TO`

Rungs 0 and 1 are each one spelling's own: a wikilink carries no title and a markdown link takes no brace. Both say what *this* link means, which is why they outrank the heading the link happens to sit under.

`heading_edges:` wins over the built-in ladder. That is why a corpus writes `heading_edges: {"Related topics": RELATED_TO}` instead of accepting `RELATED`.

#### The heading ladder is not semantics

The heading ladder classifies text. It does not prove semantics. A neutral reference under a heading containing “depend” becomes `DEPENDS_ON`, even when the source never asserted a prerequisite.

Converters should follow three rules.

- Use `[[Target]]{LINKS_TO}` (or `[text](target.md "LINKS_TO")`) for a source-neutral reference.
- Reserve stronger types for explicit source evidence.
- Keep different source relations distinct. These are not interchangeable merely because all connect two pages: a vendor table of contents or breadcrumb, a GUI entry path, a source-authored related-topic link, API index membership, symbol ownership and an execution prerequisite.

#### The `{type}` suffix

The suffix must follow the closing `]]` with nothing between them. It must hold no whitespace and close on the same line. Its text is normalised the way a frontmatter key is (§4.3), so `{see-also}`, `{see_also}` and `{SEE_ALSO}` all name `SEE_ALSO`. The result must be a non-empty name that does not start with a digit.

If a brace fails any of those rules, it is left as prose and the build **warns**, naming the note and what was written. The link keeps the type rungs 2–4 give it.

A brace the link does not touch is ordinary prose and says nothing about the link, silently. That covers a space before the brace and a line that ends before any `}`.

Two more spellings are deliberately *not* link types:

- `[[Note]] #tag`: `#tag` is a tag and stays one (§5.5).
- `![[Note]]{x}`: an embed's type is `EMBEDS`.

Everything else about the link is unchanged. `[[Note#Heading|display text]]{see-also}` keeps its `anchor`, its `label` and its retarget (§5.4).

The suffix is prose like the rest of the body, and the body is never rewritten (§10.3). A section or chunk whose text spans a typed link keeps the `{type}` in its `text` and `embed_text`, and an export writes the line back byte for byte.

### 5.4 Edge properties

Every body link carries `section`. `anchor` and `label` appear only in the cases the table gives.

| Property | Value |
|---|---|
| `section` | The enclosing heading's text verbatim. Absent above the first heading. |
| `anchor` | Present on a fragment link. The fragment without its leading `#`. A nested heading path keeps the `#`s *inside* it, so `[[Note#A#B]]` carries `A#B`. |
| `label` | Present on a link written `[[Target\|display text]]`, where `structure:` is declared (§7.1). That text. Without `structure:`, the text is dropped. |

A link or attachment reference written *in* a heading line is enclosed by that heading. It carries the same `section` string as the links below it, including any markup the heading contains, because the text is verbatim. One section is one value; otherwise a section would split into two edge groups.

#### Fragments and retargeting

**The fragment never changes which note a link names.** `[[Note#Heading]]` and `[[Note]]` name the same note, and so do the two spellings of a path link.

Where `structure:` derives sections or chunks from that note, the edge **retargets** to the derived node whose id is exactly `<note id>#<fragment>`. A fragment naming a single heading retargets to the **first** section in the note titled that, which is the heading Obsidian itself jumps to (§7.1). The `anchor` property is kept either way, so the fragment as written survives the retarget.

A fragment naming no heading and no block id in that note leaves the edge on the note and is a warning (§9).

#### One edge or two

Two links from one note to one target are **two edges** when they differ in `section`, `anchor` or `label`. They are one edge when they do not. Repeating a link inside a section is one relationship; linking from two sections is two. Edges emitted from frontmatter (§4.3) carry none of the three properties.

### 5.5 Tags

Both tag forms feed one `Tag` hub per distinct tag, joined by `TAGGED`. The two forms are:

- `tags:` in frontmatter, which also stays a list property on the note
- inline `#tag` in the body

#### Hub identity

A hub node holds its text in `id`, not in the `concept_id` a note uses. §7's table names the id property of every kind of node.

Tag identity is **case-insensitive**, as it is in Obsidian. `#Seismic` and `#seismic` are one tag, held under the lowercased id and titled with the casing the vault used most often. This is one of the places the dialects genuinely differ: `okf` and `loose` keep tags case-sensitive.

To keep the two casings apart, redeclare the hub in `vault.yaml` with `case_insensitive: false` (§7). That is the same mechanism any other hub uses.

#### Inline extraction

Inline extraction skips four things:

- fenced code
- inline code spans
- a `#` inside a URL or a wikilink anchor
- a `#` that begins a line and is followed by a space, which is a heading (a `#tag` written *in* the heading's text is still a tag)

A tag name runs over letters, digits, `_`, `-` and `/`, and must contain at least one letter. `#2026` is not a tag.

#### A tag on the node that holds it

Where `structure:` derives nodes (§7.1), every inline `#tag` is *also* written into a `tags` list property. The property lands on the **innermost derived node whose range contains the tag**:

1. the chunk, callout, step or table row it sits in
2. else the enclosing section
3. else nothing

The list is in first-use order and spelled as the note wrote it. Folding case is the hub's rule for identity, not this property's. The property makes a paragraph-scoped marker selectable: `MATCH (c:Chunk) WHERE 'warning' IN c.tags`.

`tags` is a property a derived node defines itself (§7.1), so no `inherit:` and no directive may name it. The note's own `tags` property is unchanged: it reports the frontmatter and nothing else. The note's `TAGGED` edges are unchanged too.

**Caveat: a chunk is as narrow as the author made it.** A chunk packs several paragraphs, so a marker meant for one paragraph tags the whole chunk. To avoid that, give the paragraph a `^block-id` (§5.7) or close the chunk with `<!-- kglite chunk -->` (§5.8). Both make it a chunk of its own.

#### `tag_labels:` — a family of tags modelled as its own nodes

A vault can declare that tags under a prefix are not tags at all but a kind of thing:

```yaml
tag_labels:
  "intent/*": {label: Intent, edge: HAS_INTENT}
```

`#intent/create-grid` then mints an `Intent` node with the id `create-grid`, which is the tag text **after** the prefix. It also mints an edge `HAS_INTENT` to that node. The edge starts from the same innermost derived node the `tags` property landed on, or from the note where `structure:` derives none.

The declaration has three rules:

- The pattern is `<prefix>/*` and nothing else. Any other spelling is a config error naming the key.
- Both `label` and `edge` are required.
- `edge` is spelled `UPPER_SNAKE` like every other edge type.

A tag a rule matches is modelled **only** that way. It leaves the `Tag` hub entirely, with no `Tag` node and no `TAGGED` edge, whichever of the two forms above wrote it, because both forms feed that one hub. The text does *not* leave:

- A frontmatter `tags:` list still reports every entry as written.
- A derived node's `tags` list still carries the tag, matched or not.

Matching has four more rules.

- Identity folds case exactly as the hub's does. `#Intent/Create-Grid` and `#intent/create-grid` are one `Intent`, and the node's title is the spelling the vault used most often.
- Where two rules match, the **longest prefix wins**. `intent/grid/*` takes `#intent/grid/create` out of `intent/*`.
- A tag that is only the prefix (`#intent/`) names nothing and stays an ordinary tag.
- A rule no tag in the vault matched is a warning (§9), like any other declaration the vault's content does not carry.

### 5.6 Unresolved targets

An unresolved link target becomes a `_provisional: true` stub node. The stub is labelled `Concept` and keyed by the unresolved name. "Referenced but not written" is therefore one Cypher query, and the stubs never mix with the notes' own labels. Stubs are counted in the build report and are never exported as files (§10).

### 5.7 Comments, block ids and callouts

A body can carry three more constructs. None is new syntax; all three are Obsidian's own. The reader honours them whether or not `structure:` is declared. What `structure:` adds is turning callouts into *nodes* (§7.1).

#### Comments

`%%…%%` hides text from a reader. It works inline, as in `a %%hidden%% word`, and as a block, where the opening and closing `%%` sit on lines of their own.

A comment's text is **never scanned**: no link, no tag, no attachment reference and no heading is read out of it. A fenced code block and a directive share that property (§5.1, §5.8). The comment is still part of the body property, verbatim, and it still travels through an export.

#### Block ids

A trailing ` ^id` names a block, so `[[Note#^id]]` links to exactly that block rather than to the note. The id may hold **Latin letters, digits and dashes only**. `^my_id` is not a block id; it is text.

The three placements are Obsidian's:

- at the end of the last line of a paragraph, after a space
- on a line of its own, blank line above and below, directly after a table, list, quotation or fenced block
- directly on a bullet, naming that one item

With `chunks:` declared, a block id keys the chunk it names (§7.1). It is the one derived id that survives editing around it. That makes it the answer both to a chunk id that churns and to a duplicate heading that cannot be linked.

#### Callouts

A blockquote whose first line begins `> [!type]` is a callout:

```markdown
> [!warning]+ Check the survey datum
> Depth values are metres below MSL.
```

The type identifier is case-insensitive and **arbitrary**. Obsidian styles the thirteen it knows and renders any other as a plain callout. `[!versionadded]` is therefore legal, and a corpus keeps its own vocabulary rather than being folded into `note`.

Callouts follow these rules:

- The kind is stored **lowercased**.
- A `+` or `-` directly after the identifier folds the callout open or closed. It is not part of the kind.
- Text after the identifier is the title. With none, there is no title: Obsidian displays the type, and this spec stores nothing.
- Callouts nest.

A callout's body is prose like any other. Its links, tags and images are scanned, because a callout is not a comment, and its `section` is the heading above it.

### 5.8 Directives

```markdown
<!-- kglite chunk -->
<!-- kglite address: Data tree -> Wells | Task pane: Wells -> Annotations table -->
```

#### Recognition

An HTML comment of the form `<!-- kglite <key>[: <value>] -->`, **on a line of its own**, is a directive. It is an instruction to the reader, written where it applies, in syntax Obsidian already hides from a rendered note. The form has these rules:

- Whitespace inside the comment is free.
- `kglite` must be a word of its own. `<!-- kglitex … -->` is an ordinary comment, and so is every HTML comment that does not open with the literal.
- The key is spelled like a frontmatter key: a letter or `_`, then letters, digits, `_`, `-` or `.`.
- Everything after the first `:` is the value, trimmed.
- The whole block must be the comment. A comment that only starts like a directive (`<!-- kglite owner: docs --> and more`) is prose.

**Only a block counts.** A `<!-- kglite … -->` written *inside* a paragraph is inline HTML and stays prose, so a note may document the syntax without invoking it. `<!-- kglite -->` names no key. The reader recognises it as the shape it is and warns about it (§9); it carries no meaning.

#### A directive is not prose

A directive's own bytes are never scanned. No link, no tag and no attachment is read out of one, exactly as for a `%%comment%%` (§5.1).

Where `structure:` derives nodes (§7.1), the directive's line is **cut out of every derived `text`**: a section's, a chunk's, and the `embed_text` rendered from either. The cut is the directive's own range and nothing more. The blank lines around it stay, and every other byte is the author's own. The note's `body` property keeps the directive verbatim, which is what lets an export write the file back byte for byte (§10).

#### Directive keys

A directive is recognised, and skipped, in every dialect. What a key *means* is defined here.

| Directive | Meaning |
|---|---|
| `<!-- kglite chunk -->` | Close the open chunk at this point (§7.1 `chunks:`). |
| `<!-- kglite heading -->` | Promote the first line of the paragraph below to a heading (below). |
| `<!-- kglite <key>: <value> -->` | State `<key>` on the node this directive sits in: a **typed edge** when the value names wikilinks, a **property** otherwise. |
| `<!-- kglite <key> -->` | Nothing. A key with no value states nothing, and the build warns (§9). |

#### Which node it states it on

The directive states its key on the **enclosing section** when `structure:` declares `sections:` and the directive sits under a heading. Otherwise it states the key on the **note**: above the first heading, and in a vault that derives no sections. A directive therefore reaches exactly the node a reader would point at: the one whose prose it was written beside.

#### Value or edge, by §4.3's rule

A value that names a wikilink becomes edges and no property. Two shapes qualify: one `[[Target]]`, or a list in which every element is one. The edges have type `UPPER_SNAKE(key)` and run from that node to those notes. Targets resolve as body links do (§5.2). An unresolved target becomes a stub (§5.6), exactly as a frontmatter key's would.

Any other value becomes a property named `key`. It is typed as §4.2 types a frontmatter value, and `types:` overrides that for the node's label.

#### Value grammar for inline directives

A directive is written *inline in prose*, so the value grammar bends three ways for it:

- **A bare `[[Target]]` needs no quotes**, and nor does `[[A]], [[B]]`. In frontmatter the quotes stop YAML reading `[[X]]` as a nested flow sequence. In a comment beside a sentence, `[[X]]` can mean nothing else. `[[A, B]]` is still one target: the whole value is tried as a single wikilink before the commas are.
- **A value YAML would read as a mapping is the raw text.** `:` is ordinary punctuation in a sentence, so `address: Data tree -> Wells | Task pane: Wells` states that string. A property could not hold a mapping in any case.
- **A value YAML refuses is the raw text too**, for the same reason.

#### Keys a directive may not name

A directive may not name anything a note or a derived node defines itself:

- the §4.1 reserved keys
- the derived properties `inherit:` may not name either (§7.1)
- the note's `body:` property
- `concept_id` and `file_path`

Each is an **error** naming the key (§9).

A key the vault declares under `hubs:` (§7) is nothing special here. It is a property, unless its value is a wikilink and the typed-edge rule takes it. That is the same precedence §4.3 already sets for frontmatter.

#### `<!-- kglite heading -->` — a heading the source did not write

On the line above a paragraph, the marker promotes that paragraph's **first line** to a heading. The new heading's level is the enclosing heading's level + 1: level 1 where there is none, and never deeper than 6, which is as deep as markdown goes.

- A surrounding pair of `**bold**` markers comes off the heading's text, and nothing else does. A code span, a link or a `**word**` in the middle of the line is the heading's own text.
- The paragraph's remaining lines, if any, are the first paragraph under the new heading.

```markdown
### rmsapi.Project

<!-- kglite heading -->
**open(filename, readonly=False)**

Opens a project.
```

From there it is a heading like any other:

- It derives a `Section` (§7.1).
- It is addressed as `[[Note#rmsapi.Project#open(filename, readonly=False)]]`.
- `key_from_heading:` reads its title.
- A table under it attaches to it.
- The `#fragment` ladder of §5.4 resolves onto it.

The **body is untouched**. Only the tree a reader builds from it changes, so the file still renders in Obsidian exactly as it did and an export writes it back byte for byte.

Two markers under one heading are **siblings**. The level is read from the heading the author wrote, not from the last synthetic one. That makes a converted API page a flat list of methods under its class.

A marker with no paragraph below it promotes nothing and is a warning (§9). That covers the end of a section, and a list, table, fence or second directive coming next.

#### One key, one value per node

A second directive naming a key the same node already carries replaces it, and the build warns (§9). The earlier value may come from an earlier directive or from the note's own frontmatter. Edges do not follow that rule: two wikilink directives sharing a key state two edges, as a frontmatter list does.

## 6. Attachments

1. **Accepted syntax:** `![alt](rel/path.png)`, `![[image.png]]`, `![[image.png|alt]]`, and `[text](rel/path.pdf)`. The last is a **plain** link naming a non-`.md` file, whose link text is its `alt`.
   - An image written *inside* a link's text, `[![alt](thumb.png)](full.png)`, is two references: the thumbnail and whatever the link points at (§5.1).
   - The leading `!` says how a renderer displays the file, not whether the vault holds it. Both spellings are therefore the same reference here and make the same node and the same edge. A vault of 48 `[download](tool.zip)` links would otherwise produce nothing at all.
   - The body keeps the original syntax verbatim.
   - A target carrying a URI scheme (`http(s)`, `mailto:`, anything else) is somebody else's file. It is not in the vault, no `stat` describes it, and it becomes no node. So do an in-page `[text](#fragment)` anchor and a directory link, which name nothing to resolve.
   - The `![alt](…)` and `[text](…)` spellings are percent-decoded before resolution, and the `![[…]]` one is not, exactly as §5.1 reads the two syntaxes. A reference naming an absolute or escaping path is a §9 error.
   - **A converter emits the encoding** that decoding undoes. A markdown target is read up to the first whitespace or `)`. A space therefore makes the whole reference invisible: nothing matches, and there is no node, no edge and no warning. A `)` truncates the target to a path that resolves to nothing.
   - Emit `%20`, `%28` and `%29` for those characters, and `%25` for a literal `%`. A `%` is otherwise eaten whenever the two characters after it are hex digits.
   - Encoding cannot rescue `#` or `?`. The target is decoded *before* it is split, so `img/c%23d.png` is cut at the `#` exactly as `img/c#d.png` is. The `![[…]]` spelling splits on `#` too. A file whose name holds one is unreachable from either syntax, so rename it.
   - Wikilink targets are names, not URLs, and are never encoded.
2. **Resolution ladder:** note-relative → vault-root-relative → a unique filename anywhere in the vault.
   - The stored value is always the **vault-relative** resolved path, so every consumer resolves from one root.
   - A filename occurring twice does not resolve on the third rung; qualify it.
   - A target written with a leading `/` is vault-root-relative and skips the first rung, exactly as a path *link* reads one.
3. **Nodes:** one node per distinct resolved file.
   - The label is `Image` when the extension maps to an image MIME type the MCP server delivers (`image/png`, `image/jpeg`, `image/gif`, `image/webp`). Otherwise it is `Attachment`, so SVG and TIFF are `Attachment`s.
   - The node's id field is named `path` and holds the vault-relative path, so `n.path` *is* the id.
   - `mime` comes from the extension table, and is `application/octet-stream` for an extension the table does not name.
   - `size_bytes` and `mtime` (a UTC datetime) come from `stat`.
   - `title` is the filename.
   - An `Image` also carries `text`: the distinct alt texts and the titles of the notes using it, newline-separated in first-use order, so captions stay text-searchable.
4. **Edges:** `HAS_IMAGE` or `HAS_ATTACHMENT` from the note, with these edge properties:
   - `alt`: the alt text of an `![…]` reference or the link text of a plain one, when there is one.
   - `section`: the enclosing heading.
   - `ordinal`: numbers the edges a note emits of each kind, 0-based in body order, so a folded repeat consumes no number.

   Two references to one file from one note are **two edges** when they differ in `section` or `alt`, and one when they do not. This is §5.4's rule, applied to attachments.
5. **Bytes are never read at build time.** Metadata comes from `stat` only: no content hash, no dimensions, so build cost is independent of image volume. The type therefore comes from the extension, never from the content.
6. **A missing target** becomes a `_provisional: true` node with `missing: true`.
   - It is labelled from its extension like any other.
   - It is keyed by the reference as written (normalised).
   - It is counted in the build report.
   - An ambiguous bare filename is one of these: it resolved to nothing, and the warning names the candidates.

**Recommendation for converters: emit PNG, JPEG, GIF or WebP**, the four types the bundled MCP server delivers as images. SVG is stored as an `Attachment` and is not delivered, so rasterise it when you build the source.

## 7. `.kglite/vault.yaml`

`.kglite/vault.yaml` is the declaration file. A vault keeps five things in `.kglite/`:

- Inputs the build reads: this file, plus `skills/` and `recipes/` (§8).
- Outputs kglite writes and never reads as content: `graph.kgl` (§12) and `export-manifest.json` (§10).

The file is optional. kglite reads it by explicit path, because the walk ignores dot-directories. Every build applies it, and every rebuild re-applies it. With carried embeddings, it is the only state that survives a rebuild.

Each of these is an error:

- An unknown top-level key.
- An unknown key inside `structure:` (§7.1).
- An unknown `kglite_vault` version.
- A value of the wrong shape.

Any of them **fails the build** rather than leaving a finding on a graph that looks built. The file is the only thing a rebuild re-applies, so a vault whose `vault.yaml` stopped parsing would silently lose its labels, hubs, indexes and embed targets. `okf.validate` reports the same failure as the §9 error.

The file is a vault construct. The `obsidian` dialect reads it, and only that dialect does. An `okf` or `loose` build that finds one ignores it **with a warning**.

The declarations win over whatever the caller configured, in both directions. A rebuild re-reads the file, so the file is the vault's statement about itself.

### Keys

| Key | Shape | Meaning |
|---|---|---|
| `kglite_vault` | integer | Format version. `1`. |
| `default_label` | string | Label for notes with no `type:` (§2.1, rung 2). |
| `label_from` | `type` \| `folder` | `type` (default) uses the ladder in §2.1; `folder` puts the folder rung first. |
| `body` | string | Property name for the prose. Default `body`. |
| `skip_dirs` | list of strings | Extra directories to prune (§2.4). |
| `folder_notes` | `{edge, direction}` | `edge` default `CHILD_OF`; `direction` is `child_to_parent` (default) or `parent_to_child`. |
| `hubs` | `{<frontmatter key>: {label, edge, case_insensitive}}` | Turn a list-valued key into hub nodes. `case_insensitive: true` folds the id to lowercase and titles the node with the casing the vault used most often, ties settled alphabetically; otherwise the title is the id. The built-in `tags` hub is `{label: Tag, edge: TAGGED, case_insensitive: true}` (§5.5) and can be redeclared like any other. |
| `tag_labels` | `{"<prefix>/*": {label, edge}}` | Tags under `<prefix>/` become nodes of that label instead of joining the `Tag` hub, joined by `edge` from the node that holds them (§5.5). Both fields are required; the longest matching prefix wins. |
| `heading_edges` | `{<heading text>: EDGE_TYPE}` | Merged over the built-in ladder (§5.3). |
| `types` | `{<Label>: {<property>: <type>}}` | Declared property types: `string`, `int`, `float`, `bool`, `date`, `datetime`, `list`. Overrides inference. |
| `indexes` | `{<Label>: [ <prop> \| {range: <prop>} \| {composite: [<prop>, …]} ]}` | Equality, range and composite index declarations. |
| `text_indexes` | `{<Label>: [<prop>]}` | BM25 lexical indexes. |
| `ontology` | mapping | Passed verbatim to the ontology declaration API — same document `define_ontology` accepts; see the [ontology guide](https://kglite.readthedocs.io/en/latest/python/guides/ontology.html). |
| `embed` | `{<Label>: <prop>}` | Which text property to embed per label. Reported as a build target; the vectors are computed when an embedder is bound. |
| `structure` | mapping | Nodes derived from a note's own body — sections, chunks, callouts, fences, ordered lists, tables (§7.1). |
| `edge_defaults` | `{EDGE_TYPE: {<prop>: <value>}}` | Edge properties that are constant per type, pushed onto every edge of it (§7.2). |
| `export` | `{edge_tables: {EDGE_TYPE: <heading>}}` | What the exporter writes back beyond the default (§7.3, §10.6). |

### Hubs

A hub reads a key's **list** entries. A scalar joins no hub.

If a key is both a hub and wikilink-valued, the typed-edge rule (§4.3) takes it instead. That rule wins, and the clash is a warning (§9) rather than a silent empty hub.

Declared hubs are **merged over** the built-in `tags` one rather than replacing the set. A redeclaration names only what it changes:

- An omitted `label` is `Tag`.
- An omitted `edge` is `TAGGED`.
- An omitted `case_insensitive` is what the built-in already said: `true` for `tags`, `false` for every other key.

### Declared types

`types:` decides how a note's property **column is built**. A declaration overrides inference; it does not convert a value afterwards.

- It names the label a note *ends up with* and a property that label carries.
- `concept_id` is never retyped: it is the node's identity and the index built on it.
- A value that will not coerce is left exactly as it was written and **warned** about. The warning names the note, the property, the declared type and the value.
- A declaration no note matches is a warning too.

The declaration is a statement about the vault, and a note that disagrees with it still holds what a human typed.

### Declarations that name missing things

`indexes:`, `text_indexes:` and `embed:` may name a label or property the vault does not carry yet. Each such name is a **warning**, never an error, and kglite still installs the rest.

### Which property holds the id

Every declaration above names a property, and the id property differs by kind of node. If you declare `indexes: {Topic: [concept_id]}` for a hub, kglite installs an index over a property no hub node carries and warns "indexed no value". That warning is the only signal, hence this table:

| Node | Id property |
|---|---|
| A note, whatever its label | `concept_id` |
| A `Concept` stub for an unresolved link (§5.6) | `concept_id` |
| A hub node — `Tag` and every `hubs:` entry (§5.5) | `id` |
| A `Source` node for an external URL (§5.1) | `id` |
| A `Folder` node (§2.2) | `id`, holding the vault-relative directory path |
| An `Image` or `Attachment`, present or missing (§6.3) | `path` |
| A node `structure:` derived from a note's body (§7.1) | `concept_id` |

A note's id is `concept_id` because it is a **name** every link in the vault resolves to (§3), and `types:` never retypes it. The synthesized nodes are keyed by what they already are (a tag's own text, a URL, a directory, a file path), and say so.

### Complete example

This example declares a vendor help corpus of ~7k articles:

```yaml
# .kglite/vault.yaml
kglite_vault: 1
default_label: Article  # outranks the folder rung — omit when folders are your labels
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

### 7.1 `structure:` — nodes from a note's own body

By default a note is one node and its prose is one property (§4.2). A `structure:` block also derives nodes from the note's own markdown:

- the headings,
- the paragraphs under them,
- the callouts,
- the fenced examples,
- the numbered steps,
- the table rows.

The block adds no file to the vault and no syntax to a note. Obsidian already names sub-note things, and every id this block mints is one of those names. A derived node is therefore linkable from anywhere in the vault as `[[Note#Heading]]` or `[[Note#^block-id]]`, and navigable in Obsidian itself.

There is no default and no heuristic. A vault with no `structure:` block builds the graph it built before, note for note and edge for edge. An unknown key *inside* `structure:` is an error, exactly as an unknown top-level key is (§9). The block is a hard compatibility boundary. The rest of this section says what a kglite that knows it does with each key.

#### How a derived node is keyed

Every derived node keys on `concept_id`, like a note, and **never carries `file_path`**. It is not a file, and no export writes one (§10.1).

| Construct | Id |
|---|---|
| A section | `Note#A#B` — the note's id, then its heading path joined by `#` |
| A chunk, callout, example, procedure, step or table row | its parent's id, then `~<kind><n>`: `Note#A#B~chunk3`, `~note1`, `~example2`, `~list1`, `~list1~step4`, `~row7` |
| Anything named by a block id (§5.7) | `Note#^block-id` |
| A duplicate of either | the same id with `~2`, `~3`… appended |

`<kind>` is the construct's own word, and `<n>` counts that kind under that parent from 1. The counter therefore always starts with a letter, and the duplicate suffix never starts with one. This is deliberate: a bare `~2` can only mean "the second thing that wanted this id". It can never be read as the second chunk of a section.

#### Which ids are stable

Only the section ids and the block-id ones are **stable** across edits. These cases move an id:

- A `~chunk<n>` id is scoped to its section, so it renumbers when a paragraph is inserted above it.
- Every id under a heading changes when that heading is renamed, because the heading text is in the id.

On a 22 226-chunk corpus, a one-paragraph insert moved 3.6% of the ids, and a heading rename moved 58% of one page's. Where an id has to survive, write a block id (§5.7). Where it does not, `chunk_hash` below is what carries the embedding across.

#### `sections:`

```yaml
structure:
  sections: {label: Section, edge: HAS_SECTION, parent: PARENT_SECTION, next: NEXT_SECTION}
```

`sections:` makes one node per heading in the body, in document order. A `#` inside a fenced code block is not a heading, because kglite does not scan that region at all (§5.1).

- **Title** is the heading's text **verbatim**, including any inline markup it carries.
  - `## See also [[Alice]]` titles a section `See also [[Alice]]`. That is the same string §5.4 already stores in `section`, and one heading must not have two spellings.
  - Markdown's optional closing run of `#`s (`## Overview ##`) is a closer and not part of the text.
  - Surrounding whitespace is trimmed.
- **Id** is `Note#A#B`: the note's id, then the titles of the headings enclosing this one and its own, joined by `#`. That is Obsidian's own nested-heading link spelling, so `[[Note#A#B]]` reaches exactly this node.
- **Properties**:
  - `title`, `level` (1–6), `ordinal` (0-based among its siblings), `path` (the list of titles the id joins) and `note_id`.
  - `text`: the body verbatim from the line after the heading to the next heading of the same or higher level. Trailing blank lines are trimmed, and any `<!-- kglite … -->` directive is cut out (§5.8).
  - Every `inherit:` property.
  - Whatever a `<!-- kglite <key>: <value> -->` written under this heading states, as a property or as a typed edge leaving this section (§5.8).
  - The note's own `body` property is untouched and still holds the whole body. Deriving sections moves nothing out of the prose.
- **Edges**:
  - `edge` joins the note to each of its top-level sections, and a section to each section directly inside it. Every section therefore has exactly one incoming `HAS_SECTION`, and the chain from the note spells its path.
  - `parent` states the same nesting child→parent and is emitted only for a section that has one.
  - `next` joins consecutive siblings under one parent in document order.
- **Duplicate headings.** Obsidian resolves a heading link to the **first** heading of that text and has no syntax for a later one. So does this spec.
  - `[[Note#A]]` reaches the first heading.
  - The second gets a `~2` id and a warning (§9).
  - The fix the warning steers to is a block id on the second, the only spelling Obsidian itself can link.

#### `chunks:`

```yaml
  chunks: {label: Chunk, edge: HAS_CHUNK, next: NEXT_CHUNK, max_words: 650, max_chars: 6000}
```

`chunks:` defines the retrieval unit. Leaf blocks — paragraphs, list blocks, fences, tables, quotations — are packed greedily in document order. The open chunk closes before a block that would take it past `max_words` **or** `max_chars`. A section boundary always closes the open chunk, so a chunk never spans two sections.

**A block bigger than either limit on its own is split inside itself**, at the boundaries its own kind offers:

- A list splits between its **top-level items**. A nested list travels with the item that introduced it.
- Anything else splits at line ends. For a table, that means its rows.

The pieces are packed greedily to the same caps, in document order, and chain with `next` like any consecutive chunks. A header row is in the first piece of a split table and is **not** repeated in the others: a chunk is a range of the source, not a rendering of it. A single line that busts `max_chars` by itself is cut at a `char` boundary. That is the only split left, and the only one that can land inside a word.

The build reports how many boundaries the caps forced this way as `forced_splits` (§9). A non-zero count says the source has passages the caps had to cut blind. A blank line where the author wants the break is the fix.

- `edge` joins the enclosing Section, or the note when `sections:` is not declared. `next` joins consecutive chunks within one section.
- `<!-- kglite chunk -->` on a line of its own (§5.8) **closes the open chunk** at that point and is itself no chunk at all. It is the author's break, where a blank line would have joined the two passages anyway.
  - Only a top-level marker counts. Inside a list item, a quotation or a table there is no chunk of its own to close, so the marker does nothing and the build warns (§9).
  - It is **not** counted in `forced_splits`. That number is the boundaries the caps had to place blind, and an authored one is a choice.
- A paragraph whose last line ends in a block id **closes the open chunk and is a chunk of its own**, keyed `Note#^id`.
  - A block id is therefore the author's one lever over where chunks divide, and the way to give a passage a citable id that editing around it cannot move.
  - If such a block is itself over a limit and splits, the id keys its **first** piece. That is the piece `[[Note#^id]]` was pointing at while the block still fitted.
- **Properties**:
  - `text`: the packed blocks verbatim, spaced as the source spaced them, less any `<!-- kglite … -->` directive they contain. A directive is metadata and never chunk text (§5.8).
  - `ordinal` (0-based within the section).
  - `chunk_hash` (the SHA-256 of `text`, lowercase hex).
  - `note_id`, `section_id`, plus `inherit:`.
- **Embeddings survive a rewrite.** A rebuild carries vectors by `(label, id)` (§12). For anything this block derived, it then carries them by `(label, chunk_hash)` where exactly one old node of that label carried the hash. A chunk that only moved is recognised as the same chunk instead of being minted afresh.
  - The fallback decides *which old node this is*, not whether to re-embed.
  - The changed-mode pass still compares the embedded property's own stored hash.
  - A chunk whose `embed_text` changed because its heading changed is re-embedded. One whose text merely shifted is not.

#### `callouts:`

```yaml
  callouts: {label: Note, edge: HAS_NOTE}
```

`callouts:` makes one node per callout (§5.7). `edge` attaches it to the enclosing Section, or to the note where there is no section, or to the enclosing callout where callouts nest.

Properties:

- `kind`: the type identifier, lowercased, **whatever word it is**. `versionadded` and `caution` are as valid as `note`.
- `title`: absent when the callout has none.
- `fold`: `+` or `-`, the identifier that folds the callout open or closed. Absent when the callout is not foldable.
- `text`: the callout body verbatim with the `>` markers stripped.
- `ordinal`, `note_id`, `section_id`, plus `inherit:`.

A nested callout's `text` loses **its own** depth of markers and no more. A callout written `> > text` two deep reads `text` on its own node, while its parent's `text` keeps the `> [!tip]` line that says a callout is nested inside it. `section_id` names the heading either way: a nested callout is inside another callout *and* inside the same section.

#### `code_fences:`

```yaml
  code_fences: {label: Example, edge: HAS_EXAMPLE, langs: [python]}
```

`code_fences:` makes one node per fenced block whose info string's first word is in `langs`. The comparison is case-insensitive at both ends. **If you omit `langs`, every fence qualifies**, including one carrying no info string at all. That is the setting a corpus needs when its converter dropped the languages on the way in.

Properties:

- `lang`: the first word of the info string, lowercased. Absent when there is none.
- `code`: the fence contents verbatim, without the fence lines and without the info string. kglite dedents it by the indentation of the container the fence sits in, so a fence inside a list item keeps only the code's own indentation.
- `caption`: the paragraph immediately above the fence in the same container, when that paragraph's text ends with `:`.
- `ordinal`, `note_id`, `section_id`, plus `inherit:`.

A caption paragraph stays in its chunk's text as well: nothing is taken out of the prose.

#### `ordered_lists:`

```yaml
  ordered_lists: {label: ProcedureStep, container: Procedure, edge: HAS_STEP,
                  next: NEXT_STEP, under_heading: "^(Procedure|Steps|To .*)", min_items: 2}
```

Every **top-level** ordered list becomes a procedure when it holds at least `min_items` items (default 2). A top-level list is one not nested inside another list item. An *unordered* list is never a procedure, whatever heading it sits under.

`under_heading` is optional and is the opt-in narrowing:

- It is a regular expression matched against the enclosing section's title.
- If you declare it, kglite reads only the lists under a heading that matches.
- Without it, every qualifying list is read. That is the rule the corpus this profile was measured against was built with.
- A list above the body's first heading sits under no heading at all, so a declared `under_heading` cannot reach one.

Two kinds of node result:

- The container is a **new node**, not the enclosing section relabelled. A section holding two lists therefore yields two procedures, `Note#A#B~list1` and `~list2`.
  - It carries `title` (the enclosing section's title, or the note's), `ordinal`, `step_count` (the steps the container itself holds; a sub-step counts on its own step), `note_id`, `section_id`, plus `inherit:`.
  - It joins its section by `HAS_<UPPER_SNAKE(container)>`, here `HAS_PROCEDURE`.
- Each item is one node: `text` (the item's own content, excluding any list nested inside it), `ordinal` (0-based), `level`, plus `inherit:`.
  - `edge` joins the container to each top-level step, and a step to the steps of an ordered list nested inside it.
  - `next` joins consecutive steps at one level.

#### `tables:`

```yaml
  tables:
    - {under_heading: '^Parameters$', label: ApiParameter, key_column: name, edge: HAS_PARAMETER}
    - {under_heading: '^Worked at$', edge: WORKED_AT, edges: true}
```

`tables:` is a **list** of rules. Each rule names the heading its tables sit under as a regular expression, matched the way `ordered_lists.under_heading` is. A rule that means the whole heading anchors it (`^Parameters$`), and one that means either case writes `(?i)`.

- The first rule that matches the enclosing section's title reads the table.
- A table under no matching heading is prose like any other.
- A table above the body's first heading sits under no section, and no rule reaches it.
- kglite reads only GFM pipe tables. **Raw HTML is never structure** (§1.4), so a converter emits GFM.

Each rule has one of two forms.

**Node form** (the default) makes one node per body row, labelled `label`.

- Each column becomes a property named by its header text **as written**, typed by `types:` under that label. A cell is text unless a declaration says otherwise, because a cell is a string and not a YAML scalar.
- A **blank** header cell names no property, and its cells are dropped. GFM has no headerless table, so a converter that had none wrote an empty header row. Inventing a positional name would key a corpus to a column order that moves.
- The key column is `key_column:` when declared and the first column otherwise. Its value keys the row as `<section id>~<value>` and is stored under its own column name as well.
- A key that is empty, or that a row above already used, keys on its position instead (`~row<n>`, counting from 1) and is a warning (§9). A `key_column:` the table does not carry also warns, and falls back to the first column.
- `edge` joins the enclosing section, or the note, to each row node. If omitted, it is `HAS_<UPPER_SNAKE(label)>`.

**Edge form** (`edges: true`) states **an edge, not a node**, for each row. `label:` is refused.

- The target is the first column holding a `[[wikilink]]`, or the column `key_column:` names.
- Every other column becomes a string **edge property** on an edge of type `edge:` from the note to that target. An empty cell writes no property.
- The edge also carries `section` (the enclosing heading), `anchor` where the target's own wikilink has a fragment, `label` where it has display text, and `row`, the 1-based row number. A column named like one of those is dropped with a warning, because the link itself states it.
- A table with no target column states nothing, and warns.

Edge form is how a vault states per-edge attributes, such as a role, a weight or a date range. §10.6 writes them back out.

Links and images in a cell follow the prose rules:

- kglite scans a cell's `[[links]]` and `![images]` as prose wherever they sit (§5.1). A picture inside a table cell is the note's attachment reference as usual.
- An edge table's target is *also* the note's `LINKS_TO` edge, and a row rule never swallows either the picture's attachment reference or the `LINKS_TO` edge.
- A link column that resolves to nothing becomes a `_provisional` stub and a warning, as any link does (§5.6, §9).
- A cell's `\|` is the pipe the author meant, in a property value and in a wikilink alike.

#### `key_from_heading:`

```yaml
  key_from_heading: {label: ApiSymbol, when_matches: '^[\w.]+\.[\w]+(\(.*\))?(\s*→.*)?$',
                     property: qualified_name, under_label: Api}
```

`key_from_heading:` relabels a Section whose title is really a symbol name. Two gates apply, and both are required, because the shape is cheap to match by accident:

- `under_label:` restricts the rule to notes carrying that label.
- The heading must contain a `.` or a `(`, whatever `when_matches` says.

On one corpus the regex alone matched 1 439 headings, of which 13 were symbols. A heading like `Overview` is a valid qualified name to a regex and nothing else. The default `when_matches` is the one above: a dotted name, optionally with a call's parentheses and the `→ type` return annotation a converter writes into the same heading.

kglite **splits** the heading where that annotation or the call begins:

- Everything before the first `(` or `→` is stored under `property`. That is the name a query looks up, `rmsapi.Project.open`.
- The rest is stored under `signature`, `(path) → Project`.

Storing the whole title under `property` would only repeat `title`. Relabelling changes the label and adds the two properties. The section's own properties, its id and its section edges are unchanged, and `[[Note#Heading]]` still reaches it: it is the same node under another name.

A **synthetic** heading (§5.8) is a heading here too. Suppose a converter emitted `**open(filename) → Project**` as a bold line rather than as `####`. Once the author writes `<!-- kglite heading -->` above it, it gets the same relabelling, without the file changing.

#### `inherit:` and `embed_text:`

```yaml
  inherit: [corpus, category]
  embed_text: "{title} | {heading_path}\n\n{text}"
```

`inherit:` names frontmatter properties of the note. kglite copies them verbatim onto **every** node derived from the note, so a chunk-level filter or BM25 query needs no hop back to the note. A key the note does not carry is simply absent there.

`inherit:` may not name either of these, and either one is an error (§9):

- A property a derived node defines itself: `title`, `text`, `tags`, `level`, `ordinal`, `path`, `note_id`, `section_id`, `kind`, `lang`, `code`, `caption`, `chunk_hash`, `step_count`, `signature`.
- A reserved frontmatter key (§4.1).

The alternative is a note silently overwriting the structure it was read from.

**`tags`.** Every derived node carries the inline `#tag`s written inside its own range, as a list in first-use order (§5.5). The property is absent from a node that holds none. It is the node's own: a note's frontmatter `tags:` is not copied down, which is why `inherit:` may not name it.

`embed_text:` materialises a property of that name on every derived node that carries `text`. The placeholders are:

- `{title}`: the **note's** title.
- `{section_title}`: the derived node's own title.
- `{heading_path}`: its path joined by ` > `.
- `{text}` and `{id}`.

Any other placeholder is an error. A chunk that carries its document and its heading inside its own text makes a retrieval hit legible without a second query. Declaring the template is what lets `embed: {Chunk: embed_text}` and `text_indexes: {Chunk: [embed_text]}` name it.

`indexes:`, `text_indexes:` and `embed:` name a derived label exactly as they name a note's. The labels do not exist until a rule declares them. Naming one without its rule is the §7 warning for a declaration the vault does not carry.

**Links become labelled.** Declaring `structure:` also turns on the `label` edge property, so `[[Target|the display text]]` stores that text (§5.1, §5.4). There is no separate switch. A vault that models the inside of its notes is a vault that wants its links described, and the property is simply absent everywhere else.

**Compatibility.** `structure:`, `edge_defaults:` and `export:` are unknown top-level keys to any kglite released before them, and an unknown key **fails the build** (§7). A vault using them therefore needs a kglite that knows them. `kglite_vault` stays `1`, because the keys are additive. The failure on an older build is loud and names the key rather than quietly producing a smaller graph.

### 7.2 `edge_defaults:`

```yaml
edge_defaults:
  NEXT_STEP: {derivation: source_order}
  CHILD_OF: {derivation: directory_index_hierarchy}
```

`edge_defaults:` declares edge properties that are constant for a whole type. The build pushes them onto every edge of that type it emits, whether the edge comes from prose, from frontmatter or from a `structure:` rule. It is how a vault states provenance that is true per type without writing it on every line. The declaration is not authored in notes: nothing in a note changes.

A default never overwrites a property the edge already carries (`section`, `anchor`, `alt`, `ordinal`, `label`, an edge table's own columns). That clash is a warning (§9). A type the vault has no edges of is a warning too.

### 7.3 `export:`

```yaml
export:
  edge_tables:
    WORKED_AT: "Worked at"
```

Only the exporter reads `export:` (§10.6). Each entry names an edge type and the heading whose table its edges are written under in the source note. An edge carrying properties then survives an export instead of being counted as a loss. A type with no entry keeps today's behaviour exactly: its targets go to a frontmatter list, and its properties are dropped and counted (§10.9).

The key is an edge type (`UPPER_SNAKE`), and the heading is not blank. Anything else is a config error, like any other.

The export finds this file through the graph's `source_root` provenance (§12), or through the root the caller names. A caller may add or override an entry, and that is how a graph that never was a vault declares one.

Declaring the table does not read it back. Pair it with the `structure.tables … edges: true` rule (§7.1) whose `under_heading:` matches that heading. Otherwise the export warns that the table it wrote is prose (§10.10).

## 8. Skills and recipes carried in the vault

A vault can carry its own agent guidance, so a server built from it explains how to query itself. Both directories are re-read on every **build**. What a running server exposes after that build depends on the kind of record and the server mode. See the lifecycle table at the end of this section.

If a file in either directory fails validation, kglite **skips it with a warning naming the file and the rule, and its siblings load**. The directory is hand-authored vault content, and one unfinished skill must not cost an agent the other nine. The build does not fail: nothing that reached the graph is wrong, there is simply less of it than the author intended.

(These two are the *inputs* under `.kglite/`; `graph.kgl` and `export-manifest.json` beside them are kglite's own output — §2.4.)

- `.kglite/skills/*.md` become `KgliteSkill` nodes. The frontmatter dialect is exactly the one an MCP skills directory uses: `name`, `description`, `references_tools`, `delivery`, then the markdown body. See [Authoring MCP skills](https://kglite.readthedocs.io/en/latest/python/guides/mcp-skills.html).
- `.kglite/recipes/*.md` become `KgliteRecipe` nodes, one stored Cypher query per file. The frontmatter keys are:
  - `recipe`: the group id.
  - `name`.
  - `description`.
  - `recipe_description` (optional): what the group is for.
  - `parameters` (optional): the JSON Schema for the query's `$parameters`, as a nested map.
  - `tool` (optional): an MCP tool name to serve the query under directly.

  The body is the statement, in a fenced ` ```cypher ` block:

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

  **The statement.** It must parse and must be read-only. It must not spell the literal `LIMIT 200`.

  That number is the **recipe result payload cap**. An agent host refuses a recipe result of more than 200 rows outright, naming the count, so a caller always learns that an answer was incomplete. A stored `LIMIT 200` would cap the result at exactly the threshold. The refusal could then never fire, and a truncated answer would read as a whole one.

  The rule is **equality with the cap, not a ceiling**, and it is checked against literals only:

  - `LIMIT 50` is accepted.
  - `LIMIT 201` is accepted. It is simply a query that will be refused on the day it really returns 201 rows.
  - `LIMIT $rows` is never refused, whatever the caller passes.

  A query that wants everything under the cap writes `LIMIT 199`.

  **`parameters` is checked against the statement, exactly.**

  - `properties` must name the same set as the `$parameters` the Cypher uses. One undeclared and one unused are both errors, and the message names each.
  - A property may declare a top-level `default`. Only properties **without** defaults belong in `required`, and every such property must appear there exactly.
  - Omission binds the declared default before validation. An explicit value overrides it. An explicit `null` remains null (and must satisfy the property's type).
  - Defaults nested below a parameter property are rejected.
  - The root carries `type: object`, `properties`, `required` and `additionalProperties: false`. All four are required, `type` must be exactly `object`, and `additionalProperties` must be explicitly `false`. The only other root key allowed is `description`.
  - A file that declares no `parameters:` stores the closed empty schema (`{type: object, properties: {}, required: [], additionalProperties: false}`). That schema is valid only for a statement that uses no `$parameters` at all.

  A file that fails validation is skipped, as above. kglite reads a `parameters:` map **nested**, not flattened into dotted keys, so a schema's `properties.id.type` keeps its three levels.

  **Group description.** If a file omits `recipe_description`, it inherits the group's from **any** sibling that declares it. kglite reads the whole directory before it stores any file, so which file carries the declaration is free, and the group needs exactly one. If no file in a group describes it, every member fails on its own: each is skipped with its own warning, because there is nothing to inherit.

  **`tool:` serves the query as a named MCP tool.** A server reading this vault registers a tool of that name. The tool's description is the query's, and its input schema is the query's `parameters`. An agent therefore calls it in one step instead of naming the query inside `run_recipe_query`'s arguments.

  - The name matches `^[A-Za-z_][A-Za-z0-9_-]{0,63}$`.
  - Two queries cannot claim one name.
  - A name the server has already registered for anything else refuses the boot, naming the owner.
  - The operator can switch the whole mechanism off with `extensions.recipe_tools: false`.

  **Expose a curated few.** Every named tool adds its description and schema to every `tools/list` response. Whether those wire bytes also consume model context depends on the client and its dynamic-discovery strategy; measure the client you deploy. The queries that carry the vault's routine questions earn that surface, and a long tail does not. The catalogue block in `run_recipe_query`'s description marks the ones that have a tool, so nothing is hidden by leaving `tool:` off.

  Registration is **boot-time**. Adding or removing a `tool:` requires a server restart after the changed recipe has been rebuilt into the graph. A rebuild alone is not enough.

### Update lifecycle while serving

| Change | `--vault` | `--graph FILE` |
|---|---|---|
| note content, `vault.yaml`, attachments | The watcher rebuilds before the next tool call; `rebuild_graph` forces it now and returns the report. | Rebuild the source vault to `FILE`, then call `reload_graph` (or restart). Restarting alone only reopens `FILE`; it never converts source files. |
| skill body, description or routing | Rebuild as above; the graph swap re-resolves skills for the session. | Put the changed skill in `FILE`, then `reload_graph`; graph-carried skills re-resolve on that swap. |
| recipe query, schema or description | Restart after rebuilding. The recipe catalogue is fixed at boot, so `rebuild_graph`/`reload_graph` alone does not replace it. | Put the changed recipe in `FILE`, then restart. |
| add, remove or rename a recipe `tool:` | Restart after rebuilding; the MCP tool router is fixed at boot. | Put the changed recipe in `FILE`, then restart. |

If a rebuild or reload fails, the previous graph remains active. This table describes current server behavior. File edits remain build inputs even when a running session needs an additional refresh or restart to expose them.

## 9. Build report and validation

Every build produces a structured report. Two entry points return or print it:

- `okf.validate(path)` returns the report without keeping the graph.
- `kglite okf check <dir>` prints the report and sets the exit code.

Both run the *same* read a build runs. The check is the build with the graph thrown away, never a second opinion about it.

Both read a directory as a **vault** when no dialect is named. `dialect="obsidian"` is what `okf.validate` and `kglite okf check` default to, whereas `okf.build` defaults to `okf` for the bundle callers it has always served. To check a bundle, pass an explicit `dialect="okf"`, which is what its build passes too.

The report carries:

- Files scanned, and how many became notes.
- Nodes per label, and edges per type.
- Folder notes.
- Dangling links, and missing and ambiguous attachments.
- The index / text-index / skill / recipe counts `.kglite/` produced.
- `forced_splits`: chunk boundaries the `chunks:` caps had to place inside a block (§7.1).
- The `embed:` targets declared in `vault.yaml`.
- Two classified lists of findings, errors and warnings. The classification is the contract.

This report proves **format validity**, not source fidelity or answer completeness. Test those promises separately:

1. **Original bytes available:** inventory every source member, including
   hidden files, with path, size and SHA-256; map each source page to its note.
2. **Rendered content preserved:** compare prose, logical table cells, nested
   blocks, warnings, downloads, images and ordering; list unparsed material.
3. **Facts queryable:** assert source-backed expected labels, properties and
   edges, rather than only nonzero counts.
4. **Answers complete and scoped:** evaluate serving with decisive context and
   user constraints present. This is a deployment test, not a format test.

The SHA-256 inventory is independent of §12's stat-based cache fingerprint. The fingerprint detects rebuild inputs cheaply and is not a byte-integrity proof.

### Errors

A vault with any of these does not meet this spec:

| Class | § |
|---|---|
| Unparseable frontmatter (the note still becomes a node, with no properties). | §4 |
| A reserved key of the wrong shape: a non-string `id:` or `type:`, a non-list `tags:` or `aliases:`, a non-boolean `kg_skip:`. | §4.1 |
| An id collision — two or more notes resolving to one id, each falling back to its path. | §3 |
| Two folder notes declared for one directory (`X.md` *and* `X/X.md`). | §2.3 |
| A link or attachment reference naming an absolute filesystem path, or climbing above the vault root — in the body or in a typed-edge key. | §4.3, §5.1, §6.2 |
| A `.kglite/vault.yaml` the schema refuses — an unknown key at the top level or inside `structure:` (§7.1), an unknown `kglite_vault` version, a value of the wrong shape, a malformed `ontology:` document, an `inherit:` entry naming a property a derived node defines itself, or an `embed_text:` placeholder this spec does not name. This **fails the build** (§7); `okf.validate` reports it as the report's single error. | §7 |
| An ontology document the declaration API refuses. | §7 |
| A `<!-- kglite <key>: … -->` naming a reserved key, a derived property, the note's `body:` property, `concept_id` or `file_path`. | §5.8 |

### Warnings

These are legitimate in a real vault and worth seeing:

| Class | § |
|---|---|
| A dangling link: a target that matched no note and became a stub — in the body, in a typed-edge key, or in an edge table's link column (§7.1). | §5.6 |
| A fragment link naming a heading or block id the target note does not have: the edge stays on the note and keeps its `anchor`. | §5.4 |
| A duplicate derived id — a second section with one heading path — which takes a `~2` suffix. The fix is a block id. | §7.1 |
| A table row whose key is empty or already used, which keys on its position instead; a `key_column:` the table does not carry, which falls back to the first column. | §7.1 |
| An edge table with no target column, which states nothing, or one whose column repeats a property the link itself carries, which is dropped. | §7.1 |
| A `structure:` rule that matched nothing anywhere in the vault — for an edge table, one that stated no edge. | §7.1 |
| A `<!-- kglite -->` directive naming no key. Its line is still cut out of the derived text, and it carries no meaning. | §5.8 |
| A `<!-- kglite chunk -->` written inside a list item, a quotation or a table, where there is no chunk of its own to close. | §7.1 |
| A `<!-- kglite <key> -->` with no value, which states nothing. | §5.8 |
| A key stated twice on one node — by two directives, or by a directive and the note's frontmatter. The last directive wins. | §5.8 |
| An `edge_defaults:` entry whose property the edge already carries, or whose edge type the vault has none of. | §7.2 |
| A missing attachment, or an ambiguous bare filename (which resolves to nothing, and the warning names the candidates). | §6.6 |
| A case-insensitive id collision — two ids differing only in case. | §3 |
| An alias clash: an `aliases:` entry that is another note's filename stem, or that two notes both claim. The link resolves to exactly one of them. | §5.2 |
| A declared hub key holding wikilinks, which the typed-edge rule takes instead — reported once per key, naming how many notes. | §4.3, §7 |
| A value that does not match its declared `types:` entry; it is left as written, never nulled. | §7 |
| A `vault.yaml` declaration naming a label or property the vault does not carry, an index that indexed no value, or an index / text index that would not install. | §7 |
| A `.kglite/` skill or recipe file that failed validation and was skipped; its siblings still load. | §8 |
| A `vault.yaml` found under the `okf` or `loose` dialect, where it does not apply. | §7 |

### Exit code and output

- `kglite okf check` exits non-zero when any error is present.
- `--strict` promotes every warning to an error. It is the setting a converter's own test suite should use.
- `--json` prints the same report as a JSON object, for a harness that wants the lists rather than the text.

## 10. Export

Export writes a vault from a graph: `okf.export(graph, dir)` in Python,
`kglite okf export` from the CLI. It targets this format only.

1. **Which nodes.** A note becomes a file, and a synthesized node never does.
   Which nodes count as notes depends on the graph:
   - A graph carrying `file_path` on any node was built from a vault. A node
     **with** `file_path` is a note and becomes a file. A node without one was
     synthesized by the build — a `Folder`, a hub node, an attachment — and is
     not a file.
   - A graph carrying `file_path` nowhere was not built from a vault. Every
     node in it becomes a file.

   Either way, these are never files:
   - `Tag`, `Source`, `Folder`, `Image` and `Attachment` regenerate on the
     next import.
   - `_provisional` stubs are references rather than notes.
   - `KgliteSkill` / `KgliteRecipe` nodes are written to `.kglite/skills/` and
     `.kglite/recipes/` instead (§8).
   - The nodes `structure:` derives (§7.1) are part of a note's prose: a
     section, chunk, callout, example, procedure, step or table row. They
     carry no `file_path`, and the next build derives them again from the body
     the note's own file already holds.
2. **File path.** The export preserves a node's `file_path` when it has one and
   its top-level folder still matches its label. Otherwise it re-files the note
   under `<Label>/`, so the label ladder recovers the label the export did not
   write (§10.3).

   **Re-filing moves the folder and nothing else.** The file keeps the stem it
   arrived with, because a stem is the link namespace (§3, §5.2). A
   `[[wikilink]]` in somebody else's prose spells that stem, not the note's
   title. Renaming the file after the title dangles every one of those links,
   and the next import mints a `_provisional` stub for each.

   Naming rules:
   - Only a node no file ever backed has no stem to keep. It is named
     `<title or id>.md`, and §10.3's `id:` carries the identity the stem does
     not spell.
   - A preserved folder note keeps its `X.md`-beside-`X/` spelling.
   - In a segment the export composes, `/ \ : * ? " < > |` and control
     characters become `-`, and trailing dots and spaces are stripped. Windows
     strips them on write, and a filename that differs from the one the
     manifest recorded would be refused by the next export.
   - A case-insensitive path collision appends `-<id>`.
3. **Frontmatter.**
   - `type:` is never emitted. The folder carries the label, so emitting it
     would make a later folder move a no-op.
   - `id:` is emitted only when the id differs from the filename stem.
   - `title:` is emitted only when the next import would not recover it. §3's
     ladder reads a `name:` key and **the body's first heading** before the
     stem. A note titled after its file but opening with a heading therefore
     still writes its `title:`.

   Value rules:
   - Keys are sorted.
   - Dotted keys expand back into nested maps. The exception is an expansion
     that would have to grow through a property that is already a scalar; that
     keeps the literal dotted key.
   - Lists become YAML sequences. A list or map *inside* a sequence is written
     as JSON, which is YAML flow syntax.
   - Dates and datetimes become ISO strings.
   - Points become `POINT(lon lat)` WKT.
   - A whole float keeps its `.0`.

   Never frontmatter: `file_path`, `concept_id`, `_provisional`, the body
   property and the attachment-derived properties. Embeddings are not
   properties at all. They live in the graph's own store, and no export writes
   them.
4. **Quoting.** The export quotes a string when, unquoted, it would come back
   as something else. It checks these conditions in order:
   1. The string is empty or padded with whitespace.
   2. It starts with a YAML indicator character (`-?:,[]{}#&*!|>'"%@` or a
      backtick). This is what quotes a `[[wikilink]]`, since bare `[[A]]` is a
      flow sequence and not the name it spells.
   3. It contains `: `, a ` #` comment opener or a newline, or ends in `:`.
   4. It spells a boolean or null in any of YAML's casings, including the 1.1
      words `y`/`n`/`yes`/`no`/`on`/`off`.
   5. It parses as an integer or a float, or starts `0x`/`0o`.
   6. It matches a date or datetime §4.2 would infer. This asks the reader's
      own inference, so the two cannot disagree.

   Everything else is written bare.
5. **Body.** The export writes the `body` property verbatim, starting on the
   line after the closing `---` with no blank line inserted. The reader keeps
   everything below the terminator, so a separator written here would come back
   *as* body and the next export would write another one. A note whose author
   left a blank line there has it in its body and gets it back.
   - A node without a body produces a frontmatter-only file.
   - A node with a body and nothing to say above it produces a file with no
     frontmatter block at all.

   Human-owned prose is never rewritten, with one declared exception. The table
   under a heading `export.edge_tables` names (§10.6) belongs to the export,
   which rewrites it whole or appends it where it is missing. The vault asked
   for that table by name; nothing else in the body is touched.
6. **Edges** become frontmatter lists keyed `lower_snake(TYPE)`, with wikilink
   values: `depends_on: ["[[Seismic interpretation]]"]`. The key is exactly
   what §4.3's `UPPER_SNAKE(key)` turns back into that type.

   Two kinds of edge are left out:
   - **Every edge whose target is not a file.** `CONTAINS` (it leaves a
     `Folder`), `TAGGED`, `HAS_IMAGE`, `HAS_ATTACHMENT` and every hub edge
     leave this way. None is named as a special case, because a target that is
     not a file has no wikilink to name it.
   - **An edge the body already states.** The export re-reads the prose with
     the reader's own scanner. It leaves an edge out when a body link reaches
     the same target *with the same type*. That is the type §5.3's ladder
     gives it — a `{type}` suffix as much as a heading rung, not `LINKS_TO` by
     assumption.

   The type has to match both ways. Writing an edge the body already states
   makes a second edge on the next import: one carrying the body's `section`
   and one carrying nothing. Dropping one whose type differs from what the
   body's link would produce retypes it.

   Target spelling:
   - An edge to a `_provisional` stub is written as `[[<the unresolved name>]]`,
     so a dangling link declared in frontmatter dangles in the same place next
     time.
   - An ambiguous target is written folder-qualified, `[[Label/Name]]`.

   **An edge whose type `export.edge_tables` declares** (§7.3) is written as a
   table in the body instead of a frontmatter list, and keeps its properties.
   This is the only place an export adds prose to a note, and it does so
   because the vault asked for it. An undeclared type is never written into a
   body, because human prose is never rewritten (§10.5). Its properties are
   counted as loss 1 exactly as before.

   **The declared heading's first table is the exporter's.** The export
   rewrites that table whole — header row, delimiter row and rows — keeping
   only the name the author gave its first column. Where the table goes:
   - Where the heading carries a table, the export rewrites it.
   - Where the heading carries no table, the export writes one at the end of
     what that heading itself holds, before the next heading of any level.
   - Where the heading is absent, the export appends `## <heading>` and the
     table to the body.

   A table under a *nested* heading belongs to that heading and is left alone.
   A declared type with no edges to write **removes** the table the export
   owns, because leaving it would make those edges again on the next import.
   That ownership is what makes an exported vault a fixed point. The second
   export finds its own table and replaces it, rather than appending a second
   one.

   Table layout:
   - The first column holds the `[[target]]`, carrying the edge's `anchor` as
     its fragment and its `label` as its display text.
   - Every **other** property gets a column, named for it, in name order.
   - `section`, `anchor`, `row` and `label` get no column, because the reader
     mints all four from the table itself (§7.1).
   - Rows are ordered by `row` where the edges carry one, which is the order
     the author's own table had. Without `row`, rows are ordered by target and
     then by their columns.
   - A cell is text, so a property that was not a string comes back a string.
     `row` is the exception, minted as an integer by the reader.

   Reading the table back needs the matching `structure.tables … edges: true`
   rule (§7.1). That rule lives in `vault.yaml`, which no export writes (loss
   4). Copy that file across, or the exported table reads as prose. An export
   **warns** (§10.10) in two cases:
   - A declared type has no such rule in the source vault's own `vault.yaml`.
   - No exported note emits the type at all, which is what a type only derived
     nodes emit looks like (§10.1).

   The table's own cells are links like any other. The next import therefore
   reads a `LINKS_TO` (or whatever the heading ladder types) beside the
   declared edge, exactly as it does from an author's own table.
7. **Overwrite safety.** `.kglite/export-manifest.json` records every file the
   export wrote: `{"kglite_vault": 1, "files": {"<vault-relative path>": "<sha256 hex>"}}`.
   It is one of the two files kglite writes into `.kglite/` and does not read
   back as content (§2.4), so it does not move the vault's fingerprint.

   On the next export, the manifest decides what the export may touch:
   - A file whose current hash differs from its manifest entry was edited by a
     human, and the export refuses it.
   - A file absent from the manifest was written by somebody else, and the
     export refuses that too.
   - `force` overrides both, and nothing else.
   - A refused file stays in the manifest, so the deletion pass does not read
     it as a file whose node disappeared.
   - A file absent from the manifest is never deleted at all.
   - A node deleted from the graph removes its file only when the manifest owns
     it and the bytes still match.

   A file whose bytes already equal what the export would write is not
   rewritten. An unchanged export therefore moves no modification times —
   attachments included, so a re-export of a vault of ten thousand pictures
   rewrites none of them. A copied attachment is written with the *source*
   file's modification time rather than the time of the copy, so the `mtime`
   §6.3 stats off it reads the same on both sides. A manifest that will not
   parse, or that names a version this build does not write, fails the export
   rather than being guessed at.
8. **Determinism.** The export sorts frontmatter keys, edge lists and the
   manifest's own keys, and keeps file order stable. Exporting the same graph
   twice is byte-identical.
9. **Documented losses.** The export has six losses, and no others:

   1. **Edge properties** (`section`, `anchor`, `alt`, `ordinal`, `label`) are
      not written for a type `export.edge_tables` does not declare, because a
      frontmatter list carries targets. The export report counts them. A
      declared type keeps them, as a table (§10.6). An edge the *body* states
      keeps them anyway: the prose travels verbatim, and the next import
      re-derives them from it with the same scanner. An edge only frontmatter
      carried has none to begin with. The loss therefore bites exactly where an
      edge with properties was never written in prose — a graph that was not
      built from a vault.
   2. **Attachment bytes** are copied only when the caller names the source
      root the graph was built from. Otherwise the references are reported as
      unresolvable and come back as `missing: true` stubs (§6.6). A copy that
      *does* travel keeps the source file's modification time (§10.7). So
      §6.3's `mtime` is not one of these losses, and the fixed point does not
      depend on which second the export ran in.
   3. **Synthesized nodes are not files.** `Folder`, `Tag`, `Image`,
      `Attachment` and stub nodes are whatever the exported layout regenerates.
      Where the prose decides, they are the same ones. The `Folder`s differ,
      because the layout is now one folder per label.
   4. **`.kglite/vault.yaml` is not written**, because a graph does not carry
      it. What it declared is gone: hub nodes and their edges, the index and
      text-index declarations, the `embed:` targets, and `heading_edges`
      retyping. The retyping shows up as the built-in ladder's own type
      appearing *beside* the declared one. The typed edge is written as a
      frontmatter key, and the prose still reads as what the ladder says.
   5. **A declared `type:` survives only where §4.2's inference agrees with
      it.** The writer emits an `int` as an int and a quoted string quoted, so
      most declarations are re-derived for free. The exception is temporal: a
      top-level string that looks like a date comes back a **date**, because
      quoting alone does not stop inference and only `types:` does.
   6. **A re-filed note carries its body verbatim.** A note-relative reference
      in it therefore resolves from the new location, not the old one. That is
      a §9 error when the climb now leaves the vault, and a different file when
      the same name exists in both places.

   Round-tripping is defined against exactly those losses: importing an
   exported vault reproduces the imported graph apart from them. They are taken
   **once**, on the way out of the author's vault, so an exported tree is a
   fixed point. Exporting it, reading it back and exporting it again is
   byte-identical, and so is the graph. Two things move the bytes at all, and
   only on the first export after them: loss 5, and a declared edge table,
   whose table the export owns and writes in its own spelling (§10.6).
10. **Warnings.** The report carries a line per declared edge table the export
    could not write as asked:
    - A type whose source vault declares no `structure.tables … edges: true`
      rule to read the table back.
    - A type no exported note emits.
    - A source `vault.yaml` that would not parse.

    None of them fails the export. Each is a table the author will not get
    back, said at the moment it can still be fixed rather than three steps
    later as a missing edge.

## 11. Converter checklist

Recommended converter output, in order:

1. **One `.md` file per source document**, UTF-8, under a directory tree that
   mirrors the hierarchy you want. Use the folder-note layout (§2.3): `X.md`
   beside `X/`. A folder note at the **vault root** has no folder above it to
   take a label from, so give each one a `type:` (§2.3).
2. **A stable `id:`** whenever the source has a durable identifier (a GUID, an
   accession number). Without one the filename stem is the id. That is fine for
   a hand-kept vault and fragile for a generated one.
3. **A `title:`**, unless the filename stem is already the title.
4. **Properties as frontmatter keys**, typed naturally: numbers unquoted, lists
   as YAML sequences, dates as `YYYY-MM-DD`. Do not JSON-encode anything.
5. **Typed edges as wikilink-valued keys** (§4.3), and extra parents as
   `parent:`. Do not invent a property that duplicates an edge.
6. **Body links as `[[Stem]]`**, folder-qualified `[[Label/Stem]]` when a stem
   is ambiguous. When several links all mean one relationship, group them under
   a heading and map that heading in `heading_edges:`.
7. **Images as note-relative or vault-relative references**, and **copy the
   files into the vault**. A reference that resolved in the source tree does
   not resolve after you reorganise the output. Convert to PNG, JPEG, GIF or
   WebP (§6).
   - A download link is the same thing without the `!`: `[the handbook](x.pdf)`
     is an `Attachment` reference, and the file has to be in the vault too.
   - **Percent-encode every markdown target you emit** — at minimum spaces,
     `(`, `)` and a literal `%`, which otherwise truncate or hide the reference
     (§6.1).
   - Keep `#` and `?` out of the filenames themselves, because no encoding
     reaches them.
   - Wikilink targets are never encoded.
8. **`.kglite/vault.yaml`** with `kglite_vault: 1`, your `default_label`,
   `folder_notes`, `hubs`, `heading_edges`, `types`, `indexes`, `text_indexes`
   and `embed` (§7). Add `structure:` when the notes carry structure worth
   querying, which §13 is the guide to. The file replaces the graph-building
   script: everything declarative lives here and is re-applied on every
   rebuild.
9. **`.kglite/skills/` and `.kglite/recipes/`** when the vault is served to an
   agent (§8). Do not write `.kglite/graph.kgl` by hand, because that is the
   cache `okf.open` / `kglite okf open` maintains (§12). Treat it as a
   disposable accelerator: a copy at another path rebuilds once before it can be
   reused.
10. **Run `kglite okf check <dir>`.** Zero errors is the bar.
    - Add `--strict` to your own test suite once the warnings are down to the
      ones you accept.
    - Add `--json` when the suite wants the finding lists rather than the text.

`examples/html_to_vault.py` in this repository is a worked converter following
exactly this checklist: HTML pages plus a JSON table of contents in, a
validated vault out. It is the fastest way to see each step in code. It shows
what item 1 means for a corpus that routes by directory. A page's identity is
its **path** below the source root, `<dir>/index.html` naming `<dir>`, because
a site serving clean URLs has one filename for every page in it.

## 12. Rebuild and provenance

A build stamps the graph with five values:

| Stamp | Records |
|---|---|
| `source_root` | the absolute directory the build walked |
| `source_fingerprint` | a 64-bit summary of the `(relative path, size, modification time)` of every file the build read: each note, each attachment, and every build input under `.kglite/` |
| `source_dialect` | the dialect the build read the files with |
| `source_build_version` | the version of kglite that built the graph |
| `source_options` | the non-dialect build knobs (`require_frontmatter`, `respect_skip`, `skip_dirs`, `with_body`) the build read with |

The last two record what the fingerprint cannot see. One untouched directory
fingerprints identically across a release that changed how notes are read, and
across two callers who passed different knobs, and those are different graphs.
All five are persisted in the `.kgl`. A process that opens one later can ask
whether the vault behind it has moved on, and whether this build would read it
the same way, without being told the path or the conventions again. A `.kgl`
written before 0.17.12 carries the first three and not the last two.

The dialect belongs in the stamp because the fingerprint depends on it:
- `.kglite/` is a build input for `obsidian` alone.
- The dialect decides which files are notes at all.
- So one untouched directory has one fingerprint per dialect, and a caller
  comparing across two of them is told "changed" every time.

`okf.rebuild_if_changed(graph)` and `kglite okf status <dir> --graph f.kgl`
read the dialect off the stamp when they are not given one. They refuse a given
dialect that contradicts the stamp, naming both. A `.kgl` written before the
dialect was stamped (0.17.8–0.17.10) keeps the old behaviour: the caller's
dialect, `okf` when they name none. The rebuild report says the stamp was
missing.

`okf.fingerprint(dir)` recomputes the fingerprint (a `stat` pass; no note is
read). `kglite okf status <dir> [--graph f.kgl]` prints it and exits non-zero
when the graph is stale. A *path* carries no stamp, so `okf.fingerprint` still
defaults to `dialect="okf"` and must be told `"obsidian"` for a vault.

`okf.rebuild_if_changed(graph)` returns `None` when the fingerprint still
matches and a **new** graph otherwise. The new graph carries the old graph's
vectors across by `(label, id)`. An unchanged note keeps its vector and its
stored text hash, so only notes whose text moved are re-embedded.

Two cases differ:
- Nodes a `structure:` block derived add one fallback, because their ids move
  when the prose around them does. A derived node whose id is new but whose
  `chunk_hash` matches exactly one old node of its label is that node, and
  carries its vector and hash across too (§7.1).
- A note that changed **label** — by moving between folders under a
  folder-derived label — is a different node and re-embeds. That is the
  contract, not a defect.

`embed:` targets then run a changed-mode pass when a model is bound.

### The graph a vault carries

`okf.open(dir)`, `kglite okf open <dir>` and the MCP server's `--vault` mode do
the whole of the above in one call. Each loads the vault's own graph, asks the
fingerprint whether the directory has moved, rebuilds only if it has, and writes
the result back.

The default cache lives at **`.kglite/graph.kgl`**. `okf.is_cache_artifact`
excludes it from the fingerprint, along with its `.lock` and `.lock-owner`
siblings, the in-flight `graph.kgl.tmp.*` of a save, and
`.kglite/export-manifest.json`. Writing the cache therefore does not mark the
vault it describes as changed. A vault may be *shipped* with its graph, and a
machine opening the same canonical path may reuse it without a build.

Copying or moving the vault changes that path and deliberately causes one
rebuild. If the cache is writable, later unchanged opens at the new location
reuse the refreshed cache. Shipping the cache is an optional, conditional
acceleration, never a portability guarantee.

Five things make the cache a miss rather than a hit, and every one of them
rebuilds silently:
- The file is absent or this build cannot read it.
- The vault was moved or copied (the stamped `source_root` is not this
  directory).
- The cache was built by another version of kglite.
- The cache was built with other option knobs.
- The cache carries no dialect stamp to compare under.

**A cache problem never fails an open.** A cache that cannot be written — a
read-only vault, a full volume, another process holding the writer lease —
leaves a warning in the build report and returns the graph anyway. The cost is
one more rebuild next time.

Relocate the cache with `--cache PATH` / `cache=`, or switch it off with
`--cache none` / `cache=False`. A `.kgl` anywhere in the vault other than
`.kglite/graph.kgl` is **not** a cache. Every non-hidden file under the root is
a candidate attachment, so a graph written beside the notes changes the vault it
describes. Keep the graph in `.kglite/`, or outside the vault entirely. An
explicit cache path outside the vault is recommended when the source tree must
stay read-only, clean or independently packaged.

Two consequences are worth knowing:
- Modification times are compared as whole seconds, so a file rewritten within
  the same second to exactly the same length reads as unchanged.
- The stamp does not record which embedder computed the vectors. Swapping
  models and reusing a cache serves the old model's vectors until the notes
  behind them change.

## 13. Modelling guide for converters and authoring agents

A source corpus already has structure: a table of contents, sections,
procedures, parameter tables, admonitions. This section says what to write so
that structure arrives in the graph, and what to avoid writing because it
arrives as nothing. These are recommendations for preserving meaning, not
additional validity requirements.

For the complete runnable sequence from source inventory through conversion,
reconciliation, paginated queries, serving and a clean public share, use the
[worked help-vault tutorial](https://kglite.readthedocs.io/en/latest/python/guides/help-vault.html). Its companion
example is `examples/knowledge_base/knowledge_base.py`.

> **Frontmatter is the node, headings are the sections, `^blockid` is the
> citable unit.** Everything below follows from those three.

§13.4 and §13.5 cover the other direction: annotating a corpus that is already
converted. The
[OKF guide](https://kglite.readthedocs.io/en/latest/python/guides/okf.html#annotating-a-vault-in-place)
walks all four in-note annotations on one page with the Cypher that reads each
back.

### 13.1 What to emit

| Source has … | Emit … | Graph gets … |
|---|---|---|
| a hierarchy or table of contents | folders plus folder notes — `X.md` beside `X/` (§2.3) | a `CHILD_OF` chain, labels from the folders |
| sections within a page | headings, one level per depth | `Section` with `PARENT_SECTION` / `NEXT_SECTION` (§7.1) |
| a passage worth citing | a paragraph ending in ` ^id` | a `Chunk` keyed `Note#^id` that later edits cannot move |
| an ordered procedure | a numbered list | `Procedure` + `ProcedureStep` + `NEXT_STEP` |
| parameters, fields, columns | a **GFM** table under a named heading | one node per row, columns as properties |
| notes, warnings, version remarks | callouts — `> [!versionadded] Title` | `Note {kind, title, text}` |
| code samples | fenced blocks **with the language on the fence** | `Example {lang, code}` |
| typed relations | frontmatter `key: ["[[A]]", "[[B]]"]` (§4.3) | `KEY` edges |
| relations **with attributes** | an edge table under a declared heading (§7.1) | `KEY` edges carrying properties |
| what a link means in prose | `[[Target\|the words you would have written]]` | the edge's `label` |
| categorical facets — tags, keywords, components | list-valued frontmatter keys plus `hubs:` | hub nodes and their edges |
| images and downloads | note-relative references, files copied in, PNG/JPEG/GIF/WebP | `Image` / `Attachment` nodes and edges (§6) |
| a heading that is really a symbol name | `key_from_heading:` with `under_label:` | the section relabelled, `qualified_name` stored |
| a fact about **one section** that the prose states in words — a menu path, an owner, a version | a directive under that heading (§5.8) | that property on the `Section`, out of its `text` |
| provenance that is constant per edge type | `edge_defaults:` (§7.2) | that property on every edge of the type |
| anything to search or embed | `text_indexes:` / `embed:` on `Chunk` and `Section`, with `embed_text:` | BM25 and vectors at the granularity that answers |

### 13.2 Anti-patterns

- **One file per tiny record** — per chunk, step or parameter. It destroys the
  property that makes a vault worth having: that a human can open it. A
  `structure:` block yields the same nodes from the same pages.
- **Raw HTML tables and definition lists.** HTML tags are never interpreted
  (§1.4), so a `<table>` is prose and its rows are nothing. Emit a GFM table.
  For a `<dl>`, emit headings or a table, never `<dt>`.
- **Admonitions flattened into paragraphs**, or folded into `note` because the
  source word is not one of Obsidian's thirteen. Callout kinds are arbitrary.
  Keep `versionadded` and keep `deprecated` (§5.7).
- **Unlabelled fences** when the source knew the language. `langs: [python]`
  then matches nothing. Write the language, or omit `langs:` and take them all.
- **JSON-encoded lists.** `tags: "[\"a\", \"b\"]"` is one string. Write a YAML
  sequence (§4.2).
- **Nested frontmatter maps.** They flatten to dotted keys that Obsidian's own
  Properties UI cannot edit. Keep frontmatter to scalars and lists.
- **`type:` on every file when the folder already says it.** The label ladder
  reads the folder (§2.1), and an export never writes `type:` back (§10.3). The
  declaration only makes a later folder move a no-op.
- **`default_label:` in a vault whose folders are its labels.** It is rung 2
  and the folder is rung 3 (§2.1), so it wins over every folder and relabels
  the whole vault. A converter that declared `default_label: Article` and a
  `types:` block per folder label got 1 237 `Article`s and no `Api`, with
  nothing but the per-label counts in the report to say so.
- **Absolute paths and `../` climbs** out of the vault. Each is a §9 error, in
  prose and in a typed-edge key alike.
- **SVG.** It is stored as an `Attachment` and is not delivered as an image.
  Rasterise it when you build the source (§6).
- **The same heading text twice in one note.** Obsidian can link only the first,
  and so can this spec. The second takes a `~2` id and a warning. Add a block
  id.
- **`[`, `]`, `|` or `#` inside a heading you link to.** `[[Note#Heading]]` has
  no escape for any of them: `#` starts the next path component, `|` starts the
  display text, and `[[`/`]]` end the link. The section is still built; only
  the *link* cannot be written. Rename the heading, or cite a `^blockid` on the
  paragraph below it.

### 13.3 A `structure:` block to start from

```yaml
# .kglite/vault.yaml — a converted help corpus
kglite_vault: 1

structure:
  sections: {label: Section, edge: HAS_SECTION, parent: PARENT_SECTION, next: NEXT_SECTION}
  chunks:   {label: Chunk, edge: HAS_CHUNK, next: NEXT_CHUNK, max_words: 650, max_chars: 6000}
  callouts: {label: Note, edge: HAS_NOTE}
  code_fences: {label: Example, edge: HAS_EXAMPLE}
  ordered_lists: {label: ProcedureStep, container: Procedure, edge: HAS_STEP, next: NEXT_STEP}
  tables:
    - {under_heading: "Parameters", label: ApiParameter, key_column: name, edge: HAS_PARAMETER}
    - {under_heading: "Returns", label: ApiReturn, key_column: name, edge: HAS_RETURN}
    - {under_heading: "Exceptions", label: ApiException, key_column: condition_id, edge: RAISES}
  key_from_heading: {label: ApiSymbol, property: qualified_name, under_label: Api}
  inherit: [corpus, category]
  embed_text: "{title} | {heading_path}\n\n{text}"

edge_defaults:
  NEXT_STEP: {derivation: source_order}

text_indexes:
  Chunk: [embed_text]
  ProcedureStep: [text]
embed:
  Chunk: embed_text
```

The block has no `default_label:` on purpose. It is rung 2 of the label ladder
(§2.1) and the folder name is rung 3, so declaring one labels *every* note with
it and a converted corpus's folders stop saying anything. Declare it only for a
vault whose notes are genuinely one kind, as §7's example is.

`code_fences:` here omits `langs:` on purpose. The corpus lost its languages in
conversion, and every fence is still an example. Fix the converter, and the
filter becomes worth declaring.

The general starter above selects notes labelled `Api`. The bounded fixture
labels all three source pages `Article`, so its checked-in `vault.yaml` uses
`under_label: Article` for the same rule. Use the label your converter emits.

For API reference pages, a Parameters table alone is not a contract. Preserve
the owning symbol, complete signature, return, every exception condition, and
an exact source handle. The converter in the
[worked help-vault example](https://kglite.readthedocs.io/en/latest/python/guides/help-vault.html)
emits this shape (hashes shortened here only for readability):

```markdown
## Client.connect(controller_id: str, timeout: int = 30, options: dict = {"mode": "safe"}) → Session

<!-- kglite owner: Client -->
<!-- kglite returns: Session -->
<!-- kglite source_signature: connect(controller_id: str, timeout: int = 30, options: dict = {"mode": "safe"}) -> Session -->
<!-- kglite provenance: Sources/api.html#connect -->
<!-- kglite source_sha256: <sha256> -->

### Parameters
| name | type | default | owner | source_anchor | source_sha256 |
| --- | --- | --- | --- | --- | --- |
| options | dict | {"mode": "safe"} | Client | api.html#connect | <sha256> |

### Returns
| name | type | owner | source_anchor | source_sha256 |
| --- | --- | --- | --- | --- |
| return | Session | Client | api.html#connect | <sha256> |

### Exceptions
| condition_id | condition | exception | owner | source_anchor | source_sha256 |
| --- | --- | --- | --- | --- | --- |
| value-error-empty-id | controller_id is empty | ValueError | Client | api.html#connect | <sha256> |
| value-error-negative-timeout | timeout is negative | ValueError | Client | api.html#connect | <sha256> |
```

These directive and column names are authoring conventions, not reserved vault
keys. How the shape maps to the graph:
- `key_from_heading` derives the `ApiSymbol`, its normalized `signature` and its
  `qualified_name` from the heading.
- The directives attach the owner, return, exact unnormalized
  `source_signature`, provenance and hash.
- Each table rule creates facts from its own child heading `Section`. That
  section points to the symbol with `PARENT_SECTION`.

Three rules keep the facts faithful:
- Preserve a quoted or nested default as source text. Do not reinterpret
  `{"mode": "safe"}` into a new value.
- Key an exception by a stable condition identity, not by exception type. Both
  rows above must survive even though both raise `ValueError`.
- Keep the source anchor and hash, which make each extracted fact reviewable
  after regeneration.

The resulting facts are independently queryable:

```cypher
MATCH (s:ApiSymbol {qualified_name: 'Client.connect'})
RETURN s.owner, s.signature, s.source_signature, s.returns,
       s.provenance, s.source_sha256
```

```cypher
MATCH (s:ApiSymbol {qualified_name: 'Client.connect'})
      <-[:PARENT_SECTION]-(h:Section)-[:HAS_PARAMETER]->(p:ApiParameter)
WHERE h.title = 'Parameters'
RETURN p.name, p.type, p.default, p.source_anchor, p.source_sha256
ORDER BY p.name
```

```cypher
MATCH (s:ApiSymbol {qualified_name: 'Client.connect'})
      <-[:PARENT_SECTION]-(h:Section)-[:HAS_RETURN]->(r:ApiReturn)
WHERE h.title = 'Returns'
RETURN r.name, r.type, r.source_anchor, r.source_sha256
```

```cypher
MATCH (s:ApiSymbol {qualified_name: 'Client.connect'})
      <-[:PARENT_SECTION]-(h:Section)-[:RAISES]->(e:ApiException)
WHERE h.title = 'Exceptions'
RETURN e.condition_id, e.exception, e.condition, e.source_anchor, e.source_sha256
ORDER BY e.condition_id
```

Test the values, not just nonzero counts. This example must return one symbol,
the exact nested default, one `Session` return, and two distinct `ValueError`
conditions with their source evidence.

### 13.4 Stating a fact the prose only says in words

A help corpus writes the way to reach a dialog into the sentence that
introduces it:

```markdown
### Annotation Table

To open the **Annotation Table** dialog box, click the button on the
**Wells** task pane.

<!-- kglite address: Data tree -> Wells | Task pane: Wells -> Annotations table -->
<!-- kglite documented_in: [[Wells]] -->
```

The two directives (§5.8) state on the `Annotation Table` **Section** what the
paragraph only implies: an `address` property and a `DOCUMENTED_IN` edge to the
`Wells` note. Neither reaches the section's `text` or the chunk packed from it,
so retrieval still sees only the prose. The file still renders in Obsidian
exactly as it did.

```cypher
MATCH (s:Section) WHERE s.address CONTAINS 'Task pane'
RETURN s.title, s.address
```

Reach for this when the fact is **about one section** and the alternative is a
frontmatter key about the whole note, which would be wrong, or a table nobody
reads. A fact about the whole note still belongs in frontmatter
(§4.3).

### 13.5 Tagging what a paragraph is, not what the page is about

A page mixes kinds of statement: what the reader can do, and what will go
wrong if they do it. Both are one sentence inside a longer section, so
frontmatter cannot carry either.

```markdown
## Importing wells

Use the import dialog to load a deviation survey. #intent/import-wells

The datum is not checked on import. #warning
```

Declare `tag_labels: {"intent/*": {label: Intent, edge: HAS_INTENT}}` (§5.5).
Then the two tags behave differently:
- The first tag mints an `Intent` node `import-wells`, joined from the
  **chunk** that holds it.
- The second stays an ordinary tag, but it lands in that chunk's own `tags`
  list as well.

```cypher
MATCH (c:Chunk)-[:HAS_INTENT]->(i:Intent) RETURN i.title, c.text
MATCH (c:Chunk) WHERE 'warning' IN c.tags RETURN c.concept_id, c.text
```

Use `tag_labels:` when a family of tags is really a **kind of thing** the
corpus has many of and queries by name (`intent/…`, `task/…`, `product/…`).
Leave a one-word marker like `#warning` as a tag, where the per-chunk `tags`
list already answers for it.

Mind the chunk's width. Two paragraphs often pack into one chunk, so a marker
meant for one of them marks both. To separate them, do one of these:
- Write `<!-- kglite chunk -->` between the paragraphs (§5.8).
- Give the paragraph a `^block-id` (§5.7).

### 13.6 The loop

1. Convert a **sample** — fifty pages, not the corpus.
2. Run `kglite okf check <dir> --strict`. Errors are spec violations. Warnings
   at this stage are usually the converter's, not the corpus's.
3. Read the report's per-label counts against what you know the sample holds. A
   rule that matched nothing is a warning and the fastest defect there is
   (§7.1). Likely causes: the heading regex is wrong, or the tables are still HTML.
4. Reconcile the sample against a source inventory and explicit expected facts
   (§9). Preserve defaults and exception conditions as source text when their
   expression is nested or quoted. Key repeated exceptions by condition and
   source anchor, not only by exception type.
5. Fix the converter, not the generated vault. Keep operator workflows,
   memories and reviewed annotations in separately owned overlays. Give each
   overlay stable ids, exact source anchors/hashes, precedence and
   stale-evidence checks.
6. Then run the whole corpus, and keep `--strict` in the converter's own test
   suite (§11.10). Traverse every paginated recipe with a stable ordering and
   continuation, and assert no missing or duplicated ids. A row cap or a
   non-truncated first response does not prove that arrays, previews, examples
   or text inside each row are complete.
