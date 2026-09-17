---
name: fetch_images
description: "How to get at the pictures this server's vault contains without pulling megabytes you did not ask for. TRIGGER when a note references an image and the answer depends on what the image shows, when a query returned `Image` nodes and you must decide which to look at, or when `fetch_images` refused something. SKIP when the image is decoration, when the note's text already answers the question, and for every non-image file — `fetch_images` delivers images only."
applies_to:
  mcp_methods: ">=0.4.12"
  kglite_mcp_server: ">=0.17.8"
references_tools:
  - fetch_images
  - cypher_query
auto_inject_hint: true
applies_when:
  tool_registered: fetch_images
---

Images are **nodes, not bytes**. Every `![…](…)` reference becomes an `Image`
node whose `id` *is* its vault-relative path, joined to the note by
`HAS_IMAGE`. `fetch_images` is the only route that returns bytes, and only for
the paths you name.

## Query first, fetch second

`Image` nodes carry `path`, `text` (alt texts plus the titles of the notes
using it), `mime` and `size_bytes` — usually enough to choose, often enough to
answer without fetching anything.

```cypher
MATCH (a:Article)-[r:HAS_IMAGE]->(i:Image)
WHERE a.title = 'Fault interpretation'
RETURN i.id, i.text, i.size_bytes, r.alt ORDER BY r.ordinal
```

Then fetch the one or two that matter: `{"items": ["img/faults.png"]}`.
`items` takes vault-relative paths **or** `Image` ids — the same string, so
whatever the query handed you works as-is. Absolute paths, `~`, `file:` URLs
and `..` segments are refused before the file is looked up.

## What comes back, and the caps

One image block per delivered file in request order, then one text block
listing each item as delivered (path, MIME, bytes) or refused (path, reason).
A call where *some* items are refused still succeeds — read that block. A call
where every item is refused is an error carrying the same reasons.

- **4 images per call**; extras are refused, so ask again.
- **4 MiB per image, 12 MiB per call.** An over-cap image is refused with its
  byte count named. It is never resized or truncated, so there is no smaller
  version to ask for; `max_bytes` only lowers the ceiling. Check `size_bytes`
  first if it matters.
- An operator can set all three lower in `extensions.fetch_images`; the tool
  description states the live values.

png, jpeg, gif and webp are delivered. Everything else is refused with its
type named — **SVG included**, along with PDFs and every other attachment.
Those remain queryable as `Attachment` / `HAS_ATTACHMENT` nodes; their
contents are not available here. If a diagram exists only as SVG, say so
rather than guessing at what it shows.
