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
   as `[[Faults]]` from anywhere in the vault; a `id:` in frontmatter overrides
   it (§3).
3. **A wikilink-valued key is an edge, not a property.**
   `depends_on: "[[Horizons]]"` makes a `DEPENDS_ON` edge and stores nothing
   (§4.3).
4. **Images are note-relative and live in the vault.** `![alt](img/x.png)`
   becomes an `Image` node — copy the file in; a path out of the vault is an
   error (§6).
5. **The body is prose.** It is stored whole and searched, and nothing in it is
   ever rewritten or reformatted (§4.2). It is *split* into nodes of its own —
   sections, chunks, callouts, steps, table rows — only where
   `.kglite/vault.yaml` declares a `structure:` block (§7.1); with no such block
   a note is one node holding one body.

Then run `kglite okf check <dir>`. **Errors** mean the vault does not meet this
spec; **warnings** — a dangling link, a missing image — are normal in a vault
being written. `--strict` fails on warnings too (§9).

**Writing a converter, or authoring for one?** §13 is the modelling guide: what
to emit for each shape a source already has, and the anti-patterns that cost a
corpus its structure on the way in.

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
4. Never interpreted: HTML **tags**, canvas files, Dataview inline fields
   (`key:: value`) and Logseq properties. Heading-level splitting is not done
   either unless the vault asks for it: a `structure:` block in
   `.kglite/vault.yaml` (§7.1) derives nodes from a note's own headings,
   paragraphs, callouts, lists, fences and tables, and without one a note stays
   a single node holding a single body.
   An `<a href>` is not a link and an `<img src>` is not an attachment
   reference. A line holding HTML is still prose, though: the markdown link and
   image syntax written *inside* an HTML block is scanned exactly as it is
   anywhere else (§5.1). Nothing here is a block-level exemption — the reader
   has no HTML parser to give it one.

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
parents are declared with `parent:` (§4.3); a `parent:` naming the folder note
the layout already joined this note to is the same relationship, and one edge.

A folder note is **labelled from where its folder sits**, not from inside it:
`X/X.md` takes the label rung 3 (§2.1) gives its directory's *parent*, so both
spellings of one folder note produce the same label. `Geology.md` and
`Geology/Geology.md` at the vault root are therefore both `Note`, not
`Geology`.

That last sentence catches converters out, so here it is as a tree. A corpus
split into two top-level sections writes

```
Api.md                # the folder note for Api/
Api/
  rmsapi.md           # (:Api)
Software.md           # the folder note for Software/
Software/
  panels.md           # (:Software)
```

and gets `:Api` and `:Software` on the notes *inside* the sections, but `Note`
on `Api.md` and `Software.md` themselves: they sit at the vault root, which has
no top-level folder for rung 3 to read. The ladder is doing what it says, and
the fix is one line per root — `type: Api` in `Api.md`. `default_label:` is not
the fix, because rung 2 sits ahead of rung 3 and would relabel every note in
the vault.

The folder-note edge joins **notes**. A plain subdirectory below a folder note
keeps its `Folder` node and its `CONTAINS` edge from the note — the note stands
in for a directory, so it contains what that directory contained.

Declaring **both** spellings for one directory is an error (§9): two notes
cannot both stand for `X/`. `X.md` is the one used, so the build still produces
a hierarchy.

Those two are the *only* folder-note spellings. `X/index.md` is not one —
`index.md` is an ordinary note in this dialect (§2.4) — so a tree whose section
pages are all called `index.md` gets no folder notes at all, and any two of them
collide on the stem `index` (§3) instead.

A generated tree hits the both-spellings error without meaning to, because
`X/X.md` is a name a converter writes for its own reasons. A Python API mirror
lays out `api/rmsapi.md` for the package and `api/rmsapi/` for its members —
and one of those members is the module `rmsapi` itself, so it writes
`api/rmsapi/rmsapi.md`. Both files now claim `api/rmsapi/`, the second by
accident. Rename the member page (`api/rmsapi/rmsapi_module.md`); an `id:`
will not settle it, because the layout reads paths, not ids.

Renaming the member page moves nothing else. `api/rmsapi/` is still the
directory beside `api/rmsapi.md`, so `api/rmsapi.md` is still its folder note
and every note inside keeps it as their folder-note parent — the renamed page
included, which becomes an ordinary child of the package page, which is what it
is. **Do not rename the directory instead.** `X.md` is a folder note only while
`X/` sits beside it, so renaming the directory dissolves the folder note: the
directory gets its `Folder` node back and every `CHILD_OF` under it becomes a
`CONTAINS` from that node — the hierarchy the layout was there to express. The
other spelling is a valid fix too — rename the page *beside* the directory and
let `api/rmsapi/rmsapi.md` stand for it — but then the package page is the
parent of nothing: it and the folder note end up siblings under `api/`. Rename
the member.

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
| **title** | frontmatter `title:` → frontmatter `name:` → the body's first heading, of any level → the filename stem |
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
| `cssclasses` | Obsidian's own styling key: which CSS snippets render this note. Ignored — no property, no edge (§4.3), no hub (§7). |

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
one becomes a stub (§5.6), and one naming a place the vault does not own is the
same §9 error here as in the prose.

`parent:` is the one reserved key that follows this rule with a fixed edge
type: it emits the `folder_notes.edge` type in the `folder_notes.direction`,
so a note cross-listed under several parents contributes the same edges as the
folder layout would have.

## 5. Links

### 5.1 Syntax

| Written | Meaning |
|---|---|
| `[[Note]]` | Link to `Note`. |
| `[[Note\|display text]]` | Same link. The display text is stored as the edge's `label` property when `structure:` is declared (§7.1), and is dropped otherwise. |
| `[[Note\\\|display text]]` *(in a table cell)* | The same link. `\\\|` is how Obsidian writes a pipe inside a cell, so the escape belongs to the separator and never to the note's name — and a cell's `\\\|` is unescaped wherever it is read, in a property value as much as in a link. |
| `[[Note#Heading]]`, `[[Note#A#B]]`, `[[Note#^block-id]]` | Link to `Note`, `anchor` = the fragment. A nested heading is addressed by joining the levels with further `#`. With `structure:` the edge retargets to the section or chunk the fragment names (§5.4, §7.1). |
| `[[Label/Name]]` | Folder-qualified — use when a stem is ambiguous. |
| `[[Note]]{type}`, `[[Note\|display text]]{type}` | The same link, **typed**: the brace names the edge type and outranks every heading rule (§5.3 rung 0). It must follow the `]]` with no space, hold no whitespace and close on the same line. |
| `[text](path.md)`, `[text](path.md "EDGE_TYPE")` | Path link, resolved relative to the linking note; the title is an explicit edge type. |
| `[text](path.md#Heading)` | Same link, `anchor` = the fragment — a path link carries one exactly as a wikilink does, and it is never part of the target. |
| `[text](file.ext)` | A plain link to a non-`.md` file is an attachment reference (§6), with the link text as its `alt`. |
| `[text](#fragment)`, `[text](sub/dir/)`, `[text](mailto:…)` | An in-page anchor, a directory link and any other URI scheme name no node: silent no-ops. |
| `![[Note]]` | Embed → an `EMBEDS` edge. |
| `![[image.png]]`, `![alt](img/x.png)` | Attachment (§6), never a note link. |
| `[![alt](thumb.png)](full.png)` | A thumbnail linking to the full picture: **both halves count**. The inner image is a reference to `thumb.png` and the outer link is one to `full.png` — or an ordinary note link when it names a `.md` file. The outer reference wears the inner `alt`. |
| `https://…` | An external `Source` node keyed by the URL. |

**A fenced code block (``` or `~~~`), a `%%comment%%` (§5.7), a
`<!-- kglite … -->` directive (§5.8) and the inside of an inline
`` `code span` `` are the only regions that are not scanned.** A
fence ends at its own delimiter, so a `~~~` line written inside a ``` block is
code like everything else between them. A code span is rendered literally, in
every dialect, so `` `[[Note]]` `` states no link, `` `#tag` `` no tag and
`` `![x](y.png)` `` no picture — while the span is still ordinary inline text
to what is written *around* it, so ``[`file.md`](file.md)`` is one link whose
display text happens to be code.
Indented four-space code is **not** exempt, and neither is an HTML block
that is not a directive (§5.8):
honouring CommonMark's indented-code rule would also swallow every list
continuation line, which is where a converter writes most of its links. Fence
whatever must not be read — and write `\[\[` for a literal `[[`, which is the
escape Obsidian uses. A **heading line is scanned like any other line**:
`## Overview ![map](img/x.png)` states a picture and `## See also [[Alice]]`
states a link, and §5.4 says which section they carry.

The reader parses a body's **blocks** once and scans each of them — a heading
line, a paragraph, a list item, a table cell — as one region. So the text half
of a `[…](…)` link may be hard-wrapped across lines, `[Binary\nExtensions](url)`
being one link and not none, while a bracket left open at the end of a
paragraph can never swallow the next one. A `[[wikilink]]` is the exception
that stays on one line, as it is in Obsidian.

A **markdown-style** target — the `(…)` half of `[text](…)` and `![alt](…)` —
is percent-decoded before it is resolved, because that is the spelling a tool
writes `%20` in: `[report](sales%20report.md)` reaches `sales report.md`. A `%`
not followed by two hex digits is itself, so a filename containing one
survives. A **wikilink** target is a name, not a URL, and is never decoded:
`[[a%20b]]` names a note spelled that way.

A target naming a place the vault does not own is an error (§9), not a link: an
absolute filesystem path (`C:/…`, `\\server\share`, `~/…`, `file:…`) or one
climbing above the vault root with `../`. A leading `/` is **not** one of
these — §6.2 fixes it as vault-root-relative, which is what lets a vault be
copied somewhere else.

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

0. a `{type}` written straight after a wikilink's `]]` — `[[Customers]]{joins-with}`
1. an explicit link title that looks like a type — `[x](y.md "JOINS_WITH")`
2. an entry in `heading_edges:` matching the enclosing heading text in full,
   case-insensitively
3. the built-in heading ladder, matched case-insensitively as a substring of
   the enclosing heading: *citation* → `CITES`, *join* → `JOINS_WITH`,
   *reference* → `REFERENCES`, *related* → `RELATED`, *depend* → `DEPENDS_ON`
4. `LINKS_TO`

Rungs 0 and 1 are each one spelling's own — a wikilink carries no title and a
markdown link takes no brace — and both say what *this* link means, which is
why they outrank the heading the link happens to sit under.

`heading_edges:` wins over the built-in ladder — which is why a corpus writes
`heading_edges: {"Related topics": RELATED_TO}` instead of accepting `RELATED`.

**The `{type}` suffix.** It must follow the closing `]]` with nothing between
them, hold no whitespace, and close on the same line. Its text is normalised
the way a frontmatter key is (§4.3), so `{see-also}`, `{see_also}` and
`{SEE_ALSO}` all name `SEE_ALSO`, and the result must be a non-empty name that
does not start with a digit. A brace that fails any of that is left as prose
and the build **warns**, naming the note and what was written; the link keeps
the type rungs 2–4 give it. A brace the link does not touch — a space before
it, or no `}` before the line ends — is ordinary prose and says nothing about
the link, silently. Two more spellings that are deliberately *not* link types:
`[[Note]] #tag`, where `#tag` is a tag and stays one (§5.5), and
`![[Note]]{x}`, because an embed's type is `EMBEDS`. Everything else about the
link is unchanged — `[[Note#Heading|display text]]{see-also}` keeps its
`anchor`, its `label` and its retarget (§5.4).

The suffix is prose like the rest of the body, and the body is never rewritten
(§10.3): a section or chunk whose text spans a typed link keeps the `{type}` in
its `text` and `embed_text`, and an export writes the line back byte for byte.

### 5.4 Edge properties

Every body link carries `section`, the enclosing heading's text verbatim
(absent above the first heading). A link or attachment reference written *in* a
heading line is enclosed by that heading, so it carries the same string as the
links below it — including any markup the heading contains, because the text is
verbatim; one section is one value, or a section would split into two edge
groups. A fragment link also carries `anchor`, the fragment without its leading
`#` — a nested heading path keeps the `#`s *inside* it, so `[[Note#A#B]]`
carries `A#B`. A link written `[[Target|display text]]` carries `label`, that
text, where `structure:` is declared (§7.1); without it the text is dropped.

**The fragment never changes which note a link names**: `[[Note#Heading]]` and
`[[Note]]` name the same note, and so do the two spellings of a path link. Where
`structure:` derives sections or chunks from that note, the edge then
**retargets** to the derived node whose id is exactly `<note id>#<fragment>`, and
a fragment naming a single heading retargets to the **first** section in the note
titled that — the heading Obsidian itself jumps to (§7.1). The `anchor` property
is kept either way, so the fragment as written survives the retarget. A fragment
naming no heading and no block id in that note leaves the edge on the note and is
a warning (§9).

Two links from one note to one target are **two edges** when they differ in
`section`, `anchor` or `label`, and one when they do not — repeating a link
inside a section is one relationship, linking from two sections is two. Edges
emitted from frontmatter (§4.3) carry none of the three.

### 5.5 Tags

Both forms feed one `Tag` hub per distinct tag, joined by `TAGGED`. A hub node
holds its text in `id`, not in the `concept_id` a note uses — §7's table names
the id property of every kind of node. Tag identity is **case-insensitive**, as
it is in Obsidian: `#Seismic` and `#seismic` are one tag, held under the
lowercased id and titled with the casing the vault used most often. A vault that
wants the two kept apart redeclares the hub in `vault.yaml` with
`case_insensitive: false` (§7), which is the same mechanism any other hub uses.
This is one of the places the dialects genuinely differ: `okf` and `loose` keep
tags case-sensitive. The two forms are:

- `tags:` in frontmatter, which also stays a list property on the note
- inline `#tag` in the body

Inline extraction skips fenced code, inline code spans, a `#` inside a URL or a
wikilink anchor, and a `#` that begins a line and is followed by a space (that
is a heading — though a `#tag` written *in* the heading's text is still a tag).
A tag name runs over letters, digits, `_`, `-` and `/`, and must contain at
least one letter: `#2026` is not a tag.

**A tag on the node that holds it.** Where `structure:` derives nodes (§7.1),
every inline `#tag` is *also* written into a `tags` list property on the
**innermost derived node whose range contains it** — the chunk, callout, step
or table row it sits in, else the enclosing section, else nothing. The list is
in first-use order and spelled as the note wrote it; folding case is the hub's
rule for identity, not this property's. It is what makes a paragraph-scoped
marker selectable: `MATCH (c:Chunk) WHERE 'warning' IN c.tags`. `tags` is
therefore a property a derived node defines itself (§7.1), so no `inherit:` and
no directive may name it. The note's own `tags` property is unchanged — it
reports the frontmatter and nothing else — and so are the note's `TAGGED`
edges.

**Caveat: a chunk is as narrow as the author made it.** A chunk packs several
paragraphs, so a marker meant for one paragraph tags the whole chunk. Where
that matters, give the paragraph a `^block-id` (§5.7) or close the chunk with
`<!-- kglite chunk -->` (§5.8); both make it a chunk of its own.

**`tag_labels:` — a family of tags modelled as its own nodes.** A vault can
declare that tags under a prefix are not tags at all but a kind of thing:

```yaml
tag_labels:
  "intent/*": {label: Intent, edge: HAS_INTENT}
```

`#intent/create-grid` then mints an `Intent` node with the id `create-grid` —
the tag text **after** the prefix — and an edge `HAS_INTENT` to it from the
same innermost derived node the `tags` property landed on (from the note where
`structure:` derives none). The pattern is `<prefix>/*` and nothing else; any
other spelling is a config error naming the key. Both `label` and `edge` are
required, and `edge` is spelled `UPPER_SNAKE` like every other edge type.

A tag a rule matches is modelled **only** that way: it leaves the `Tag` hub
entirely — no `Tag` node, no `TAGGED` edge — whichever of the two forms above
wrote it, because both forms feed that one hub. What it does *not* leave is the
text: a frontmatter `tags:` list still reports every entry as written, and a
derived node's `tags` list still carries the tag, matched or not. Identity
folds case exactly as the hub's does (`#Intent/Create-Grid` and
`#intent/create-grid` are one `Intent`), and the node's title is the spelling
the vault used most often. Where two rules match, the **longest prefix wins**,
so `intent/grid/*` takes `#intent/grid/create` out of `intent/*`. A tag that is
only the prefix (`#intent/`) names nothing and stays an ordinary tag. A rule no
tag in the vault matched is a warning (§9), like any other declaration the
vault's content does not carry.

### 5.6 Unresolved targets

An unresolved link target becomes a `_provisional: true` stub node, labelled
`Concept` and keyed by the unresolved name, so "referenced but not written" is
one Cypher query and the stubs never mix with the notes' own labels. Stubs
are counted in the build report and are never exported as files (§10).

### 5.7 Comments, block ids and callouts

Three more constructs a body can carry. None is new syntax — all three are
Obsidian's own — and the reader honours them whether or not `structure:` is
declared. Turning callouts into *nodes* is what `structure:` adds (§7.1).

**Comments.** `%%…%%` hides text from a reader: inline, `a %%hidden%% word`, and
as a block, where the opening and closing `%%` sit on lines of their own. A
comment's text is **never scanned** — no link, no tag, no attachment reference
and no heading is read out of it — a property it shares with a fenced code
block and a directive (§5.1, §5.8). It is still part of the body property,
verbatim, and it still travels through an export.

**Block ids.** A trailing ` ^id` names a block, so `[[Note#^id]]` links to
exactly that block rather than to the note. The id may hold **Latin letters,
digits and dashes only**: `^my_id` is not a block id, it is text. The three
placements are Obsidian's:

- at the end of the last line of a paragraph, after a space;
- on a line of its own, blank line above and below, directly after a table,
  list, quotation or fenced block;
- directly on a bullet, naming that one item.

With `chunks:` declared a block id keys the chunk it names (§7.1). It is the one
derived id that survives editing around it, which makes it the answer both to a
chunk id that churns and to a duplicate heading that cannot be linked.

**Callouts.** A blockquote whose first line begins `> [!type]` is a callout:

```markdown
> [!warning]+ Check the survey datum
> Depth values are metres below MSL.
```

The type identifier is case-insensitive and **arbitrary**: Obsidian styles the
thirteen it knows and renders any other as a plain callout, so `[!versionadded]`
is legal and a corpus keeps its own vocabulary rather than being folded into
`note`. The kind is stored **lowercased**. A `+` or `-` directly after the
identifier folds the callout open or closed and is not part of the kind. Text
after the identifier is the title; with none, there is no title — Obsidian
displays the type, and this spec stores nothing. Callouts nest.

A callout's body is prose like any other: its links, tags and images are
scanned — a callout is not a comment — and its `section` is the heading above it.

### 5.8 Directives

```markdown
<!-- kglite chunk -->
<!-- kglite address: Data tree -> Wells | Task pane: Wells -> Annotations table -->
```

An HTML comment of the form `<!-- kglite <key>[: <value>] -->`, **on a line of
its own**, is a directive: an instruction to the reader written where it
applies, in syntax Obsidian already hides from a rendered note. Whitespace
inside the comment is free. `kglite` must be a word of its own, so
`<!-- kglitex … -->` is an ordinary comment, and so is every HTML comment that
does not open with the literal. The key is spelled like a frontmatter key — a
letter or `_`, then letters, digits, `_`, `-` or `.` — and everything after the
first `:` is the value, trimmed. A comment that only starts like a directive
(`<!-- kglite owner: docs --> and more`) is prose, because the whole block must be
the comment.

**Only a block counts.** A `<!-- kglite … -->` written *inside* a paragraph is
inline HTML and stays prose, so a note may document the syntax without
invoking it. `<!-- kglite -->`, which names no key, is recognised as the shape
it is and warned about (§9); it carries no meaning.

**A directive is not prose.** Its own bytes are never scanned — no link, no
tag and no attachment is read out of one, exactly as for a `%%comment%%`
(§5.1) — and where `structure:` derives nodes (§7.1) the directive's line is
**cut out of every derived `text`**: a section's, a chunk's, and the
`embed_text` rendered from either. The cut is the directive's own range and
nothing more, so the blank lines around it stay and every other byte is the
author's own. The note's `body` property keeps the directive verbatim, which
is what lets an export write the file back byte for byte (§10).

A directive is recognised — and skipped — in every dialect; what a key
*means* is defined here.

| Directive | Meaning |
|---|---|
| `<!-- kglite chunk -->` | Close the open chunk at this point (§7.1 `chunks:`). |
| `<!-- kglite heading -->` | Promote the first line of the paragraph below to a heading (below). |
| `<!-- kglite <key>: <value> -->` | State `<key>` on the node this directive sits in: a **typed edge** when the value names wikilinks, a **property** otherwise. |
| `<!-- kglite <key> -->` | Nothing — a key with no value states nothing, and the build warns (§9). |

**Which node it states it on.** The **enclosing section**, when `structure:`
declares `sections:` and the directive sits under a heading; the **note**
otherwise — above the first heading, and in a vault that derives no sections.
A directive therefore reaches exactly the node a reader would point at: the
one whose prose it was written beside.

**Value or edge, by §4.3's rule.** A value that names a wikilink — one
`[[Target]]`, or a list in which every element is one — becomes edges of type
`UPPER_SNAKE(key)` from that node to those notes, and no property. Targets
resolve as body links do (§5.2) and an unresolved one becomes a stub (§5.6),
exactly as a frontmatter key's would. Anything else becomes a property named
`key`, typed as §4.2 types a frontmatter value and overridden by `types:` for
that node's label.

A directive is written *inline in prose*, and the value grammar bends three
ways for it:

- **A bare `[[Target]]` needs no quotes**, and nor does `[[A]], [[B]]`. In
  frontmatter the quotes are what stop YAML reading `[[X]]` as a nested flow
  sequence; in a comment beside a sentence there is nothing else `[[X]]` could
  mean. `[[A, B]]` is still one target — the whole value is tried as a single
  wikilink before the commas are.
- **A value YAML would read as a mapping is the raw text.** `:` is ordinary
  punctuation in a sentence, so `address: Data tree -> Wells | Task pane:
  Wells` states that string. A property could not hold a mapping in any case.
- **A value YAML refuses is the raw text too**, for the same reason.

**Keys a directive may not name.** Anything a note or a derived node defines
itself: the §4.1 reserved keys, the derived properties `inherit:` may not name
either (§7.1), the note's `body:` property, and `concept_id` / `file_path`.
Each is an **error** naming the key (§9). A key the vault declares under
`hubs:` (§7) is nothing special here — it is a property, unless its value is a
wikilink and the typed-edge rule takes it, which is the same precedence §4.3
already sets for frontmatter.

**`<!-- kglite heading -->` — a heading the source did not write.** On the
line above a paragraph, the marker promotes that paragraph's **first line** to
a heading at the enclosing heading's level + 1 (level 1 where there is none,
and never deeper than 6, which is as deep as markdown goes). A surrounding
pair of `**bold**` markers comes off the heading's text and nothing else does:
a code span, a link or a `**word**` in the middle of the line is the heading's
own text. The paragraph's remaining lines, if any, are the first paragraph
under the new heading.

```markdown
### rmsapi.Project

<!-- kglite heading -->
**open(filename, readonly=False)**

Opens a project.
```

From there it is a heading like any other: it derives a `Section` (§7.1), it
is addressed as `[[Note#rmsapi.Project#open(filename, readonly=False)]]`,
`key_from_heading:` reads its title, a table under it attaches to it, and the
`#fragment` ladder of §5.4 resolves onto it. The **body is untouched** — only
the tree a reader builds from it changes — so the file still renders in
Obsidian exactly as it did and an export writes it back byte for byte.

Two markers under one heading are **siblings**: the level is read from the
heading the author wrote, not from the last synthetic one, which is what makes
a converted API page a flat list of methods under its class. A marker with no
paragraph below it — the end of a section, or a list, table, fence or second
directive next — promotes nothing and is a warning (§9).

**One key, one value per node.** A second directive naming a key the same node
already carries — from an earlier directive or from the note's own frontmatter
— replaces it, and the build warns (§9). Edges do not follow that rule: two
wikilink directives sharing a key state two edges, as a frontmatter list does.

## 6. Attachments

1. **Accepted syntax:** `![alt](rel/path.png)`, `![[image.png]]`,
   `![[image.png|alt]]` — and `[text](rel/path.pdf)`, a **plain** link naming a
   non-`.md` file, whose link text is its `alt`. An image written *inside* a
   link's text, `[![alt](thumb.png)](full.png)`, is two references: the
   thumbnail and whatever the link points at (§5.1). The leading `!` says how a
   renderer displays the file, not whether the vault holds it, so both
   spellings are the same reference here and make the same node and the same
   edge; a vault of 48 `[download](tool.zip)` links would otherwise produce
   nothing at all. The body keeps the original syntax verbatim. A target
   carrying a URI scheme — `http(s)`, `mailto:`, anything else — is somebody
   else's file: it is not in the vault, no `stat` describes it, and it becomes
   no node. So do an in-page `[text](#fragment)` anchor and a directory link,
   which name nothing to resolve. The `![alt](…)` and `[text](…)` spellings are
   percent-decoded before resolution and the `![[…]]` one is not, exactly as
   §5.1 reads the two syntaxes; a reference naming an absolute or escaping path
   is a §9 error. **A converter emits the encoding** that decoding undoes: a
   markdown target is read up to the first whitespace or `)`, so a space makes
   the whole reference invisible — nothing matches, and there is no node, no
   edge and no warning — and a `)` truncates it to a path that resolves to
   nothing. Emit `%20`, `%28` and `%29` for those, and `%25` for a literal `%`,
   which is otherwise eaten whenever the two characters after it are hex
   digits. Encoding cannot rescue `#` or `?`: the target is decoded *before* it
   is split, so `img/c%23d.png` is cut at the `#` exactly as `img/c#d.png` is,
   and the `![[…]]` spelling splits on `#` too — a file whose name holds one is
   unreachable from either syntax, so rename it. Wikilink targets are names,
   not URLs, and are never encoded.
2. **Resolution ladder:** note-relative → vault-root-relative → a unique
   filename anywhere in the vault. The stored value is always the
   **vault-relative** resolved path, so every consumer resolves from one root.
   A filename occurring twice does not resolve on the third rung; qualify it.
   A target written with a leading `/` is vault-root-relative and skips the
   first rung, exactly as a path *link* reads one.
3. **Nodes:** one node per distinct resolved file, labelled `Image` when the
   extension maps to an image MIME type the MCP server delivers
   (`image/png`, `image/jpeg`, `image/gif`, `image/webp`) and `Attachment`
   otherwise — so SVG and TIFF are `Attachment`s. The node's id field is named
   `path` and holds the vault-relative path, so `n.path` *is* the id; `mime`
   comes from the extension table (`application/octet-stream` for an extension
   it does not name), `size_bytes` and `mtime` (a UTC datetime) from `stat`,
   and `title` is the filename. An `Image` also carries `text`: the distinct
   alt texts and the titles of the notes using it, newline-separated in
   first-use order, so captions stay text-searchable.
4. **Edges:** `HAS_IMAGE` or `HAS_ATTACHMENT` from the note, with edge
   properties `alt` (the alt text of an `![…]` reference or the link text of a
   plain one, when there is one), `section` (enclosing heading) and `ordinal`. Two references to one file from one note are **two
   edges** when they differ in `section` or `alt` and one when they do not —
   §5.4's rule, applied to attachments. `ordinal` numbers the edges a note
   emits of each kind, 0-based in body order, so a folded repeat consumes no
   number.
5. **Bytes are never read at build time.** Metadata comes from `stat` only: no
   content hash, no dimensions, so build cost is independent of image volume.
   The type therefore comes from the extension, never from the content.
6. **A missing target** becomes a `_provisional: true` node with
   `missing: true`, labelled from its extension like any other, keyed by the
   reference as written (normalised), and counted in the build report. An
   ambiguous bare filename is one of these: it resolved to nothing, and the
   warning names the candidates.

**Recommendation for converters: emit PNG, JPEG, GIF or WebP** — the four types
the bundled MCP server delivers as images. SVG is stored as an `Attachment` and
is not delivered; rasterise it when you build the source.

## 7. `.kglite/vault.yaml`

Optional. Read by explicit path (the walk ignores dot-directories), applied to
every build and re-applied to every rebuild — with carried embeddings, it is
the only state that survives one. An unknown top-level key, an unknown key
inside `structure:` (§7.1), an unknown `kglite_vault` version, or a value of
the wrong shape is an error, and one of
those **fails the build** rather than leaving a finding on a graph that looks
built: because the file is the only thing a rebuild re-applies, a vault whose
`vault.yaml` stopped parsing would silently lose its labels, hubs, indexes and
embed targets. `okf.validate` reports the same failure as the §9 error.

The file is a vault construct: it is read under the `obsidian` dialect only,
and an `okf` or `loose` build that finds one ignores it **with a warning**.

The declarations win over whatever the caller configured, in both directions —
a rebuild re-reads the file, so the file is the vault's statement about itself.

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

A hub reads a key's **list** entries; a scalar joins no hub. A key that is
both a hub and wikilink-valued goes to the typed-edge rule instead (§4.3) —
that rule wins, and the clash is a warning (§9) rather than a silent empty hub.
Declared hubs are **merged over** the built-in `tags` one rather than replacing
the set, and a redeclaration names only what it changes: an omitted `label` is
`Tag`, an omitted `edge` is `TAGGED`, and an omitted `case_insensitive` is what
the built-in already said — `true` for `tags`, `false` for every other key.

`types:` decides how a note's property **column is built**, so a declaration
overrides inference rather than converting a value afterwards. It names the
label a note *ends up with* and a property that label carries; `concept_id` is
never retyped (it is the node's identity and the index built on it). A value
that will not coerce is left exactly as it was written and **warned** about,
naming the note, the property, the declared type and the value — the
declaration is a statement about the vault, and a note that disagrees with it
still holds what a human typed. A declaration no note matches is a warning too.

`indexes:`, `text_indexes:` and `embed:` name a label or property the vault may
not carry yet; each is a **warning**, never an error, and the rest are still
installed.

**Which property holds the id.** Every declaration above names a property, and
the id property is not the same word for every kind of node. Declaring
`indexes: {Topic: [concept_id]}` for a hub installs an index over a property no
hub node carries and warns "indexed no value" — that warning is the only thing
that says so, hence this table:

| Node | Id property |
|---|---|
| A note, whatever its label | `concept_id` |
| A `Concept` stub for an unresolved link (§5.6) | `concept_id` |
| A hub node — `Tag` and every `hubs:` entry (§5.5) | `id` |
| A `Source` node for an external URL (§5.1) | `id` |
| A `Folder` node (§2.2) | `id`, holding the vault-relative directory path |
| An `Image` or `Attachment`, present or missing (§6.3) | `path` |
| A node `structure:` derived from a note's body (§7.1) | `concept_id` |

A note's id is `concept_id` because it is a **name** every link in the vault
resolves to (§3), and it is the one property `types:` never retypes. The
synthesized nodes are keyed by what they already are — a tag's own text, a URL,
a directory, a file path — and say so.

A complete example — a vendor help corpus of ~7k articles:

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

By default a note is one node and its prose is one property (§4.2). A
`structure:` block derives nodes from the note's own markdown as well: the
headings, the paragraphs under them, the callouts, the fenced examples, the
numbered steps, the table rows. It adds no file to the vault and no syntax to a
note — Obsidian already names sub-note things, and every id this block mints is
one of those names, so a derived node is linkable from anywhere in the vault as
`[[Note#Heading]]` or `[[Note#^block-id]]` and navigable in Obsidian itself.

There is no default and no heuristic: a vault with no `structure:` block builds
the graph it built before, note for note and edge for edge. An unknown key
*inside* `structure:` is an error exactly as an unknown top-level key is (§9) —
the block is a hard compatibility boundary, and the rest of this section says
what a kglite that knows it does with each key.

**How a derived node is keyed.** Every one of them keys on `concept_id`, like a
note, and **never carries `file_path`** — it is not a file, and no export writes
one (§10.1).

| Construct | Id |
|---|---|
| A section | `Note#A#B` — the note's id, then its heading path joined by `#` |
| A chunk, callout, example, procedure, step or table row | its parent's id, then `~<kind><n>`: `Note#A#B~chunk3`, `~note1`, `~example2`, `~list1`, `~list1~step4`, `~row7` |
| Anything named by a block id (§5.7) | `Note#^block-id` |
| A duplicate of either | the same id with `~2`, `~3`… appended |

`<kind>` is the construct's own word and `<n>` counts that kind under that
parent from 1, so the counter always starts with a letter and the duplicate
suffix never starts with one. That is deliberate: a bare `~2` can only ever mean
"the second thing that wanted this id", and can never be read as the second
chunk of a section.

Only the section ids and the block-id ones are **stable** across edits. A
`~chunk<n>` id is scoped to its section and renumbers when a paragraph is
inserted above it; every id under a heading changes when that heading is
renamed, because the heading text is in the id. On a 22 226-chunk corpus a
one-paragraph insert moved 3.6% of the ids and a heading rename moved 58% of
one page's. Where an id has to survive, write a block id (§5.7); where it does
not, `chunk_hash` below is what carries the embedding across.

#### `sections:`

```yaml
structure:
  sections: {label: Section, edge: HAS_SECTION, parent: PARENT_SECTION, next: NEXT_SECTION}
```

One node per heading in the body, in document order; a `#` inside a fenced code
block is not one, because that region is not scanned at all (§5.1).

- **Title** is the heading's text **verbatim**, including any inline markup it
  carries: `## See also [[Alice]]` titles a section `See also [[Alice]]`, which
  is the same string §5.4 already stores in `section`, and one heading must not
  have two spellings. Markdown's optional closing run of `#`s (`## Overview ##`)
  is a closer and not part of the text; surrounding whitespace is trimmed.
- **Id** is `Note#A#B`: the note's id, then the titles of the headings enclosing
  this one and its own, joined by `#`. That is Obsidian's own nested-heading
  link spelling, so `[[Note#A#B]]` reaches exactly this node.
- **Properties**: `title`, `level` (1–6), `ordinal` (0-based among its
  siblings), `path` (the list of titles the id joins), `text` (the body verbatim
  from the line after the heading to the next heading of the same or higher
  level, trailing blank lines trimmed and any `<!-- kglite … -->` directive
  cut out, §5.8), `note_id`, plus every `inherit:`
  property — **and** whatever a `<!-- kglite <key>: <value> -->` written under
  this heading states, as a property or as a typed edge leaving this section
  (§5.8). The note's own `body` property is untouched and still holds the
  whole body — deriving sections moves nothing out of the prose.
- **Edges**: `edge` joins the note to each of its top-level sections and a
  section to each section directly inside it, so every section has exactly one
  incoming `HAS_SECTION` and the chain from the note spells its path. `parent`
  states the same nesting child→parent and is emitted only for a section that
  has one. `next` joins consecutive siblings under one parent in document order.
- **Duplicate headings.** Obsidian resolves a heading link to the **first**
  heading of that text and has no syntax for a later one. So does this spec:
  `[[Note#A]]` reaches the first, the second gets a `~2` id and a warning (§9),
  and the fix the warning steers to is a block id on the second — the only
  spelling Obsidian itself can link.

#### `chunks:`

```yaml
  chunks: {label: Chunk, edge: HAS_CHUNK, next: NEXT_CHUNK, max_words: 650, max_chars: 6000}
```

The retrieval unit. Leaf blocks — paragraphs, list blocks, fences, tables,
quotations — are packed greedily in document order, and the open chunk closes
before a block that would take it past `max_words` **or** `max_chars`. A section
boundary always closes the open chunk, so a chunk never spans two sections.

**A block bigger than either limit on its own is split inside itself**, at the
boundaries its own kind offers: a list between its **top-level items** (a
nested list travels with the item that introduced it), anything else at line
ends — which for a table is its rows. The pieces are packed greedily to the
same caps, in document order, and chain with `next` like any consecutive
chunks. A header row is therefore in the first piece of a split table and is
**not** repeated in the others: a chunk is a range of the source, not a
rendering of it. A single line that busts `max_chars` by itself is cut at a
`char` boundary — the only split left, and the only one that can land inside a
word. The build reports how many boundaries the caps forced this way as
`forced_splits` (§9); a non-zero count says the source has passages the caps
had to cut blind, and a blank line where the author wants the break is the
fix.

- `edge` joins the enclosing Section, or the note when `sections:` is not
  declared; `next` joins consecutive chunks within one section.
- `<!-- kglite chunk -->` on a line of its own (§5.8) **closes the open
  chunk** at that point and is itself no chunk at all: the author's break,
  where a blank line would have joined the two passages anyway. Only a
  top-level marker counts — inside a list item, a quotation or a table there
  is no chunk of its own to close, so the marker does nothing and the build
  warns (§9). It is **not** counted in `forced_splits`: that number is the
  boundaries the caps had to place blind, and an authored one is a choice.
- A paragraph whose last line ends in a block id **closes the open chunk and is
  a chunk of its own**, keyed `Note#^id`. That makes a block id the author's one
  lever over where chunks divide, and the way to give a passage a citable id
  that editing around it cannot move. Where such a block is itself over a limit
  and splits, the id keys its **first** piece — the one `[[Note#^id]]` was
  pointing at while the block still fitted.
- **Properties**: `text` (the packed blocks verbatim, spaced as the source
  spaced them, less any `<!-- kglite … -->` directive they contain — a
  directive is metadata and never chunk text, §5.8), `ordinal` (0-based within
  the section), `chunk_hash` (the SHA-256 of `text`, lowercase hex),
  `note_id`, `section_id`, plus `inherit:`.
- **Embeddings survive a rewrite.** A rebuild carries vectors by `(label, id)`
  (§12) and then, for anything this block derived, by `(label, chunk_hash)`
  where exactly one old node of that label carried the hash — so a chunk that
  only moved is recognised as the same chunk instead of being minted afresh. The
  fallback decides *which old node this is*, not whether to re-embed: the
  changed-mode pass still compares the embedded property's own stored hash, so a
  chunk whose `embed_text` changed because its heading changed is re-embedded,
  and one whose text merely shifted is not.

#### `callouts:`

```yaml
  callouts: {label: Note, edge: HAS_NOTE}
```

One node per callout (§5.7), attached by `edge` to the enclosing Section — or to
the note where there is no section, and to the enclosing callout where callouts
nest. Properties: `kind` (the type identifier, lowercased, **whatever word it
is** — `versionadded` and `caution` are as valid as `note`), `title` (absent
when the callout has none), `fold` (`+` or `-`, the identifier that folds the
callout open or closed; absent when it is not foldable), `text` (the callout
body verbatim with the `>` markers stripped), `ordinal`, `note_id`,
`section_id`, plus `inherit:`.

A nested callout's `text` loses **its own** depth of markers and no more: a
callout written `> > text` two deep reads `text` on its own node, while its
parent's `text` keeps the `> [!tip]` line that says a callout is nested inside
it. `section_id` names the heading either way — a nested callout is inside
another callout *and* inside the same section.

#### `code_fences:`

```yaml
  code_fences: {label: Example, edge: HAS_EXAMPLE, langs: [python]}
```

One node per fenced block whose info string's first word is in `langs`. **Omit
`langs` and every fence qualifies**, including one carrying no info string at
all — which is the setting a corpus needs when its converter dropped the
languages on the way in. The comparison is case-insensitive at both ends.
Properties: `lang` (the first word of the info string, lowercased; absent when
there is none), `code` (the fence contents verbatim, without the fence lines
and without the info string, and dedented by the indentation of the container
it sits in, so a fence inside a list item keeps only the code's own
indentation), `caption` (the paragraph immediately above the fence in the same
container, when that paragraph's text ends with `:`), `ordinal`,
`note_id`, `section_id`, plus `inherit:`. A caption paragraph stays in its
chunk's text as well: nothing is taken out of the prose.

#### `ordered_lists:`

```yaml
  ordered_lists: {label: ProcedureStep, container: Procedure, edge: HAS_STEP,
                  next: NEXT_STEP, under_heading: "^(Procedure|Steps|To .*)", min_items: 2}
```

Every **top-level** ordered list — one not nested inside another list item —
holding at least `min_items` items (default 2) becomes a procedure.
`under_heading` is optional and is the opt-in narrowing: a regular expression
matched against the enclosing section's title, reading only the lists under a
heading that matches. Without it every qualifying list is read, which is the
rule the corpus this profile was measured against was built with. A list above
the body's first heading sits under no heading at all, so a declared
`under_heading` cannot reach one. An *unordered* list is never a procedure,
whatever heading it sits under.

- The container is a **new node**, not the enclosing section relabelled, so a
  section holding two lists yields two procedures — `Note#A#B~list1` and
  `~list2`. It carries `title` (the enclosing section's title, or the note's),
  `ordinal`, `step_count` (the steps the container itself holds — a sub-step
  counts on its own step), `note_id`, `section_id`, plus `inherit:`, and joins
  its section by `HAS_<UPPER_SNAKE(container)>`, here `HAS_PROCEDURE`.
- One node per item: `text` (the item's own content, excluding any list nested
  inside it), `ordinal` (0-based), `level`, plus `inherit:`. `edge` joins the
  container to each top-level step and a step to the steps of an ordered list
  nested inside it; `next` joins consecutive steps at one level.

#### `tables:`

```yaml
  tables:
    - {under_heading: '^Parameters$', label: ApiParameter, key_column: name, edge: HAS_PARAMETER}
    - {under_heading: '^Worked at$', edge: WORKED_AT, edges: true}
```

A **list** of rules, each naming the heading its tables sit under as a regular
expression — matched the way `ordered_lists.under_heading` is, so a rule that
means the whole heading anchors it (`^Parameters$`) and one that means either
case writes `(?i)`. The first rule that matches the enclosing section's title
reads the table, and a table under no matching heading is prose like any other.
A table above the body's first heading sits under no section and no rule
reaches it. Only GFM pipe tables are read — **raw HTML is never structure**
(§1.4), so a converter emits GFM.

- **Node form** (the default): one node per body row, labelled `label`. Each
  column becomes a property named by its header text **as written**, typed by
  `types:` under that label; a cell is text unless a declaration says
  otherwise, because a cell is a string and not a YAML scalar. A **blank**
  header cell names no property and its cells are dropped: GFM has no
  headerless table, so a converter that had none wrote an empty header row, and
  inventing a positional name would key a corpus to a column order that moves.
  The key column is `key_column:` when declared and the first column otherwise;
  its value keys the row — `<section id>~<value>` — and is stored under its own
  column name as well. A key that is empty, or that a row above already used,
  keys on its position instead (`~row<n>`, counting from 1) and is a warning
  (§9); so is a `key_column:` the table does not carry, which falls back to the
  first column. `edge` joins the enclosing section, or the note, to each row
  node; omitted, it is `HAS_<UPPER_SNAKE(label)>`.
- **Edge form** (`edges: true`): the row states **an edge, not a node**, and
  `label:` is refused. Its target is the first column holding a `[[wikilink]]`,
  or the column `key_column:` names; every other column becomes a string
  **edge property** on an edge of type `edge:` from the note to that target, and
  an empty cell writes no property. The edge also carries `section` (the
  enclosing heading), `anchor` where the target's own wikilink has a fragment,
  `label` where it has display text, and `row`, the 1-based row number — a
  column named like one of those is dropped with a warning, because the link
  itself states it. A table with no target column states nothing, and warns.
  This is how a vault states per-edge attributes — a role, a weight, a date
  range — and §10.6 writes them back out.
- A cell's `[[links]]` and `![images]` are scanned as prose wherever they sit
  (§5.1), so a picture inside a table cell is the note's attachment reference as
  usual, an edge table's target is *also* the note's `LINKS_TO` edge, and a row
  rule never swallows either. A link column that resolves to nothing becomes a
  `_provisional` stub and a warning, as any link does (§5.6, §9). A cell's `\|`
  is the pipe the author meant, in a property value and in a wikilink alike.

#### `key_from_heading:`

```yaml
  key_from_heading: {label: ApiSymbol, when_matches: '^[\w.]+\.[\w]+(\(.*\))?(\s*→.*)?$',
                     property: qualified_name, under_label: Api}
```

Relabels a Section whose title is really a symbol name. Two gates, both
required, because the shape is cheap to match by accident: `under_label:`
restricts the rule to notes carrying that label, and the heading must contain a
`.` or a `(` whatever `when_matches` says. On one corpus the regex alone
matched 1 439 headings of which 13 were symbols — a heading like `Overview` is
a valid qualified name to a regex and nothing else. The default `when_matches`
is the one above: a dotted name, optionally with a call's parentheses and the
`→ type` return annotation a converter writes into the same heading.

The heading is **split** where that annotation or the call begins: everything
before the first `(` or `→` is stored under `property` — the name a query looks
up, `rmsapi.Project.open` — and the rest under `signature`, `(path) → Project`.
Storing the whole title under `property` would only repeat `title`. Relabelling
changes the label and adds the two properties; the section's own properties,
its id and its section edges are unchanged, and `[[Note#Heading]]` still
reaches it — it is the same node under another name.

A **synthetic** heading (§5.8) is a heading here too: a converter that emitted
`**open(filename) → Project**` as a bold line rather than as `####` gets the
same relabelling once the author writes `<!-- kglite heading -->` above it,
without the file changing.

#### `inherit:` and `embed_text:`

```yaml
  inherit: [corpus, category]
  embed_text: "{title} | {heading_path}\n\n{text}"
```

`inherit:` names frontmatter properties of the note that are copied verbatim
onto **every** node derived from it, so a chunk-level filter or BM25 query needs
no hop back to the note. A key the note does not carry is simply absent there.
It may not name a property a derived node defines itself — `title`, `text`,
`tags`, `level`, `ordinal`, `path`, `note_id`, `section_id`, `kind`, `lang`,
`code`, `caption`, `chunk_hash`, `step_count`, `signature` — nor a reserved
frontmatter key (§4.1); either is an error (§9), because the alternative is a note silently overwriting
the structure it was read from.

**`tags`.** Every derived node carries the inline `#tag`s written inside its
own range, as a list in first-use order (§5.5). It is absent from a node that
holds none, and it is the node's own — a note's frontmatter `tags:` is not
copied down, which is why `inherit:` may not name it.

`embed_text:` materialises a property of that name on every derived node that
carries `text`. The placeholders are `{title}` (the **note's** title),
`{section_title}` (the derived node's own), `{heading_path}` (its path joined by
` > `), `{text}` and `{id}`; any other placeholder is an error. A chunk that
carries its document and its heading inside its own text is what makes a
retrieval hit legible without a second query, and declaring the template is what
lets `embed: {Chunk: embed_text}` and `text_indexes: {Chunk: [embed_text]}` name
it.

`indexes:`, `text_indexes:` and `embed:` name a derived label exactly as they
name a note's. The labels do not exist until a rule declares them, so naming one
without its rule is the §7 warning for a declaration the vault does not carry.

**Links become labelled.** Declaring `structure:` also turns on the `label` edge
property, so `[[Target|the display text]]` stores that text (§5.1, §5.4). There
is no separate switch: a vault that models the inside of its notes is a vault
that wants its links described, and the property is simply absent everywhere
else.

**Compatibility.** `structure:`, `edge_defaults:` and `export:` are unknown
top-level keys to any kglite released before them, and an unknown key **fails
the build** (§7). A vault using them therefore needs a kglite that knows them.
`kglite_vault` stays `1`: the keys are additive, and the failure on an older
build is loud and names the key rather than quietly producing a smaller graph.

### 7.2 `edge_defaults:`

```yaml
edge_defaults:
  NEXT_STEP: {derivation: source_order}
  CHILD_OF: {derivation: directory_index_hierarchy}
```

Edge properties that are constant for a whole type, pushed onto every edge of it
the build emits — from prose, from frontmatter or from a `structure:` rule
alike. It is how a vault states provenance that is true per type without writing
it on every line, and it is declared rather than authored: nothing in a note
changes. A default never overwrites a property the edge already carries
(`section`, `anchor`, `alt`, `ordinal`, `label`, an edge table's own columns),
and that clash is a warning (§9); so is a type the vault has no edges of.

### 7.3 `export:`

```yaml
export:
  edge_tables:
    WORKED_AT: "Worked at"
```

Read by the exporter only (§10.6). Each entry names an edge type and the heading
whose table its edges are written under in the source note, so an edge carrying
properties survives an export instead of being counted as a loss. A type with no
entry keeps today's behaviour exactly: its targets go to a frontmatter list and
its properties are dropped and counted (§10.9).

The key is an edge type (`UPPER_SNAKE`) and the heading is not blank; anything
else is a config error, like any other. The export finds this file through the
graph's `source_root` provenance (§12), or through the root the caller names,
and a caller may add or override an entry — that is how a graph that never was
a vault declares one. Declaring the table does not read it back: pair it with
the `structure.tables … edges: true` rule (§7.1) whose `under_heading:` matches
that heading, or the export warns that the table it wrote is prose (§10.10).

## 8. Skills and recipes carried in the vault

A vault can carry its own agent guidance, so a server built from it explains
how to query itself. Both directories are re-read on every build, so editing a
file is the whole update procedure.

A file in either directory that fails validation is **skipped with a warning
naming the file and the rule, and its siblings load** — the directory is
hand-authored vault content, and one unfinished skill must not cost an agent
the other nine. The build is not failed: nothing that reached the graph is
wrong, there is simply less of it than the author intended.

- `.kglite/skills/*.md` become `KgliteSkill` nodes. The frontmatter dialect is
  exactly the one an MCP skills directory uses — `name`, `description`,
  `references_tools`, `delivery`, then the markdown body. See
  [Authoring MCP skills](https://kglite.readthedocs.io/en/latest/python/guides/mcp-skills.html).
- `.kglite/recipes/*.md` become `KgliteRecipe` nodes — one stored Cypher query
  per file. Frontmatter carries `recipe` (the group id), `name`, `description`,
  optional `recipe_description` (what the group is for), optional `parameters`
  (the JSON Schema for the query's `$parameters`, as a nested map) and optional
  `tool` (an MCP tool name to serve the query under directly). The body is the
  statement, in a fenced ` ```cypher ` block:

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

  The statement must parse and must be read-only, and it must not spell the
  literal `LIMIT 200`. That number is the **recipe result payload cap**: an
  agent host refuses a recipe result of more than 200 rows outright, naming the
  count, so a caller always learns that an answer was incomplete. A stored
  `LIMIT 200` would cap the result at exactly the threshold, that refusal could
  never fire, and a truncated answer would read as a whole one.

  The rule is **equality with the cap, not a ceiling**, and it is checked
  against literals only. `LIMIT 50` and `LIMIT 201` are both accepted — 201 is
  simply a query that will be refused on the day it really returns 201 rows —
  and `LIMIT $rows` is never refused, whatever the caller passes. A query that
  wants everything under the cap writes `LIMIT 199`.

  **`parameters` is checked against the statement, exactly.** Its `properties`
  must name the same set as the `$parameters` the Cypher uses — one undeclared
  and one unused are both errors, and the message names each — and `required`
  must list *every* one of those properties: a recipe parameter is never
  optional. The root carries `type: object`, `properties`, `required` and
  `additionalProperties: false`; all four are required, `type` must be exactly
  `object`, `additionalProperties` must be explicitly `false`, and the only
  other root key allowed is `description`. A file that declares no
  `parameters:` stores the closed empty schema
  (`{type: object, properties: {}, required: [], additionalProperties: false}`),
  which is valid only for a statement that uses no `$parameters` at all.

  A file that fails validation is skipped, as above. A `parameters:` map is
  read **nested**, not flattened into dotted keys, so a schema's
  `properties.id.type` keeps its three levels. A file that omits
  `recipe_description` inherits the group's from **any** sibling that declares
  it: the whole directory is read before any file is stored, so which file
  carries the declaration is free, and the group needs exactly one. A group no
  file describes is every member's own failure — each is skipped with its own
  warning, because there is nothing to inherit.

  **`tool:` serves the query as a named MCP tool.** A server reading this
  vault registers a tool of that name whose description is the query's and
  whose input schema is its `parameters`, so an agent calls it in one step
  instead of naming the query inside `run_recipe_query`'s arguments. The name
  matches `^[A-Za-z_][A-Za-z0-9_-]{0,63}$`; two queries cannot claim one name,
  and a name the server has already registered for anything else refuses the
  boot naming the owner. The operator can switch the whole mechanism off with
  `extensions.recipe_tools: false`.

  **Expose a curated few.** Every named tool costs its description and its
  schema in every `tools/list`, which every session pays for whether or not it
  calls the query; the queries that carry the vault's routine questions earn
  that, a long tail does not. The catalogue block in `run_recipe_query`'s
  description marks the ones that have a tool, so nothing is hidden by leaving
  `tool:` off. Registration is **boot-time**: adding or removing a `tool:`
  needs the server restarted, not a rebuild.

## 9. Build report and validation

Every build produces a structured report. `okf.validate(path)` returns it
without keeping the graph; `kglite okf check <dir>` prints it and sets the exit
code. Both run the *same* read a build runs — the check is the build with the
graph thrown away, never a second opinion about it — and both read a directory
as a **vault** when no dialect is named: `dialect="obsidian"` is what
`okf.validate` and `kglite okf check` default to, where `okf.build` defaults to
`okf` for the bundle callers it has always served. A bundle is therefore
checked with an explicit `dialect="okf"`, which is what its build passes too.

It carries: files scanned and how many became notes, nodes per label, edges per
type, folder notes, dangling links, missing and ambiguous attachments, the
index / text-index / skill / recipe counts `.kglite/` produced, `forced_splits`
— chunk boundaries the `chunks:` caps had to place inside a block (§7.1) — the
`embed:` targets declared in `vault.yaml`, and two classified lists of
findings. The classification is the contract.

**Errors** — a vault with any of these does not meet this spec:

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

**Warnings** — legitimate in a real vault, worth seeing:

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

`kglite okf check` exits non-zero when any error is present. `--strict`
promotes every warning to an error — the setting a converter's own test suite
should use. `--json` prints the same report as a JSON object for a harness that
wants the lists rather than the text.

## 10. Export

Export writes a vault from a graph: `okf.export(graph, dir)` in Python,
`kglite okf export` from the CLI. It targets this format only.

1. **Which nodes.** A graph carrying `file_path` on any node was built from a
   vault, and in it a node **with** `file_path` is a note and becomes a file
   while a node without one was synthesized by the build — a `Folder`, a hub
   node, an attachment — and is not. A graph carrying `file_path` nowhere was
   not built from a vault, and every node in it becomes a file. Either way the
   synthesized labels are never files: `Tag`, `Source`, `Folder`, `Image` and
   `Attachment` regenerate on the next import, `_provisional` stubs are
   references rather than notes, and `KgliteSkill` / `KgliteRecipe` nodes are
   written to `.kglite/skills/` and `.kglite/recipes/` instead (§8). So are the nodes `structure:` derives
   (§7.1): a section, chunk, callout, example, procedure, step or table row is
   part of a note's prose, carries no `file_path`, and the next build derives it
   again from the body the note's own file already holds.
2. **File path.** The node's `file_path` is preserved when it has one and its
   top-level folder still matches its label; otherwise the note is re-filed
   under `<Label>/`, so the label ladder recovers the label the export did not
   write (§10.3). **Re-filing moves the folder and nothing else**: the file
   keeps the stem it arrived with, because a stem is the link namespace (§3,
   §5.2) and a `[[wikilink]]` in somebody else's prose spells that stem, not
   the note's title — renaming the file after the title dangles every one of
   them, and the next import mints a `_provisional` stub for each. Only a node
   no file ever backed has no stem to keep, and it is named
   `<title or id>.md`; §10.3's `id:` then carries the identity the stem does
   not spell. A preserved folder note keeps its `X.md`-beside-`X/` spelling. In
   a segment the export composes, `/ \ : * ? " < > |` and control characters
   become `-`, and trailing dots and spaces are stripped — Windows strips them
   on write, and a filename that differs from the one the manifest recorded
   would be refused by the next export. A case-insensitive path collision
   appends `-<id>`.
3. **Frontmatter.** `type:` is never emitted — the folder carries the label, so
   emitting it would make a later folder move a no-op. `id:` is emitted only
   when the id differs from the filename stem, and `title:` only when the next
   import would not recover it: §3's ladder reads a `name:` key and **the
   body's first heading** before the stem, so a note titled after its file but
   opening with a heading still writes its `title:`. Keys are sorted; dotted keys expand back into nested maps, except
   where the expansion would have to grow through a property that is already a
   scalar, which keeps the literal dotted key; lists become YAML sequences, and
   a list or map *inside* a sequence is written as JSON, which is YAML flow
   syntax; dates and datetimes become ISO strings; points become
   `POINT(lon lat)` WKT; a whole float keeps its `.0`. `file_path`,
   `concept_id`, `_provisional`, the body property and the attachment-derived
   properties are never frontmatter, and embeddings are not properties at all —
   they live in the graph's own store and no export writes them.
4. **Quoting.** A string is quoted when, unquoted, it would come back as
   something else. In the order checked: it is empty or padded with
   whitespace; it starts with a YAML indicator character (`-?:,[]{}#&*!|>'"%@`
   or a backtick) — which is what quotes a `[[wikilink]]`, since bare `[[A]]`
   is a flow sequence and not the name it spells; it contains `: `,
   a ` #` comment opener or a newline, or ends in `:`; it spells a boolean or
   null in any of YAML's casings, including the 1.1 words `y`/`n`/`yes`/`no`/
   `on`/`off`; it parses as an integer or a float, or starts `0x`/`0o`; or it
   matches a date or datetime §4.2 would infer — which is asked of the reader's
   own inference, so the two cannot disagree. Everything else is written bare.
5. **Body.** The `body` property verbatim, starting on the line after the
   closing `---` with no blank line inserted: the reader keeps everything below
   the terminator, so a separator written here would come back *as* body and
   the next export would write another one. A note whose author left a blank
   line there has it in its body and gets it back. A node without a body
   produces a frontmatter-only file; a node with a body and nothing to say
   above it produces a file with no frontmatter block at all. Human-owned prose
   is never rewritten, with one declared exception: the table under a heading
   `export.edge_tables` names (§10.6) belongs to the export, which rewrites it
   whole or appends it where it is missing. The vault asked for that table by
   name; nothing else in the body is touched.
6. **Edges** become frontmatter lists keyed `lower_snake(TYPE)`, with wikilink
   values: `depends_on: ["[[Seismic interpretation]]"]`. The key is exactly
   what §4.3's `UPPER_SNAKE(key)` turns back into that type. Two kinds of edge
   are left out. **Every edge whose target is not a file**, which is how
   `CONTAINS` (it leaves a `Folder`), `TAGGED`, `HAS_IMAGE`, `HAS_ATTACHMENT`
   and every hub edge leave — none of them named as a special case, because a
   target that is not a file has no wikilink to name it. And **an edge the body
   already states**: the prose is re-read with the reader's own scanner, and an
   edge is left out when a body link reaches the same target *with the same
   type* — which is the type §5.3's ladder gives it, a `{type}` suffix as much
   as a heading rung, not `LINKS_TO` by assumption. The type has to match both ways. Writing an edge the body
   already states makes a second edge on the next import, one carrying the
   body's `section` and one carrying nothing; dropping one whose type differs
   from what the body's link would produce retypes it.
   An edge to a `_provisional` stub is written as `[[<the unresolved name>]]`,
   so a dangling link declared in frontmatter dangles in the same place next
   time. An ambiguous target is written folder-qualified, `[[Label/Name]]`.

   **An edge whose type `export.edge_tables` declares** (§7.3) is written as a
   table in the body instead of a frontmatter list, and keeps its properties.
   This is the only place an export adds prose to a note, and it does so
   because the vault asked for it — an undeclared type is never written into a
   body, because human prose is never rewritten (§10.5), and its properties are
   counted as loss 1 exactly as before.

   **The declared heading's first table is the exporter's.** It rewrites that
   table whole — header row, delimiter row and rows — keeping only the name the
   author gave its first column; where the heading carries no table, one is
   written at the end of what that heading itself holds, before the next
   heading of any level; where the heading is absent, `## <heading>` and the
   table are appended to the body. A table under a *nested* heading belongs to
   that heading and is left alone. A declared type with no edges to write
   **removes** the table the export owns, because leaving it would make those
   edges again on the next import. That ownership is what makes an exported
   vault a fixed point: the second export finds its own table and replaces it,
   rather than appending a second one.

   The first column holds the `[[target]]`, carrying the edge's `anchor` as its
   fragment and its `label` as its display text; every **other** property gets a
   column, named for it, in name order. `section`, `anchor`, `row` and `label`
   get none: the reader mints all four from the table itself (§7.1). Rows are
   ordered by `row` where the edges carry one — which is the order the author's
   own table had — and by target and then by their columns where they do not. A
   cell is text, so a property that was not a string comes back a string; `row`
   is the exception, minted as an integer by the reader.

   Reading the table back needs the matching `structure.tables … edges: true`
   rule (§7.1), which lives in `vault.yaml`, which no export writes (loss 4):
   copy that file across, or the exported table reads as prose. An export
   **warns** (§10.10) when a declared type has no such rule in the source
   vault's own `vault.yaml`, and when no exported note emits it at all — which
   is what a type only derived nodes emit looks like (§10.1). The table's own
   cells are links like any other, so the next import reads a `LINKS_TO` (or
   whatever the heading ladder types) beside the declared edge, exactly as it
   does from an author's own table.
7. **Overwrite safety.** `.kglite/export-manifest.json` records every file the
   export wrote: `{"kglite_vault": 1, "files": {"<vault-relative path>":
   "<sha256 hex>"}}`. On the next export a file whose current hash differs from
   its manifest entry was edited by a human, and the export refuses it; a file
   absent from the manifest was written by somebody else, and the export
   refuses that too. `force` overrides both, and nothing else. A refused file
   stays in the manifest, so the deletion pass does not read it as a file whose
   node disappeared; a file absent from the manifest is never deleted at all;
   and a node deleted from the graph removes its file only when the manifest
   owns it and the bytes still match. A file whose bytes already equal what the
   export would write is not rewritten, so an unchanged export moves no
   modification times — attachments included, so a re-export of a vault of
   ten thousand pictures rewrites none of them. A copied attachment is written
   with the *source* file's modification time rather than the time of the copy,
   so the `mtime` §6.3 stats off it reads the same on both sides. A manifest that will not parse, or that names a version
   this build does not write, fails the export rather than being guessed at.
8. **Determinism.** Frontmatter keys sorted, edge lists sorted, file order
   stable, the manifest's own keys sorted: exporting the same graph twice is
   byte-identical.
9. **Documented losses.** Six, and no others:

   1. **Edge properties** (`section`, `anchor`, `alt`, `ordinal`, `label`) are
      not written for a type `export.edge_tables` does not declare — a
      frontmatter list carries targets — and are counted in the export report.
      A declared type keeps them, as a table (§10.6). An edge the *body* states
      keeps them anyway: the prose
      travels verbatim and the next import re-derives them from it with the
      same scanner. An edge only frontmatter carried has none to begin with, so
      the loss bites exactly where an edge with properties was never written in
      prose — a graph that was not built from a vault.
   2. **Attachment bytes** are copied only when the caller names the source
      root the graph was built from; otherwise the references are reported as
      unresolvable and come back as `missing: true` stubs (§6.6). A copy that
      *does* travel keeps the source file's modification time (§10.7), so
      §6.3's `mtime` is not one of these losses and the fixed point does not
      depend on which second the export ran in.
   3. **Synthesized nodes are not files**, so `Folder`, `Tag`, `Image`,
      `Attachment` and stub nodes are whatever the exported layout regenerates
      — the same ones where the prose decides, a different set of `Folder`s
      because the layout is now one folder per label.
   4. **`.kglite/vault.yaml` is not written** — a graph does not carry it — so
      what it declared is gone: hub nodes and their edges, the index and
      text-index declarations, the `embed:` targets, and `heading_edges`
      retyping, which shows up as the built-in ladder's own type appearing
      *beside* the declared one, because the typed edge is written as a
      frontmatter key and the prose still reads as what the ladder says.
   5. **A declared `type:` survives only where §4.2's inference agrees with
      it.** The writer emits an `int` as an int and a quoted string quoted, so
      most declarations are re-derived for free. The exception is temporal:
      a top-level string that looks like a date comes back a **date**, because
      quoting alone does not stop inference and only `types:` does.
   6. **A re-filed note carries its body verbatim**, so a note-relative
      reference in it resolves from the new location, not the old one — a
      §9 error when the climb now leaves the vault, and a different file when
      the same name exists in both places.

   Round-tripping is defined against exactly those: importing an exported vault
   reproduces the imported graph apart from them. They are taken **once**, on
   the way out of the author's vault, so an exported tree is a fixed point —
   exporting it, reading it back and exporting it again is byte-identical, and
   so is the graph. Two things move the bytes at all, and only on the first
   export after them: loss 5, and a declared edge table, whose table the export
   owns and writes in its own spelling (§10.6).
10. **Warnings.** The report carries a line per declared edge table the export
    could not write as asked: a type whose source vault declares no
    `structure.tables … edges: true` rule to read the table back, a type no
    exported note emits, and a source `vault.yaml` that would not parse. None
    of them fails the export — every one is a table the author will not get
    back, said at the moment it can still be fixed rather than three steps
    later as a missing edge.

## 11. Converter checklist

What a converter must emit, in order:

1. **One `.md` file per source document**, UTF-8, under a directory tree that
   mirrors the hierarchy you want. Use the folder-note layout (§2.3):
   `X.md` beside `X/`. A folder note at the **vault root** has no folder above
   it to take a label from, so give each one a `type:` (§2.3).
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
   A download link is the same thing without the `!`: `[the handbook](x.pdf)`
   is an `Attachment` reference, and the file has to be in the vault too.
   **Percent-encode every markdown target you emit** — at minimum spaces, `(`,
   `)` and a literal `%`, which otherwise truncate or hide the reference
   (§6.1) — and keep `#` and `?` out of the filenames themselves, because no
   encoding reaches them. Wikilink targets are never encoded.
8. **`.kglite/vault.yaml`** with `kglite_vault: 1`, your `default_label`,
   `folder_notes`, `hubs`, `heading_edges`, `types`, `indexes`, `text_indexes`
   and `embed` (§7) — plus `structure:` when the notes carry structure worth
   querying, which §13 is the guide to. It replaces the graph-building script: everything
   declarative lives here and is re-applied on every rebuild.
9. **`.kglite/skills/` and `.kglite/recipes/`** when the vault is served to an
   agent (§8).
10. **Run `kglite okf check <dir>`.** Zero errors is the bar; add `--strict`
    to your own test suite once the warnings are down to the ones you accept,
    and `--json` when the suite wants the finding lists rather than the text.

`examples/html_to_vault.py` in this repository is a worked converter following
exactly this checklist — HTML pages plus a JSON table of contents in, a
validated vault out — and is the fastest way to see each step in code. It shows
what item 1 means for a corpus that routes by directory: a page's identity is
its **path** below the source root, `<dir>/index.html` naming `<dir>`, because
a site serving clean URLs has one filename for every page in it.

## 12. Rebuild and provenance

A build stamps the graph with `source_root` (the absolute directory it walked),
`source_fingerprint` — a 64-bit summary of the `(relative path, size,
modification time)` of every file the build read: each note, each attachment,
and everything under `.kglite/` — and `source_dialect`, the dialect it read
them with. All three are persisted in the `.kgl`, so a process that opens one
later can ask whether the vault behind it has moved on without being told the
path, or the conventions, again.

The dialect belongs in the stamp because the fingerprint depends on it:
`.kglite/` is a build input for `obsidian` alone, and the dialect decides which
files are notes at all, so one untouched directory has one fingerprint per
dialect. A caller comparing across two of them is told "changed" every time.
So `okf.rebuild_if_changed(graph)` and `kglite okf status <dir> --graph f.kgl`
read the dialect off the stamp when they are not given one, and refuse a given
one that contradicts it, naming both. A `.kgl` written before the dialect was
stamped (0.17.8–0.17.10) keeps the old behaviour — the caller's dialect, `okf`
when they name none — and the rebuild report says the stamp was missing.

`okf.fingerprint(dir)` recomputes it (a `stat` pass; no note is read) and
`kglite okf status <dir> [--graph f.kgl]` prints it, exiting non-zero when the
graph is stale. A *path* carries no stamp, so `okf.fingerprint` still defaults
to `dialect="okf"` and must be told `"obsidian"` for a vault.
`okf.rebuild_if_changed(graph)` returns `None` when the
fingerprint still matches and a **new** graph otherwise, carrying the old
graph's vectors across by `(label, id)`: an unchanged note keeps its vector and
its stored text hash, so only notes whose text moved are re-embedded. Nodes a
`structure:` block derived add one fallback to that, because their ids move
when the prose around them does: a derived node whose id is new but whose
`chunk_hash` matches exactly one old node of its label is that node, and
carries its vector and hash across too (§7.1). A note
that changed **label** — by moving between folders under a folder-derived label
— is a different node and re-embeds; that is the contract, not a defect.
`embed:` targets then run a changed-mode pass when a model is bound.

Two consequences worth knowing. Modification times are compared as whole
seconds, so a file rewritten within the same second to exactly the same length
reads as unchanged. And every non-hidden file under the root is a candidate
attachment, so writing the `.kgl` *into* the vault changes the vault: keep it
outside.

## 13. Modelling guide for converters and authoring agents

A source corpus already has structure — a table of contents, sections,
procedures, parameter tables, admonitions. This section says what to write so
that structure arrives in the graph, and what to avoid writing because it
arrives as nothing. It is normative in the same sense as the rest: a converter
that follows it produces a vault this spec describes.

> **Frontmatter is the node, headings are the sections, `^blockid` is the
> citable unit.** Everything below follows from those three.

§13.4 and §13.5 are about the other direction — annotating a corpus that is
already converted. The
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
  property that makes a vault worth having, that a human can open it; a
  `structure:` block yields the same nodes from the same pages.
- **Raw HTML tables and definition lists.** HTML tags are never interpreted
  (§1.4), so a `<table>` is prose and its rows are nothing. Emit a GFM table;
  emit headings or a table for a `<dl>`, never `<dt>`.
- **Admonitions flattened into paragraphs**, or folded into `note` because the
  source word is not one of Obsidian's thirteen. Callout kinds are arbitrary —
  keep `versionadded`, keep `deprecated` (§5.7).
- **Unlabelled fences** when the source knew the language. `langs: [python]`
  then matches nothing; write the language, or omit `langs:` and take them all.
- **JSON-encoded lists.** `tags: "[\"a\", \"b\"]"` is one string. Write a YAML
  sequence (§4.2).
- **Nested frontmatter maps.** They flatten to dotted keys that Obsidian's own
  Properties UI cannot edit. Keep frontmatter to scalars and lists.
- **`type:` on every file when the folder already says it.** The label ladder
  reads the folder (§2.1) and an export never writes `type:` back (§10.3), so
  the declaration only makes a later folder move a no-op.
- **`default_label:` in a vault whose folders are its labels.** It is rung 2
  and the folder is rung 3 (§2.1), so it wins over every folder and relabels
  the whole vault — a converter that declared `default_label: Article` and a
  `types:` block per folder label got 1 237 `Article`s and no `Api`, with
  nothing but the per-label counts in the report to say so.
- **Absolute paths and `../` climbs** out of the vault: a §9 error, in prose and
  in a typed-edge key alike.
- **SVG.** It is stored as an `Attachment` and is not delivered as an image;
  rasterise it when you build the source (§6).
- **The same heading text twice in one note.** Obsidian can link only the first
  and so can this spec; the second takes a `~2` id and a warning. Add a block id.
- **`[`, `]`, `|` or `#` inside a heading you link to.** `[[Note#Heading]]` has
  no escape for any of them: `#` starts the next path component, `|` starts the
  display text, and `[[`/`]]` end the link. The section is still built; it is
  the *link* that cannot be written. Rename the heading, or cite a `^blockid`
  on the paragraph below it.

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

There is no `default_label:` on purpose: it is rung 2 of the label ladder
(§2.1) and the folder name is rung 3, so declaring one labels *every* note with
it and a converted corpus's folders stop saying anything. Declare it only for a
vault whose notes are genuinely one kind, as §7's example is.

`code_fences:` here omits `langs:` on purpose — the corpus lost its languages in
conversion, and every fence is still an example. Fix the converter and the
filter becomes worth declaring.

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
paragraph only implies: an `address` property and a `DOCUMENTED_IN` edge to
the `Wells` note. Neither reaches the section's `text` or the chunk packed
from it, so retrieval still sees only the prose, and the file still renders in
Obsidian exactly as it did.

```cypher
MATCH (s:Section) WHERE s.address CONTAINS 'Task pane'
RETURN s.title, s.address
```

Reach for this when the fact is **about one section** and the alternative is a
frontmatter key about the whole note, which would be wrong, or a table nobody
reads. A fact about the whole note still belongs in frontmatter (§4.3).

### 13.5 Tagging what a paragraph is, not what the page is about

A page mixes kinds of statement: what the reader can do, and what will go
wrong if they do it. Both are one sentence inside a longer section, so
frontmatter cannot carry either.

```markdown
## Importing wells

Use the import dialog to load a deviation survey. #intent/import-wells

The datum is not checked on import. #warning
```

With `tag_labels: {"intent/*": {label: Intent, edge: HAS_INTENT}}` declared
(§5.5), the first tag mints an `Intent` node `import-wells` joined from the
**chunk** that holds it, and the second stays an ordinary tag — but lands in
that chunk's own `tags` list as well:

```cypher
MATCH (c:Chunk)-[:HAS_INTENT]->(i:Intent) RETURN i.title, c.text
MATCH (c:Chunk) WHERE 'warning' IN c.tags RETURN c.concept_id, c.text
```

Reach for `tag_labels:` when a family of tags is really a **kind of thing** the
corpus has many of and queries by name (`intent/…`, `task/…`, `product/…`);
leave a one-word marker like `#warning` as a tag, where the per-chunk `tags`
list already answers for it. The caveat is the chunk's width: two paragraphs
often pack into one chunk, so a marker meant for one of them marks both unless
the author writes `<!-- kglite chunk -->` between them (§5.8) or gives the
paragraph a `^block-id` (§5.7).

### 13.6 The loop

1. Convert a **sample** — fifty pages, not the corpus.
2. `kglite okf check <dir> --strict`. Errors are spec violations; warnings at
   this stage are usually the converter's, not the corpus's.
3. Read the report's per-label counts against what you know the sample holds. A
   rule that matched nothing is a warning and the fastest defect there is
   (§7.1): the heading regex is wrong, or the tables are still HTML.
4. Fix the converter, not the vault. Then run the whole corpus, and keep
   `--strict` in the converter's own test suite (§11.10).
