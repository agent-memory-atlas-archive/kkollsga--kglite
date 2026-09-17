"""Type stubs for kglite.okf — Open Knowledge Format bundle ingestion."""

from __future__ import annotations

from kglite import KnowledgeGraph

def build(
    path: str,
    *,
    dialect: str | None = ...,
    require_frontmatter: bool | None = ...,
    respect_skip: bool = ...,
    skip_dirs: list[str] | None = ...,
    with_body: bool | None = ...,
    embed: bool = ...,
) -> KnowledgeGraph:
    """Build a :class:`~kglite.KnowledgeGraph` from an OKF bundle directory.

    A bundle is a directory tree of markdown files with YAML frontmatter,
    cross-linked by markdown links. Each non-reserved ``.md`` file becomes one
    node (label from the frontmatter ``type``, or ``Concept`` when absent; id =
    the bundle-relative path minus ``.md``); frontmatter keys become node
    properties (``tags`` and nested maps are JSON-encoded); markdown links become
    typed edges. The ``"obsidian"`` dialect changes the label, id and value
    rules — see ``dialect`` below. Link types are inferred most-specific-first: an explicit link
    title (``[x](/y.md "JOINS_WITH")``) → the enclosing section header
    (``# Citations`` → ``CITES``) → ``LINKS_TO``. Links to not-yet-written
    concepts become ``_provisional`` stub nodes
    (``MATCH (n {_provisional: true})``).

    The graph is enriched by default with synthesized nodes that densify it:
    ``Tag`` nodes (``(:Concept)-[:TAGGED]->(:Tag)`` from ``tags``), ``Source``
    nodes (external ``http(s)`` links → ``(:Concept)-[:CITES]->(:Source)``), and
    ``Folder`` nodes (the directory tree, ``(:Folder)-[:CONTAINS]->`` concepts /
    subfolders, with each directory's ``index.md`` enriching its Folder).

    Ingestion is *partial*: the markdown body is not stored unless ``with_body``
    is set — each node keeps a ``file_path`` pointer instead. The ``"obsidian"``
    dialect stores it by default.

    Args:
        path: Bundle root directory.
        dialect: ``"okf"`` (default) for strict markdown links; ``"loose"`` to
            also resolve ``[[wikilinks]]`` and tolerate concepts with no
            frontmatter ``type``; ``"obsidian"`` for the **vault** format
            specified in ``VAULT.md``, which changes several defaults:

            * **Label**: frontmatter ``type:`` → the note's *top-level* folder
              name (verbatim — no singularising, no case change) → ``Note``.
              ``metadata.type`` is not a rung here; it stays an ordinary
              property.
            * **Id**: frontmatter ``id:`` → the filename stem, so a note keeps
              its identity when it moves between folders. When two notes
              resolve to the same id, every one of them falls back to its
              path-relative id and the build reports the collision.
            * **Body**: stored as ``body`` by default (``with_body`` still
              overrides).
            * **Frontmatter values**: sequences and nested maps stay native
              ``list`` / ``dict`` properties instead of JSON strings, and a
              top-level string spelling an ISO ``YYYY-MM-DD`` date or an
              RFC 3339 timestamp becomes a date / datetime value.
            * **Frontmatter is not required** (``require_frontmatter`` defaults
              to ``False``), so a plain ``.md`` file is a note.
            * **Links**: every body link carries the enclosing heading's text
              as a ``section`` edge property, and a ``[[Note#Heading]]`` or
              ``[x](note.md#frag)`` link its fragment as ``anchor`` — the
              fragment never changes which node the link reaches. A note's
              ``aliases:`` answer link resolution between the stem and slug
              rungs. ``![[Note]]`` is an ``EMBEDS`` edge; ``![[image.png]]``
              is an attachment and not a link.
            * **Frontmatter edges**: a key whose value is a wikilink string,
              or a list of nothing but wikilink strings, becomes edges typed
              ``UPPER_SNAKE(key)`` and is *not* stored as a property; a list
              mixing wikilinks with plain strings stays a property. The
              reserved ``parent:`` key emits ``CHILD_OF`` from the note to
              each named parent.
            * **Tags**: inline ``#tags`` in the body join the same ``Tag`` hub
              as ``tags:`` (fenced code, inline code spans and URL or wikilink
              fragments are skipped), while the ``tags`` property keeps
              reporting only what the frontmatter said.
        require_frontmatter: When ``True``, only ``.md`` files with a YAML
            frontmatter block are ingested — the discriminator between
            *structured* knowledge (OKF concepts, Claude memories) and plain
            markdown (READMEs, notes). Point at a parent of many projects to
            sweep out only the structured files across all of them. Set
            ``False`` to ingest every ``.md``. Left unset it takes the
            dialect's default: ``True`` for ``"okf"`` / ``"loose"``, ``False``
            for ``"obsidian"``. Under the OKF ladder node labels fall back
            ``type`` → ``metadata.type`` → ``Concept`` and titles ``title`` →
            ``name`` → first ``# H1`` → file stem, so Claude memories land as
            ``:feedback`` / ``:project`` / etc. with their ``name`` as title.
        respect_skip: When ``True`` (default), honor a ``kg_skip: true``
            frontmatter marker that opts a file out of the sweep. Set ``False``
            to ingest skip-marked files anyway.
        skip_dirs: Directories to prune from the walk (the directory and its
            whole subtree). gitignore-style: an entry without a ``/`` matches a
            directory by **name** at any depth (``"node_modules"``); an entry
            with a ``/`` is an anchored **bundle-relative path**
            (``"vendor/repos"``). Use it to exclude cloned / vendored trees you
            don't own.
        with_body: Store each concept's markdown body as a ``body`` property.
            Left unset it takes the dialect's default: off for ``"okf"`` /
            ``"loose"`` (bodies are read on demand through ``source()``), on
            for ``"obsidian"``. Passing it explicitly wins either way.
        embed: Reserved for the opt-in embedder pass (body vectors for
            ``text_score``); not yet wired.

    Returns:
        A KnowledgeGraph of the bundle.

    Raises:
        RuntimeError: If the bundle path does not exist or is not a directory.
    """

def source(path: str) -> str:
    """Read a concept's markdown body on demand (frontmatter stripped).

    Pairs with partial ingestion: nodes store a ``file_path`` (relative to the
    bundle root), not the body. Join it with the root and pass it here to fetch
    the prose once a query has narrowed to a single concept. A file with no
    frontmatter returns its whole content.

    Args:
        path: Path to the concept's ``.md`` file.

    Returns:
        The markdown body, with any leading YAML frontmatter removed.

    Raises:
        RuntimeError: If the file cannot be read.
    """
