---
name: vault_authoring
description: "How this server's graph is built from the markdown vault it serves: the five rules that decide what a note becomes, and what a note's own body becomes where `.kglite/vault.yaml` declares `structure:`. TRIGGER when you are about to write or edit a `.md` note under the served directory, when a note you wrote did not appear in the graph or appeared with the wrong label, when you need to link two notes, when you are choosing frontmatter keys, or when you are converting a source corpus into notes. ALSO TRIGGER before calling `rebuild_graph` — it reports errors and warnings, and the difference matters. SKIP for read-only querying: `cypher_query` and `graph_overview` need nothing from here."
applies_to:
  mcp_methods: ">=0.4.8"
  kglite_mcp_server: ">=0.17.8"
references_tools:
  - rebuild_graph
  - cypher_query
auto_inject_hint: true
applies_when:
  tool_registered: rebuild_graph
---

The markdown files are canonical. In `--vault` mode, edits rebuild before the
next tool call; `rebuild_graph` forces it now. In `--graph` mode, rebuild the
served `.kgl` first, then reload or restart — restart alone does not convert
markdown. Never write the graph to change the notes.

## Five rules decide what a note becomes

1. **The folder is the label.** `Geology/Faults.md` is a `:Geology` node
   unless frontmatter says `type:` or `.kglite/vault.yaml` sets
   `default_label:`.
2. **The filename stem is the id, and the link target.** `Faults.md` is
   reached as `[[Faults]]` from anywhere; frontmatter `id:` overrides it.
3. **A wikilink-valued frontmatter key is an edge, not a property.**
   `depends_on: "[[Horizons]]"` makes a `DEPENDS_ON` edge and stores no
   property of that name. A plain string stays a property.
4. **Images are note-relative and live in the vault.** `![alt](img/x.png)`
   becomes an `Image` node — copy the file in. A path leaving the vault
   (`../`, absolute, `file:`) is an error.
5. **The body is prose**, stored whole and searched; nothing in it is ever
   rewritten.

Optional `.kglite/vault.yaml` declares types, indexes, ontology, embeddings and
`structure:`. `.kglite/skills/` and `.kglite/recipes/` are build inputs; skills
refresh after a graph swap, while recipes and named recipe tools need a restart.
`VAULT.md` is the full format and lifecycle reference.

## What the body becomes

**Frontmatter is the node, headings are its sections, a paragraph ending in
` ^id` is the citable unit.** A `structure:` block in `.kglite/vault.yaml`
turns the shapes a note already has into nodes of their own, under Obsidian's
own ids (`Note#Heading`, `Note#^id`) — still linkable, still navigable.
Write the markdown; declare the rule:

| Write … | Declared by … | Graph gets … |
|---|---|---|
| headings, one level per depth | `sections:` | `Section` + `PARENT_SECTION` / `NEXT_SECTION` |
| prose under them, a cited passage ending ` ^id` | `chunks:` | retrieval-sized `Chunk`s to embed and search; the `^id` one is fixed |
| `> [!versionadded] Title` callouts | `callouts:` | `Note {kind, title, text}` |
| fences carrying their language | `code_fences:` | `Example {lang, code}` |
| numbered lists | `ordered_lists:` | `Procedure` + `ProcedureStep` + `NEXT_STEP` |
| a **GFM** table of parameters or fields | `tables:` | one node per row, columns as properties — or, with `edges: true`, one *edge* per row |
| a heading that is a symbol name | `key_from_heading:` | that section relabelled, `qualified_name` stored |
| `[[Target\|the words you meant]]` | — | those words as the edge's `label` |

`inherit:` copies chosen note properties onto the derived nodes,
`embed_text:` renders what they are embedded on, `edge_defaults:` stamps a
constant property on every edge of a type. Declare only rules the notes carry:
one matching nothing is a warning.

## Anti-patterns

- **One file per tiny record** — per chunk, step or parameter. `structure:`
  yields the same nodes from pages a human can still open.
- **Raw HTML tables and definition lists.** HTML is never interpreted, so a
  `<table>` is prose and its rows are nothing. Emit GFM; emit headings or a
  table for a `<dl>`.
- **Admonitions flattened into paragraphs**, or folded into `note` because the
  source word is not one of Obsidian's thirteen. Kinds are arbitrary — keep
  `versionadded`.
- **Unlabelled fences** when the source knew the language.
- **JSON-encoded lists.** `tags: "[\"a\"]"` is one string; write a YAML
  sequence. Keep frontmatter to scalars and lists: a nested map flattens to
  dotted keys Obsidian's Properties UI cannot edit.
- **`[`, `]`, `|` or `#` in a heading you link to.** `[[Note#Heading]]` has no
  escape for them — rename it, or cite a `^id` below it.
- **The same heading text twice in one note.** Only the first is linkable; the
  second takes a `~2` id and a warning.

## The edit loop

Write the note, then `rebuild_graph` (the watcher gets there by itself on the
next tool call; call it when you want the report now). Outside this server the
same check is `kglite okf check <vault> --strict`. Read its two classes
differently:

- **Errors** mean the vault does not meet the format — unparseable
  frontmatter, a reserved key of the wrong shape (non-string `id:`, scalar
  `tags:`), an id collision, an invalid `.kglite/vault.yaml`, a path escaping
  the vault. These are defects in what you wrote; fix them.
- **Warnings** are normal while a vault is being written: a dangling wikilink
  (the note does not exist *yet* — the edge survives via a provisional node), a
  missing image, a case-only filename collision, a `structure:` rule that
  matched nothing (usually a heading pattern, or tables still in HTML).

Converting a corpus? Do fifty pages first, read the per-label counts against
what you know that sample holds, and fix the converter — never the vault it
wrote.

A build that fails outright leaves the previous graph serving and returns the
message; it never serves an empty graph. After a rebuild that moved files or
changed edge types, call `graph_overview` before writing Cypher: rule 1 means a
moved file is a *relabelled* node, and a query against the old label returns
zero rows rather than an error.
