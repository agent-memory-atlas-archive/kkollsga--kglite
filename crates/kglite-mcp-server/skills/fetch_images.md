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

Images in this vault are **nodes, not bytes**. Every `![…](…)` reference in a
note becomes an `Image` node whose `id` *is* its vault-relative path, joined to
the note by `HAS_IMAGE`. `fetch_images` is the only route that returns the
bytes, and it returns them only for the paths you name.

## Query first, fetch second

`Image` nodes carry `path`, `text` (the alt texts plus the titles of the notes
using it), `mime` and `size_bytes`. That is usually enough to choose — and
often enough to answer without fetching anything.

```cypher
MATCH (a:Article)-[r:HAS_IMAGE]->(i:Image)
WHERE a.title = 'Fault interpretation'
RETURN i.id, i.text, i.size_bytes, r.alt
ORDER BY r.ordinal
```

Then fetch the one or two that matter:

```json
{"items": ["img/faults.png"]}
```

`items` takes vault-relative paths **or** `Image` ids — they are the same
string, so whichever the query handed you works as-is. Absolute paths, `~`,
`file:` URLs and `..` segments are refused before the file is looked up; there
is no way to reach outside the served directory, so do not try to construct
one.

## What comes back

One image content block per delivered file, in request order, followed by one
text block listing each item as delivered (path, MIME, byte count) or refused
(path, reason). A call where *some* items are refused still succeeds — read the
text block to see which. A call where *every* item is refused is an error whose
body is the same list of reasons.

## The caps, and what to do about them

- **4 images per call.** Extras are refused; ask for them in a second call.
- **4 MiB per image, 12 MiB per call.** An over-cap image is refused with its
  byte count named. It is never resized and never truncated, so there is no
  smaller version to ask for — `max_bytes` can only lower the ceiling, never
  raise it. Check `size_bytes` in the query if you want to know first.
- An operator can set all three lower in the server's
  `extensions.fetch_images` block; the tool description states the live values.

## What is never delivered

png, jpeg, gif and webp are delivered. Everything else is refused with its type
named — **SVG included**, along with PDFs and every other attachment. Those
still exist as nodes you can query (`Attachment`, `HAS_ATTACHMENT`); their
*contents* are not available through this server. If a diagram only exists as
SVG, say so rather than guessing at what it shows.
