---
name: vault_authoring
description: "How this server's graph is built from the markdown vault it serves, and the five rules that decide what a note becomes. TRIGGER when you are about to write or edit a `.md` note under the served directory, when a note you wrote did not appear in the graph or appeared with the wrong label, when you need to link two notes, or when you are choosing frontmatter keys. ALSO TRIGGER before calling `rebuild_graph` — it reports errors and warnings, and the difference matters. SKIP for read-only querying: `cypher_query` and `graph_overview` need nothing from here."
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

The graph is **derived from the markdown files**, not stored separately. The
files are canonical: edit a note and the graph rebuilds on the next tool call.
Never write the graph to change the notes.

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
5. **The body is prose**, stored whole and searched; nothing in it is
   rewritten or reformatted.

Optional `.kglite/vault.yaml` declares profile, property types, indexes,
ontology and embed targets; `.kglite/skills/` and `.kglite/recipes/` carry the
vault's own agent guidance. All of it is re-read on every build. The full
format spec is `VAULT.md` in the kglite repository — read it before inventing
a key, not before writing an ordinary note.

## The edit loop

Write the note, then `rebuild_graph` (the watcher gets there by itself on the
next tool call; call it when you want the report now). Outside this server the
same check is `kglite okf check <vault>`, which prints the identical report.

Read its two classes differently:

- **Errors** mean the vault does not meet the format — unparseable
  frontmatter, a reserved key of the wrong shape (non-string `id:`, scalar
  `tags:`), an id collision, an invalid `.kglite/vault.yaml`, a path escaping
  the vault. These are defects in what you wrote; fix them.
- **Warnings** are normal while a vault is being written: a dangling wikilink
  (the note does not exist *yet* — the edge survives via a provisional node), a
  missing image, a case-only filename collision.

A build that fails outright leaves the previous graph serving and returns the
message; it never serves an empty graph.

After a rebuild that moved files or changed edge types, call `graph_overview`
before writing Cypher: rule 1 means a moved file is a *relabelled* node, and a
query against the old label returns zero rows rather than an error.
