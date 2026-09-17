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

The graph this server answers from is **derived from the markdown files**, not
stored separately. The files are canonical: edit a note and the graph rebuilds
itself on the next tool call. Never write the graph to change the notes.

## A minimal valid note, `Geology/Faults.md`

```markdown
---
title: Fault interpretation
depends_on: "[[Horizons]]"
---
Picked on the 2024 survey. See [[Horizons]] for the surfaces.

![Fault map](img/faults.png)
```

## Five rules decide what a note becomes

1. **The folder is the label.** `Geology/Faults.md` is a `:Geology` node
   unless its frontmatter says `type:`, or `.kglite/vault.yaml` sets a
   `default_label:`.
2. **The filename stem is the id, and the link target.** `Faults.md` is reached
   as `[[Faults]]` from anywhere in the vault; an `id:` in frontmatter
   overrides it.
3. **A wikilink-valued frontmatter key is an edge, not a property.**
   `depends_on: "[[Horizons]]"` makes a `DEPENDS_ON` edge and stores no
   property of that name. A plain string value stays a property.
4. **Images are note-relative and live in the vault.** `![alt](img/x.png)`
   becomes an `Image` node — copy the file in. A path that leaves the vault
   (`../`, an absolute path, a `file:` URL) is an error.
5. **The body is prose.** It is stored whole and searched; nothing in it is
   rewritten, split or reformatted.

Optional configuration lives in `.kglite/vault.yaml` (profile overrides,
declared property types, indexes, ontology, embed targets), and the vault's own
agent guidance in `.kglite/skills/*.md` and `.kglite/recipes/*.md`. All of it
is re-read on every build, so editing a file is the whole update procedure.

## After editing: `rebuild_graph`

The watcher rebuilds on the next tool call by itself; call `rebuild_graph` when
you want the rebuild *now*, or when you want to read the build report.

Read the report's two classes differently:

- **Errors** mean the vault does not meet the format: unparseable frontmatter,
  a reserved key of the wrong shape (a non-string `id:`, a scalar `tags:`), an
  id collision, a `.kglite/vault.yaml` that will not validate, a link or image
  path that escapes the vault. Fix these — they are a defect in what you wrote.
- **Warnings** are normal in a vault being written: a dangling wikilink (the
  note does not exist *yet*), a missing image, a case-only filename collision.
  A dangling link still creates a provisional node, so the edge survives the
  note being written later.

A build that fails outright — a broken `.kglite/vault.yaml` is the usual cause
— leaves the previous graph serving and returns the message. It never serves an
empty graph.

## Checking the schema you just changed

After a rebuild that changed labels or edge types, call `graph_overview` before
writing Cypher against the new shape: rule 1 means a moved file is a *relabelled*
node, and a query written against the old label returns zero rows rather than an
error.
