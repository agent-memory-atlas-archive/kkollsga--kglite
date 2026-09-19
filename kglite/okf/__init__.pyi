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
        dialect: ``"okf"`` (this function's default; :func:`validate` defaults
            to ``"obsidian"`` instead) for strict markdown links; ``"loose"`` to
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
              reserved ``parent:`` key emits the folder-note edge to each
              named parent — the ``folder_notes`` type and direction,
              ``CHILD_OF`` from the note to the parent unless the vault
              redeclares them.
            * **Tags**: inline ``#tags`` in the body join the same ``Tag`` hub
              as ``tags:`` (fenced code, inline code spans and URL or wikilink
              fragments are skipped), while the ``tags`` property keeps
              reporting only what the frontmatter said.
            * **Reserved filenames**: ``index.md`` and ``log.md`` are ordinary
              notes here, not folder metadata and not skipped.
            * **Folder notes**: ``X.md`` beside ``X/``, or ``X/X.md``, takes
              that directory's place — no ``Folder`` node is created for it and
              the notes inside are joined to the note by the ``folder_notes``
              edge, ``CHILD_OF`` child → parent by default.
            * **Hubs**: every hub node carries a ``title`` alongside its id.
            * **The vault's own declaration file**: ``.kglite/vault.yaml``,
              read by explicit path (the walk never enters a dot-directory).
              It carries ``kglite_vault: 1`` plus any of ``default_label``,
              ``label_from``, ``body``, ``skip_dirs``, ``folder_notes``,
              ``hubs``, ``heading_edges``, ``types``, ``indexes``,
              ``text_indexes``, ``ontology``, ``embed``, ``structure`` and
              ``edge_defaults``, and it wins over
              both the dialect's defaults and this function's keywords for
              what it declares — a rebuild re-reads it, so it is the vault's
              statement about itself. An unknown key, an unknown
              ``kglite_vault`` version or a value of the wrong shape raises
              rather than being ignored. ``embed:`` is *reported*, not run:
              core links no embedder. ``.kglite/skills/*.md`` and
              ``.kglite/recipes/*.md`` are imported into the graph's skill and
              recipe layers; a file that fails validation is skipped and its
              siblings still load. ``structure:`` derives nodes from each
              note's own body — its headings as ``Section`` nodes, the prose
              under them as ``Chunk`` nodes, its callouts, fenced blocks and
              ordered lists as nodes of their own — with ``inherit:`` and
              ``embed_text:`` decorating them and ``#``-anchored links
              retargeting onto them; see ``VAULT.md`` §7.1 for its keys.
              ``edge_defaults:`` pushes constant properties onto every edge of
              a named type (§7.2). See ``VAULT.md`` §7 and §8 for the schema.
        require_frontmatter: When ``True``, only ``.md`` files with a YAML
            frontmatter block are ingested — the discriminator between
            *structured* knowledge (OKF concepts, Claude memories) and plain
            markdown (READMEs, notes). Point at a parent of many projects to
            sweep out only the structured files across all of them. Set
            ``False`` to ingest every ``.md``. Left unset it takes the
            dialect's default: ``True`` for ``"okf"`` / ``"loose"``, ``False``
            for ``"obsidian"``. Under the OKF ladder node labels fall back
            ``type`` → ``metadata.type`` → ``Concept`` and titles ``title`` →
            ``name`` → the body's first heading → file stem, so Claude memories land as
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

    Returns:
        A KnowledgeGraph of the bundle.

    Raises:
        RuntimeError: If the bundle path does not exist or is not a directory.
    """

def fingerprint(
    path: str,
    *,
    dialect: str | None = ...,
    require_frontmatter: bool | None = ...,
    respect_skip: bool = ...,
    skip_dirs: list[str] | None = ...,
    with_body: bool | None = ...,
) -> int:
    """A stable 64-bit summary of what a build of this directory would read.

    Specified by ``VAULT.md`` §12. Every note, every attachment and — under
    ``"obsidian"`` — every build input under ``.kglite/`` contributes its
    ``(relative path, size, modification time)``, so the same directory read
    with the same keywords gives the same number in any process, on any
    machine. Editing, touching, renaming, adding or removing a file changes
    it; the order the filesystem lists the files does not.

    Only the files a build with these keywords would read count: a directory
    pruned by ``skip_dirs`` (or by the vault's own ``skip_dirs:``) is outside
    the summary, and ``.kglite/`` counts only under ``"obsidian"``, the one
    dialect that reads it. The files kglite writes into ``.kglite/`` itself
    are not inputs and are excluded — the vault's own ``graph.kgl`` cache
    with its lock and in-flight save siblings, and ``export-manifest.json``
    — so caching or exporting a vault does not mark it changed.

    Modification times are compared as **whole seconds**, because the number
    travels between copies and filesystems that do not agree below that. The
    failure this admits is narrow and worth naming: a file rewritten within
    the same second, to exactly the same length, reads as unchanged.

    The number is only meaningful beside the dialect it was taken with —
    ``.kglite/`` counts for ``"obsidian"`` alone, and the dialect decides
    which files are notes at all — and a path carries no stamp to read one
    from, so ``dialect`` here keeps :func:`build`'s ``"okf"`` default and
    must be passed for a vault. To compare against a *graph*, read the
    dialect it was built with off
    :attr:`~kglite.KnowledgeGraph.source_dialect`, or let
    :func:`rebuild_if_changed` do it.

    :func:`build` stamps this value on the graph it returns, where
    :attr:`~kglite.KnowledgeGraph.source_fingerprint` reports it and
    :func:`rebuild_if_changed` compares it.

    Args:
        path: Vault (or bundle) root directory.
        dialect: As :func:`build` — ``"okf"`` when omitted, which is **not**
            what an Obsidian vault fingerprints as.
        require_frontmatter: As :func:`build`.
        respect_skip: As :func:`build`.
        skip_dirs: As :func:`build`.
        with_body: As :func:`build`.

    Returns:
        The fingerprint, as an unsigned 64-bit integer.

    Raises:
        RuntimeError: If the path does not exist, is not a directory, or
            carries a ``.kglite/vault.yaml`` that does not parse.
    """

def rebuild_if_changed(
    graph: KnowledgeGraph,
    *,
    dialect: str | None = ...,
    embedder: Any | None = ...,
    require_frontmatter: bool | None = ...,
    respect_skip: bool = ...,
    skip_dirs: list[str] | None = ...,
    with_body: bool | None = ...,
) -> KnowledgeGraph | None:
    """Rebuild a graph from its own directory, if that directory has changed.

    Specified by ``VAULT.md`` §12. ``graph`` must carry the provenance
    :func:`build` stamps — its
    :attr:`~kglite.KnowledgeGraph.source_root`,
    :attr:`~kglite.KnowledgeGraph.source_fingerprint` and
    :attr:`~kglite.KnowledgeGraph.source_dialect`, all of which survive
    ``save()`` / ``load()``.

    Returns **None** when the directory still fingerprints as it did at build
    time: nothing beyond a ``stat`` pass is read, and the graph you passed is
    still current. Otherwise it returns a **new** graph — the object passed in
    is never modified — built from the same directory, with:

    * **the old graph's vectors carried across**, matched by
      ``(label, id)``: a note that kept both keeps its vector *and* its stored
      text hash, so an unchanged note is never re-embedded. A note that
      changed **label** — by moving between folders under a folder-derived
      label — is a different node and re-embeds. That is the documented
      contract, not a limitation to work around.
    * **a changed-mode embedding pass** for each ``embed:`` target the
      rebuilt ``.kglite/vault.yaml`` declares, when a model is available: the
      ``embedder`` argument, or the one already bound to ``graph`` with
      ``set_embedder()``. Only notes whose text no longer matches their
      carried hash are sent to the model. With no model the targets are
      reported as a warning and no vectors are computed; a target that fails
      mid-pass leaves a warning too rather than discarding a graph that is
      otherwise correct. Those warnings are not surfaced on the returned
      graph — run :func:`validate` for the report.

    **The dialect comes from the graph.** Omit ``dialect`` and the vault is
    rebuilt as it was built, because the stamp records that too; pass one
    that contradicts the stamp and the call is refused, naming both. That
    matters because the fingerprint is dialect-dependent: asked as an OKF
    bundle, an untouched Obsidian vault reads as changed *every* time and
    rebuilds into a near-empty graph that loads and looks valid. A ``.kgl``
    saved by 0.17.8–0.17.10 carries no dialect stamp; for those the keyword
    still decides (``"okf"`` when omitted) and the rebuild says the stamp was
    missing.

    The remaining keywords must be the ones the graph was built with: the
    stamp does not record them, so a rebuild with different ones is simply a
    different build.

    Args:
        graph: A graph built by :func:`build`.
        dialect: As :func:`build`. Omitted, it is the one
            :attr:`~kglite.KnowledgeGraph.source_dialect` reports.
        embedder: A model with ``dimension`` and ``embed()``, as
            :meth:`~kglite.KnowledgeGraph.set_embedder` takes. Omitted, the
            model bound to ``graph`` is used, and the returned graph carries it
            too.
        require_frontmatter: As :func:`build`.
        respect_skip: As :func:`build`.
        skip_dirs: As :func:`build`.
        with_body: As :func:`build`.

    Returns:
        A new :class:`~kglite.KnowledgeGraph`, or ``None`` when the directory
        is unchanged.

    Raises:
        RuntimeError: If the graph carries no ``source_root``; if ``dialect``
            contradicts the one the graph was built with; or if the directory
            no longer exists — a vault that is gone is an error, not a vault
            that is unchanged.

    Example::

        g = okf.build("vault", dialect="obsidian")
        ...
        fresh = okf.rebuild_if_changed(g)   # as it was built
        if fresh is not None:
            g = fresh
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
) -> VaultReport:
    """Check a vault and return the build report, without keeping the graph.

    Runs exactly the read :func:`build` runs — same walk, same parse, same
    resolution — and discards the graph, so what the report says is what a
    build does. It is the Python half of ``kglite okf check``, and it defaults
    to the same dialect that command does: ``"obsidian"``, the vault format
    this validates against. :func:`build` still defaults to ``"okf"``, so a
    bundle caller passes ``dialect="okf"`` here to check what it builds.

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
        dialect: As :func:`build`, except that this function defaults to
            ``"obsidian"`` — the vault rules it checks against, and what
            ``kglite okf check`` reads a directory with. Under ``"okf"`` /
            ``"loose"`` the report still describes what was built, in those
            dialects' terms.
        strict: Promote every warning to a failure — the setting a converter's
            own test suite should use. It changes ``VaultReport.ok`` only:
            errors and warnings stay classified as ``VAULT.md`` §9 classifies
            them.
        require_frontmatter: As :func:`build`.
        respect_skip: As :func:`build`.
        skip_dirs: As :func:`build`.
        with_body: As :func:`build`.

    Returns:
        A :class:`VaultReport`.

    Raises:
        RuntimeError: If the path does not exist or is not a directory.

    Example::

        report = okf.validate("vault", strict=True)
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
        ``skills_imported``, ``recipes_imported``, ``forced_splits`` (ints —
        ``forced_splits`` counts the chunk boundaries ``structure.chunks``'
        ``max_words`` / ``max_chars`` had to place *inside* one block, so a
        non-zero count means the vault holds lists, tables or paragraphs the
        caps cut without a blank line to cut at), and ``embed_targets``
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
    edge_tables: dict[str, str] | None = ...,
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

    **Declared edge tables.** A frontmatter list carries targets and nothing
    else, so an edge's properties are a documented loss. A type named in
    ``export: {edge_tables: {TYPE: "Heading"}}`` in the source vault's
    ``.kglite/vault.yaml`` — or in ``edge_tables`` here — is written as a GFM
    table under that heading in each source note's body instead, one column per
    property, and keeps them. This is the only prose an export ever adds. The
    table is read back only by a vault whose ``structure.tables`` declares the
    matching ``edges: true`` rule; no export writes ``vault.yaml``, so copy it
    across, and ``ExportReport.warnings`` says so when the rule is missing.

    Args:
        graph: The graph to write. It need not have come from a vault — a
            graph that carries no ``file_path`` anywhere exports every node.
        path: Target directory, created if it does not exist. An existing
            directory is written *into*.
        force: Replace files the manifest does not own or that were edited
            since the last export, and delete owned files that were edited.
        edge_tables: Edge type to the heading its edges are written under,
            merged over the source vault's own ``export.edge_tables:`` per
            type. How a graph that never was a vault declares one, and how to
            override a heading the vault named.
        source_root: The directory the graph's attachments were read from, so
            their bytes are copied into the exported vault. Omitted, it falls
            back to the graph's own :attr:`~kglite.KnowledgeGraph.source_root`
            — a graph built by :func:`build` knows where its pictures are, so
            an export of a vault-built graph copies them without being told.
            With neither, the body references are left as written and counted
            in ``ExportReport.attachments_unresolved``.

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
    def warnings(self) -> list[str]:
        """One line per declared edge table the export could not write as asked.

        A type whose source vault has no ``structure.tables`` rule to read the
        table back, one no exported note emits, or a ``vault.yaml`` that would
        not parse. Nothing here failed; each line is a table the author will
        not get back.
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
