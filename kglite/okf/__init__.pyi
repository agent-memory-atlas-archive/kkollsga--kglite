"""Type stubs for kglite.okf — Open Knowledge Format bundle ingestion."""

from __future__ import annotations

from typing import Any

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
            * **Attachments**: ``![alt](img/x.png)`` and ``![[x.png]]``
              resolve note-relative → vault-root-relative → by unique
              filename, and become one ``Image`` (PNG/JPEG/GIF/WebP) or
              ``Attachment`` node per file, keyed by its vault-relative
              ``path`` and carrying ``mime`` / ``size_bytes`` / ``mtime``
              from ``stat`` — the bytes are never read. The note reaches it
              by ``HAS_IMAGE`` / ``HAS_ATTACHMENT`` carrying ``alt``,
              ``section`` and ``ordinal``, an ``Image`` also carries a
              ``text`` of the alt texts and using-note titles, and a
              reference that matches no file becomes a
              ``missing: true`` stub.
            * **Path safety**: a markdown-style target (``[text](x)`` and
              ``![alt](x)``) is percent-decoded before it is resolved, so
              ``img/a%20b.png`` reaches the file named ``a b.png``; a wikilink
              is a name and is taken literally. A body reference naming an
              absolute filesystem path (``C:/…``, ``~/…``, ``file:…``, a UNC
              path), or climbing above the vault root with ``../``, is a
              reported **error** — a leading ``/`` is vault-root-relative and
              is not one.
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
            * **Reserved filenames**: ``index.md`` and ``log.md`` are ordinary
              notes here, not folder metadata and not skipped.
            * **Folder notes**: ``X.md`` beside ``X/``, or ``X/X.md``, takes
              that directory's place — no ``Folder`` node is created for it and
              the notes inside are joined to the note by ``CHILD_OF``.
            * **Hubs**: every hub node carries a ``title`` alongside its id.
            * **The vault's own declaration file**: ``.kglite/vault.yaml``,
              read by explicit path (the walk never enters a dot-directory).
              It carries ``kglite_vault: 1`` plus any of ``default_label``,
              ``label_from``, ``body``, ``skip_dirs``, ``folder_notes``,
              ``hubs``, ``heading_edges``, ``types``, ``indexes``,
              ``text_indexes``, ``ontology`` and ``embed``, and it wins over
              both the dialect's defaults and this function's keywords for
              what it declares — a rebuild re-reads it, so it is the vault's
              statement about itself. An unknown key, an unknown
              ``kglite_vault`` version or a value of the wrong shape raises
              rather than being ignored. ``embed:`` is *reported*, not run:
              core links no embedder. ``.kglite/skills/*.md`` and
              ``.kglite/recipes/*.md`` are imported into the graph's skill and
              recipe layers; a file that fails validation is skipped and its
              siblings still load. See ``VAULT.md`` §7 and §8 for the schema.
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

def validate(
    path: str,
    *,
    dialect: str | None = ...,
    strict: bool = ...,
    require_frontmatter: bool | None = ...,
    respect_skip: bool = ...,
    skip_dirs: list[str] | None = ...,
    with_body: bool | None = ...,
    embed: bool = ...,
) -> VaultReport:
    """Check a vault and return the build report, without keeping the graph.

    Runs exactly the read :func:`build` runs — same walk, same parse, same
    resolution — and discards the graph, so what the report says is what a
    build does. It is the Python half of ``kglite okf check``.

    Findings are classified by ``VAULT.md`` §9. **Errors** mean the vault does
    not meet the spec: unparseable frontmatter, a misused reserved key, id
    collisions, two folder notes for one directory, a `.kglite/vault.yaml`
    schema error, an ontology the declaration API refuses, and a link or
    attachment reference naming an absolute filesystem path or climbing above
    the vault root. **Warnings** are legitimate in a real vault but worth
    seeing: dangling links, missing or ambiguous attachments,
    case-insensitive collisions, alias clashes, a declared hub key spent on
    the typed-edge rule, a value that does not match its declared type, a
    declaration naming a label or property the vault does not carry, a carried
    skill or recipe that failed validation, and a `vault.yaml` found under a
    dialect that does not read one.

    A `.kglite/vault.yaml` that does not parse makes :func:`build` raise; here
    it is the report's single error instead, so a broken vault and a faulty
    one are read the same way. The only failure that raises is a root that
    cannot be read at all.

    Args:
        path: Vault (or bundle) root directory.
        dialect: As :func:`build`. The vault rules this checks against are the
            ``"obsidian"`` ones; under ``"okf"`` / ``"loose"`` the report still
            describes what was built, in those dialects' terms.
        strict: Promote every warning to a failure — the setting a converter's
            own test suite should use. It changes ``VaultReport.ok`` only:
            errors and warnings stay classified as ``VAULT.md`` §9 classifies
            them.
        require_frontmatter: As :func:`build`.
        respect_skip: As :func:`build`.
        skip_dirs: As :func:`build`.
        with_body: As :func:`build`.
        embed: As :func:`build`.

    Returns:
        A :class:`VaultReport`.

    Raises:
        RuntimeError: If the path does not exist or is not a directory.

    Example::

        report = okf.validate("vault", dialect="obsidian", strict=True)
        if not report.ok:
            print(report)
            raise SystemExit(1)
    """

class VaultReport:
    """What a build saw: counts, errors, warnings, and one verdict.

    Returned by :func:`validate`. Immutable — it describes a build that already
    happened. ``str(report)`` renders the same text ``kglite okf check``
    prints: the counts, then the errors, then the warnings.
    """

    @property
    def errors(self) -> list[str]:
        """Findings that fail the spec, in the order the build found them."""

    @property
    def warnings(self) -> list[str]:
        """Findings that leave a usable graph, in the order the build found them."""

    @property
    def counts(self) -> dict[str, Any]:
        """What the build counted.

        Keys: ``files_scanned`` and ``concepts`` (ints — ``concepts`` is lower
        when ``require_frontmatter`` or ``kg_skip`` excluded files),
        ``nodes_by_label`` and ``edges_by_type`` (``dict[str, int]``, including
        synthesized ``Folder`` / hub / ``Image`` nodes and ``_provisional``
        stubs), ``dangling``, ``folder_notes``, ``missing_attachments``,
        ``ambiguous_attachments``, ``indexes_declared``, ``text_indexes_built``,
        ``skills_imported``, ``recipes_imported`` (ints), and ``embed_targets``
        — the ``(label, property)`` pairs ``.kglite/vault.yaml`` declared, in
        declaration order. The engine computes no vectors: run
        :meth:`~kglite.KnowledgeGraph.embed_texts` for each pair once an
        embedder is bound.
        """

    @property
    def ok(self) -> bool:
        """Whether the vault passed: no errors, and no warnings under ``strict``."""

    def __str__(self) -> str:
        """The report as text — counts, then errors, then warnings."""

def export(
    graph: KnowledgeGraph,
    path: str,
    *,
    force: bool = ...,
    source_root: str | None = ...,
) -> ExportReport:
    """Write a graph out as an Obsidian vault — the inverse of :func:`build`.

    Specified by ``VAULT.md`` §10. Each node becomes one ``.md`` file under a
    folder named for its label, so the label ladder recovers the label without
    a ``type:`` key; its properties become sorted frontmatter, its ``body``
    the prose below, and its outgoing edges wikilink-valued keys named
    ``lower_snake(TYPE)``. Nodes the build synthesized — ``Folder``, ``Tag``,
    ``Source``, ``Image``, ``Attachment``, hub nodes and ``_provisional``
    stubs — are not files; the next import makes them again. Graph-carried
    skills and recipes are written to ``.kglite/skills/`` and
    ``.kglite/recipes/``.

    **Nothing this export did not write is ever replaced.**
    ``.kglite/export-manifest.json`` records a SHA-256 per exported file. A
    file missing from it, or one whose bytes have moved since (a human edited
    it), is refused and named in ``ExportReport.refusals`` rather than
    overwritten; only a manifest-owned file whose bytes still match is
    replaced, or deleted when its node is gone. ``force`` lifts the two
    refusals and nothing else.

    Exporting the same graph twice is byte-identical: keys, edge lists, file
    order and the manifest are all sorted.

    Args:
        graph: The graph to write. It need not have come from a vault — a
            graph that carries no ``file_path`` anywhere exports every node.
        path: Target directory, created if it does not exist. An existing
            directory is written *into*.
        force: Replace files the manifest does not own or that were edited
            since the last export, and delete owned files that were edited.
        source_root: The directory the graph's attachments were read from, so
            their bytes are copied into the exported vault. Without it the
            body references are left as written and counted in
            ``ExportReport.attachments_unresolved``.

    Returns:
        An :class:`ExportReport`.

    Raises:
        RuntimeError: If ``path`` exists and is not a directory, if the
            existing manifest cannot be read or names an unknown version, or
            if a write fails.

    Example::

        report = okf.export(g, "out/vault", source_root="vault")
        print(report)
        assert report.ok, report.refusals
    """

class ExportReport:
    """What an export wrote, left alone, deleted and refused.

    Returned by :func:`export`. Immutable — it describes an export that already
    happened. ``str(report)`` renders the same text ``kglite okf export``
    prints.
    """

    @property
    def files_written(self) -> int:
        """Files created or replaced."""

    @property
    def files_unchanged(self) -> int:
        """Files already byte-identical to what the export would write.

        These are not rewritten, so their modification times do not move.
        """

    @property
    def files_deleted(self) -> int:
        """Manifest-owned files whose node is gone from the graph."""

    @property
    def files_refused(self) -> int:
        """Writes and deletions declined for safety; see :attr:`refusals`."""

    @property
    def refusals(self) -> list[str]:
        """One line per refusal — the path and why — in path order."""

    @property
    def edge_properties_dropped(self) -> int:
        """Edge properties lost: frontmatter lists carry targets, not properties.

        ``section``, ``anchor``, ``alt`` and ``ordinal`` are the documented
        loss of the format (``VAULT.md`` §10.9).
        """

    @property
    def attachments_copied(self) -> int:
        """Attachment files copied in from ``source_root``."""

    @property
    def attachments_unresolved(self) -> int:
        """Attachment nodes whose bytes could not be copied.

        Either no ``source_root`` was given, or the file is not under it. The
        body references to them are written as they stand.
        """

    @property
    def skills_written(self) -> int:
        """Files written under ``.kglite/skills/``."""

    @property
    def recipes_written(self) -> int:
        """Files written under ``.kglite/recipes/``."""

    @property
    def ok(self) -> bool:
        """Whether the export wrote everything it wanted to — no refusals."""

    def __str__(self) -> str:
        """The report as text — the counts, then the refusals."""
