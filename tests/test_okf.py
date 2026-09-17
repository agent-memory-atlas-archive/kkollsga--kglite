"""OKF (Open Knowledge Format) ingestion tests.

Tier 1 — golden synthetic fixtures (deterministic regression backbone). The
committed bundles under ``tests/fixtures/okf/golden/`` exercise every parse and
build path: labelled concepts, the edge-type ladder, dangling → provisional
stubs, an orphan, nested-frontmatter flattening, a no-frontmatter degrade, the
loose/obsidian wikilink dialect, and reserved-file handling.

(Tier 2 — real-corpus integration against Google's OKF bundles — lives in
``test_okf_corpus.py``.)
"""

from __future__ import annotations

from collections import Counter
from datetime import datetime
from pathlib import Path

import kglite
from kglite import okf

FIXTURES = Path(__file__).parent / "fixtures" / "okf" / "golden"
OKF_BUNDLE = FIXTURES / "okf"
OBSIDIAN_BUNDLE = FIXTURES / "obsidian"
VAULT_BUNDLE = FIXTURES / "vault"


def _labels(g) -> Counter:
    rows = g.cypher("MATCH (n) RETURN labels(n)[0] AS l").to_list()
    return Counter(r["l"] for r in rows)


def _edge_types(g) -> Counter:
    rows = g.cypher("MATCH ()-[r]->() RETURN type(r) AS t").to_list()
    return Counter(r["t"] for r in rows)


class TestOkfGoldenBundle:
    """Strict OKF dialect over the committed golden bundle."""

    def test_node_count_and_labels(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        # 8 concepts (plain.md has no frontmatter → skipped by default) +
        # 1 `tables/ghost` stub + 2 Tag (sales, orders) + 1 Source +
        # 6 Folder (tables, datasets, references, playbooks, meta, guide) = 18.
        assert g.cypher("MATCH (n) RETURN count(n) AS c").to_list()[0]["c"] == 18
        labels = _labels(g)
        assert labels["Folder"] == 6
        assert labels["BigQuery Table"] == 2
        assert labels["BigQuery Dataset"] == 1
        assert labels["Reference"] == 1
        assert labels["Playbook"] == 1
        # profile.md has no top-level `type` → label falls back to metadata.type.
        assert labels["user"] == 1
        assert labels["Guide"] == 1
        assert labels["Section"] == 1
        # only the ghost stub is a bare Concept now (plain.md was skipped).
        assert labels["Concept"] == 1
        # synthesized nodes
        assert labels["Tag"] == 2
        assert labels["Source"] == 1

    def test_concept_id_and_title(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        rows = g.cypher("MATCH (n {concept_id:'tables/orders'}) RETURN n.title AS title, n.file_path AS fp").to_list()
        assert rows == [{"title": "Orders", "fp": "tables/orders.md"}]
        # plain.md (no frontmatter) is skipped by default (require_frontmatter).
        plain = g.cypher("MATCH (n {concept_id:'plain'}) RETURN count(n) AS c").to_list()
        assert plain[0]["c"] == 0

    def test_label_and_title_fallback(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        # profile.md: no top-level `type`/`title` → label from metadata.type,
        # title from `name` (the Claude-memory shape).
        rows = g.cypher("MATCH (n {concept_id:'meta/profile'}) RETURN labels(n)[0] AS l, n.title AS t").to_list()
        assert rows == [{"l": "user", "t": "User Profile"}]

    def test_require_frontmatter_false_includes_plain(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False, require_frontmatter=False)
        plain = g.cypher("MATCH (n {concept_id:'plain'}) RETURN labels(n)[0] AS l").to_list()
        assert plain == [{"l": "Concept"}]

    def test_frontmatter_mapping(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        rows = g.cypher("MATCH (n {concept_id:'tables/orders'}) RETURN n.tags AS tags, n.timestamp AS ts").to_list()
        # `tags` list → JSON string; ISO timestamp stays a string.
        assert rows[0]["tags"] == '["sales","orders"]'
        assert rows[0]["ts"] == "2026-05-28T14:30:00Z"
        # nested `metadata:` flattens to dotted keys.
        meta = g.cypher(
            "MATCH (n {concept_id:'meta/profile'}) RETURN n.`metadata.type` AS mt, n.`metadata.scope` AS ms"
        ).to_list()
        assert meta == [{"mt": "user", "ms": "project"}]

    def test_edge_type_ladder(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        et = _edge_types(g)
        assert et["JOINS_WITH"] == 1  # "# Joins" section
        assert et["PART_OF"] == 1  # explicit link title
        assert et["CITES"] == 2  # "# Citations": internal note + external Source
        assert et["LINKS_TO"] == 1  # untyped (the dangling ghost link)
        assert et["CONTAINS"] == 7  # folder → concept across the 6 dirs
        assert et["TAGGED"] == 3  # orders→{sales,orders}, customers→sales

        # spot-check endpoints of the typed edges
        joins = g.cypher("MATCH (a)-[:JOINS_WITH]->(b) RETURN a.concept_id AS a, b.concept_id AS b").to_list()
        assert joins == [{"a": "tables/orders", "b": "tables/customers"}]
        contains = g.cypher("MATCH (f:Folder)-[:CONTAINS]->(c) RETURN f.id AS f, c.concept_id AS c").to_list()
        pairs = {(r["f"], r["c"]) for r in contains}
        assert ("tables", "tables/orders") in pairs
        assert ("guide", "guide/intro") in pairs

    def test_tag_nodes_connect_concepts(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        # the shared `sales` tag links both tables through a Tag hub (the
        # densification that makes clustering meaningful).
        tagged = g.cypher("MATCH (a)-[:TAGGED]->(:Tag {id:'sales'}) RETURN a.concept_id AS a").to_list()
        assert {r["a"] for r in tagged} == {"tables/orders", "tables/customers"}

    def test_external_citation_becomes_source(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        # the external citation URL became a Source node with a CITES edge.
        src = g.cypher("MATCH (a {concept_id:'tables/orders'})-[:CITES]->(s:Source) RETURN s.id AS url").to_list()
        assert any("cloud.google.com" in r["url"] for r in src)

    def test_folder_nodes_and_index_enrichment(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        # the tables/ directory is a Folder containing its concepts...
        contained = g.cypher("MATCH (:Folder {id:'tables'})-[:CONTAINS]->(c) RETURN c.concept_id AS c").to_list()
        assert {r["c"] for r in contained} == {"tables/orders", "tables/customers"}
        # ...and its title comes from tables/index.md (reserved file recovered).
        title = g.cypher("MATCH (f:Folder {id:'tables'}) RETURN f.title AS t").to_list()
        assert title == [{"t": "All Tables"}]

    def test_dangling_link_becomes_provisional_stub(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        stubs = g.cypher("MATCH (n {_provisional:true}) RETURN n.concept_id AS id").to_list()
        assert stubs == [{"id": "tables/ghost"}]

    def test_orphan_detectable(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        # With Folder nodes every concept has a structural CONTAINS edge, so a
        # meaningful "orphan" is one with no *semantic* edge (exclude the
        # structural CONTAINS/TAGGED). The playbook is deliberately unlinked.
        deg = g.cypher(
            "MATCH (n {concept_id:'playbooks/incident'}) "
            "OPTIONAL MATCH (n)-[r]-(m) WHERE NOT type(r) IN ['CONTAINS', 'TAGGED'] "
            "RETURN count(r) AS d"
        ).to_list()
        assert deg[0]["d"] == 0

    def test_reserved_index_not_a_node(self):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        # index.md must not appear as a concept.
        assert g.cypher("MATCH (n {concept_id:'index'}) RETURN count(n) AS c").to_list()[0]["c"] == 0

    def test_build_is_deterministic(self):
        a = okf.build(str(OKF_BUNDLE), respect_skip=False)
        b = okf.build(str(OKF_BUNDLE), respect_skip=False)
        for q in (
            "MATCH (n) RETURN count(n) AS c",
            "MATCH ()-[r]->() RETURN count(r) AS c",
        ):
            assert a.cypher(q).to_list() == b.cypher(q).to_list()

    def test_save_load_roundtrip(self, tmp_path):
        g = okf.build(str(OKF_BUNDLE), respect_skip=False)
        before_n = g.cypher("MATCH (n) RETURN count(n) AS c").to_list()[0]["c"]
        before_e = g.cypher("MATCH ()-[r]->() RETURN count(r) AS c").to_list()[0]["c"]
        path = str(tmp_path / "okf.kgl")
        g.save(path)
        h = kglite.load(path)
        assert h.cypher("MATCH (n) RETURN count(n) AS c").to_list()[0]["c"] == before_n
        assert h.cypher("MATCH ()-[r]->() RETURN count(r) AS c").to_list()[0]["c"] == before_e
        # a property survives the round-trip
        assert (
            h.cypher("MATCH (n {concept_id:'tables/orders'}) RETURN n.tags AS t").to_list()[0]["t"]
            == '["sales","orders"]'
        )


class TestOkfObsidianDialect:
    """Obsidian vault dialect over a bundle with no folders: root notes only."""

    def test_wikilinks_and_degrade(self):
        g = okf.build(str(OBSIDIAN_BUNDLE), respect_skip=False, dialect="obsidian")
        # alice + bob + MEMORY + carol-missing stub = 4. A vault does not
        # require frontmatter, so MEMORY.md is an ordinary note.
        assert g.cypher("MATCH (n) RETURN count(n) AS c").to_list()[0]["c"] == 4
        # All three are root-level with no `type:`, so the label ladder falls
        # through to `Note`; `metadata.type` is not a vault rung. The dangling
        # stub keeps the `Concept` label every dialect gives a stub.
        assert _labels(g) == Counter({"Note": 3, "Concept": 1})
        assert (
            g.cypher("MATCH (n {concept_id:'alice'}) RETURN n.`metadata.type` AS mt").to_list()[0]["mt"] == "person"
        ), "metadata.type survives as an ordinary property"

    def test_wikilink_resolution_and_dangling(self):
        g = okf.build(str(OBSIDIAN_BUNDLE), respect_skip=False, dialect="obsidian")
        edges = g.cypher("MATCH (a)-[r]->(b) RETURN a.concept_id AS a, b.concept_id AS b ORDER BY b").to_list()
        assert {"a": "alice", "b": "bob"} in edges
        assert {"a": "alice", "b": "carol-missing"} in edges
        stubs = g.cypher("MATCH (n {_provisional:true}) RETURN n.concept_id AS id").to_list()
        assert stubs == [{"id": "carol-missing"}]

    def test_wikilinks_ignored_in_strict_dialect(self):
        # In the default (okf) dialect, [[wikilinks]] are not links → no edges.
        g = okf.build(str(OBSIDIAN_BUNDLE), respect_skip=False)
        assert g.cypher("MATCH ()-[r]->() RETURN count(r) AS c").to_list()[0]["c"] == 0


class TestVaultGoldenBundle:
    """The Obsidian vault dialect over the committed ``golden/vault`` bundle.

    Thirteen notes across three top-level folders plus two root notes,
    carrying a ``type:`` override, an ``id:`` override, a stem-collision pair, a
    case-collision pair, a native list and an ISO date — plus the link
    semantics of VAULT.md §5: ``aliases:``, a ``#section`` anchor, wikilink-
    valued frontmatter keys (``depends_on:`` and the reserved ``parent:``),
    inline ``#tags``, an ``![[embed]]`` and one dangling link; VAULT.md §6:
    an ``img/`` folder whose two PNGs and one PDF are reached by all three
    rungs of the resolution ladder, plus one reference to a file that is not
    there; and VAULT.md §2.3–2.4: ``projects.md`` is the folder note for
    ``projects/`` and ``notes/index.md`` is an ordinary note. VAULT.md §7-§8:
    the bundle carries a ``.kglite/vault.yaml`` declaring ``default_label``, a
    case-folding ``keywords`` hub, a ``heading_edges`` entry, a ``types``
    coercion, two indexes, a text index and an ``embed`` target, plus one
    ``.kglite/skills/`` and one ``.kglite/recipes/`` file — so every label and
    edge below is the *declared* vault's. The build *report* for the same
    bundle (the collision findings, the dangling warning, the declaration
    counters, the embed targets) is asserted in Rust, at
    ``okf::build::tests::golden_vault_bundle_report`` — the report has no
    Python surface yet.
    """

    def build(self):
        return okf.build(str(VAULT_BUNDLE), dialect="obsidian")

    def test_label_ladder(self):
        # `type:` → `default_label` → top-level folder → `Note`. The vault
        # declares `default_label: Article`, which sits ahead of the folder
        # rung, so every note without a `type:` is an Article.
        assert _labels(self.build()) == Counter(
            {
                "Article": 11,
                "Folder": 3,  # `projects/` has a folder note, so it has no Folder node
                "Tag": 3,
                "Initiative": 2,
                # `faults` and `horizons`, folded from five spellings by the
                # declared case-insensitive hub
                "Keyword": 2,
                "Concept": 1,  # the `[[Missing]]` stub, labelled as every dialect labels one
                # img/diagram.png and img/faults.png
                "Image": 2,
                # img/handbook.pdf, plus the absent img/appendix.pdf stub —
                # the extension labels a file that is not there too
                "Attachment": 2,
                # `.kglite/skills/one.md` and `.kglite/recipes/one.md` — system
                # labels, but ordinary nodes to Cypher (VAULT.md §8)
                "KgliteSkill": 1,
                "KgliteRecipe": 1,
            }
        )

    def test_edge_types(self):
        assert _edge_types(self.build()) == Counter(
            {
                "CONTAINS": 8,
                "LINKS_TO": 6,
                # four notes under the `projects` folder note, plus the reserved
                # `parent:` key on seismic.md
                "CHILD_OF": 5,
                "TAGGED": 4,
                "DEPENDS_ON": 2,  # `depends_on:` names two wikilinks
                "EMBEDS": 1,  # `![[old]]`
                # `## Related topics`, retyped from the ladder's `RELATED` by
                # the vault's `heading_edges`
                "RELATED_TO": 1,
                "HAS_KEYWORD": 4,  # two notes x two folded keywords
                # links.md reaches both images; seismic.md re-reaches faults.png
                # from another folder by its bare filename
                "HAS_IMAGE": 3,
                # index.md → handbook.pdf, links.md → the absent appendix
                "HAS_ATTACHMENT": 2,
            }
        )

    def test_ids_are_stems_declared_ids_and_collision_fallbacks(self):
        g = self.build()
        ids = sorted(
            r["id"] for r in g.cypher("MATCH (n) WHERE n.concept_id IS NOT NULL RETURN n.concept_id AS id").to_list()
        )
        assert ids == [
            "Missing",  # the dangling `[[Missing]]` stub keeps its raw name
            "Roadmap",  # case-collision pair: ids are left alone
            "atlas",
            "index",  # `index.md` is an ordinary note in a vault
            "links",
            "mtg-2026-01",  # declared `id:` wins over the stem `meeting`
            "nested",  # a stem, three folders deep
            "notes/alpha",  # stem collision → path-relative fallback
            "old",
            "projects",  # the folder note for `projects/`
            "projects/alpha",
            "roadmap",
            "seismic",
            "welcome",
        ]

    def test_declared_id_is_not_also_a_property(self):
        g = self.build()
        rows = g.cypher("MATCH (n {concept_id:'mtg-2026-01'}) RETURN n.title AS t, n.file_path AS f").to_list()
        assert rows == [{"t": "Kickoff", "f": "notes/meeting.md"}]

    def test_title_falls_back_to_the_first_h1(self):
        g = self.build()
        rows = g.cypher("MATCH (n {concept_id:'welcome'}) RETURN n.title AS t").to_list()
        assert rows == [{"t": "Welcome"}]

    def test_body_is_stored_by_default(self):
        g = self.build()
        body = g.cypher("MATCH (n {concept_id:'welcome'}) RETURN n.body AS b").to_list()[0]["b"]
        assert body.startswith("# Welcome")
        # …and the explicit option still turns it off.
        off = okf.build(str(VAULT_BUNDLE), dialect="obsidian", with_body=False)
        assert off.cypher("MATCH (n {concept_id:'welcome'}) RETURN n.body AS b").to_list() == [{"b": None}]

    def test_require_frontmatter_defaults_off(self):
        # welcome.md has no frontmatter at all and is still a node.
        assert self.build().cypher("MATCH (n {concept_id:'welcome'}) RETURN count(n) AS c").to_list()[0]["c"] == 1
        on = okf.build(str(VAULT_BUNDLE), dialect="obsidian", require_frontmatter=True)
        assert on.cypher("MATCH (n {concept_id:'welcome'}) RETURN count(n) AS c").to_list()[0]["c"] == 0

    def test_lists_stay_native(self):
        g = self.build()
        rows = g.cypher("MATCH (n {concept_id:'atlas'}) RETURN n.keywords AS k, n.tags AS t").to_list()
        assert rows == [{"k": ["faults", "horizons"], "t": ["seismic"]}]
        # The okf dialect still JSON-encodes them.
        j = okf.build(str(VAULT_BUNDLE), require_frontmatter=False)
        assert j.cypher("MATCH (n {concept_id:'projects/atlas'}) RETURN n.keywords AS k").to_list() == [
            {"k": '["faults","horizons"]'}
        ]

    def test_iso_strings_become_temporal_values(self):
        g = self.build()
        rows = g.cypher(
            "MATCH (n {concept_id:'atlas'}) "
            "RETURN n.updated AS u, n.reviewed AS r, "
            "n.updated + duration({days: 1}) AS plus, n.updated < date('2026-02-01') AS lt"
        ).to_list()
        # A date renders as its ISO string, but it is a date: arithmetic and
        # ordering against date() both work, which a string could not do.
        assert rows[0]["u"] == "2026-01-15"
        assert rows[0]["plus"] == "2026-01-16"
        assert rows[0]["lt"] is True
        assert rows[0]["r"] == datetime(2026, 1, 15, 9, 30)

    def test_folders_come_from_the_path_not_the_stem_id(self):
        g = self.build()
        rows = g.cypher("MATCH (f:Folder)-[:CONTAINS]->(c) RETURN f.id AS f, c.concept_id AS c, c.id AS fid").to_list()
        pairs = {(r["f"], r["c"] if r["c"] is not None else r["fid"]) for r in rows}
        # `projects/` has a folder note, so no Folder CONTAINS its notes.
        assert pairs == {
            ("notes", "links"),
            ("notes", "index"),
            ("notes", "notes/alpha"),
            ("notes", "roadmap"),
            ("notes", "mtg-2026-01"),
            ("notes", "notes/deep"),
            ("notes/deep", "nested"),
            ("archive", "old"),
        }

    def test_links_resolve_through_the_ladder(self):
        g = self.build()
        edges = sorted(
            (r["a"], r["b"])
            for r in g.cypher("MATCH (a)-[:LINKS_TO]->(b) RETURN a.concept_id AS a, b.concept_id AS b").to_list()
        )
        assert edges == [
            ("links", "Roadmap"),  # an exact id
            ("links", "atlas"),  # `[[atlas#Overview]]` — the anchor never resolves
            ("links", "seismic"),  # `[[Seismic interpretation]]` — an alias
            ("nested", "atlas"),
            ("welcome", "atlas"),
            ("welcome", "old"),  # a path link, relative to the linking note
        ]

    def test_only_the_named_dangling_link_is_a_link_stub(self):
        g = self.build()
        stubs = g.cypher("MATCH (n {_provisional:true}) WHERE n.missing IS NULL RETURN n.concept_id AS id").to_list()
        # `![[diagram.png]]` resolves to a real `Image`; the one absent
        # attachment is a stub of its own kind (`missing: true`, §6.6), which
        # is what this filter excludes.
        assert stubs == [{"id": "Missing"}]

    def test_body_link_edges_carry_section_and_anchor(self):
        g = self.build()
        rows = g.cypher(
            "MATCH (a)-[r:LINKS_TO]->(b) WHERE a.concept_id = 'links' "
            "RETURN b.concept_id AS b, r.section AS section, r.anchor AS anchor ORDER BY b"
        ).to_list()
        assert rows == [
            {"b": "Roadmap", "section": None, "anchor": None},  # above the first heading
            {"b": "atlas", "section": "Deep dive", "anchor": "Overview"},
            {"b": "seismic", "section": "Deep dive", "anchor": None},
        ]

    def test_embed_of_a_note_is_an_edge(self):
        g = self.build()
        rows = g.cypher("MATCH (a)-[:EMBEDS]->(b) RETURN a.concept_id AS a, b.concept_id AS b").to_list()
        assert rows == [{"a": "links", "b": "old"}]

    def test_wikilink_valued_frontmatter_keys_are_edges_not_properties(self):
        g = self.build()
        deps = sorted(
            r["b"]
            for r in g.cypher("MATCH (a {concept_id:'seismic'})-[:DEPENDS_ON]->(b) RETURN b.concept_id AS b").to_list()
        )
        assert deps == ["Missing", "atlas"]
        # `parent:` emits the folder note's edge type and direction, not
        # `PARENT` — alongside the one the folder layout gives the same note.
        parents = sorted(
            r["b"]
            for r in g.cypher("MATCH (a {concept_id:'seismic'})-[:CHILD_OF]->(b) RETURN b.concept_id AS b").to_list()
        )
        assert parents == ["atlas", "projects"]
        rows = g.cypher(
            "MATCH (n {concept_id:'seismic'}) RETURN n.depends_on AS d, n.parent AS p, n.reviewers AS r, n.aliases AS a"
        ).to_list()
        assert rows == [
            {
                "d": None,
                "p": None,
                # a list mixing wikilinks with plain strings is never split
                "r": ["[[atlas]]", "ada"],
                "a": ["Seismic interpretation", "seismics"],
            }
        ]

    def test_inline_tags_join_the_frontmatter_hub(self):
        g = self.build()
        tags = sorted(r["t"] for r in g.cypher("MATCH (t:Tag) RETURN t.id AS t").to_list())
        assert tags == ["field-work", "geoscience", "seismic"], "`#incode` / fenced tags do not count"
        tagged = sorted(
            (r["a"], r["t"])
            for r in g.cypher("MATCH (a)-[:TAGGED]->(t:Tag) RETURN a.concept_id AS a, t.id AS t").to_list()
        )
        assert tagged == [
            ("atlas", "seismic"),
            ("seismic", "field-work"),
            ("seismic", "geoscience"),
            ("seismic", "seismic"),
        ]
        # …and the `tags` property still reports only what the frontmatter said.
        assert g.cypher("MATCH (n {concept_id:'seismic'}) RETURN n.tags AS t").to_list() == [{"t": ["seismic"]}]

    def test_the_folder_note_took_the_folders_place(self):
        g = self.build()
        # No `Folder` node for `projects/` at all …
        assert g.cypher("MATCH (f:Folder) RETURN f.id AS f ORDER BY f").to_list() == [
            {"f": "archive"},
            {"f": "notes"},
            {"f": "notes/deep"},
        ]
        # … and its notes hang off the note instead, by the declared edge.
        children = sorted(
            r["c"]
            for r in g.cypher("MATCH (c)-[:CHILD_OF]->(p {concept_id:'projects'}) RETURN c.concept_id AS c").to_list()
        )
        assert children == ["Roadmap", "atlas", "projects/alpha", "seismic"]
        # The folder note itself is at the root, so no Folder contains it.
        assert (
            g.cypher("MATCH (:Folder)-[:CONTAINS]->(n {concept_id:'projects'}) RETURN count(*) AS c").to_list()[0]["c"]
            == 0
        )

    def test_index_md_is_an_ordinary_note_in_a_vault(self):
        g = self.build()
        rows = g.cypher("MATCH (n {concept_id:'index'}) RETURN labels(n)[0] AS l, n.title AS t").to_list()
        assert rows == [{"l": "Article", "t": "Notes index"}]
        # The `okf` dialect still diverts it to the folder's metadata.
        j = okf.build(str(VAULT_BUNDLE), require_frontmatter=False)
        assert j.cypher("MATCH (n {concept_id:'notes/index'}) RETURN count(n) AS c").to_list()[0]["c"] == 0

    def test_hub_nodes_carry_a_title(self):
        g = self.build()
        rows = g.cypher("MATCH (t:Tag) RETURN t.id AS id, t.title AS title ORDER BY id").to_list()
        # The built-in tag hub is case-sensitive, so each title is its id.
        assert rows == [
            {"id": "field-work", "title": "field-work"},
            {"id": "geoscience", "title": "geoscience"},
            {"id": "seismic", "title": "seismic"},
        ]

    def test_attachment_nodes_carry_their_stat_metadata(self):
        import datetime

        g = self.build()
        rows = g.cypher(
            "MATCH (n:Image) RETURN n.path AS path, n.title AS title, n.mime AS mime, "
            "n.size_bytes AS size, n.mtime AS mtime ORDER BY path"
        ).to_list()
        assert [r["path"] for r in rows] == ["img/diagram.png", "img/faults.png"]
        assert [r["title"] for r in rows] == ["diagram.png", "faults.png"]
        assert {r["mime"] for r in rows} == {"image/png"}
        # The committed PNGs are 69 bytes each; the point is that `stat` was
        # read and the bytes were not.
        assert [r["size"] for r in rows] == [69, 69]
        assert all(isinstance(r["mtime"], datetime.datetime) for r in rows)
        assert g.cypher("MATCH (n:Attachment {path:'img/handbook.pdf'}) RETURN n.mime AS m").to_list() == [
            {"m": "application/pdf"}
        ]

    def test_image_text_carries_alts_and_using_note_titles(self):
        # VAULT.md §6.3: captions stay text-searchable — an edge property is
        # not. `faults.png` is used by two notes, one alt text between them.
        g = self.build()
        assert g.cypher("MATCH (n:Image {path:'img/faults.png'}) RETURN n.text AS t").to_list() == [
            {"t": "Fault map\nLink semantics\nseismic"}
        ]

    def test_attachment_edges_carry_alt_section_and_ordinal(self):
        g = self.build()
        rows = g.cypher(
            "MATCH (a)-[r:HAS_IMAGE]->(b) RETURN a.concept_id AS src, b.path AS tgt, "
            "r.alt AS alt, r.section AS section, r.ordinal AS ordinal ORDER BY src, ordinal"
        ).to_list()
        assert rows == [
            {"src": "links", "tgt": "img/faults.png", "alt": "Fault map", "section": "Figures", "ordinal": 0},
            # the `![[diagram.png]]` spelling carries no alt at all
            {"src": "links", "tgt": "img/diagram.png", "alt": None, "section": "Figures", "ordinal": 1},
            # a second note numbers from zero again, above any heading
            {"src": "seismic", "tgt": "img/faults.png", "alt": "Fault map", "section": None, "ordinal": 0},
        ]

    def test_missing_attachment_is_a_provisional_stub(self):
        g = self.build()
        assert g.cypher(
            "MATCH (n {missing:true}) RETURN labels(n)[0] AS label, n.path AS path, n._provisional AS prov"
        ).to_list() == [
            {"label": "Attachment", "path": "img/appendix.pdf", "prov": True},
        ]

    def test_okf_and_loose_still_drop_image_references(self):
        # VAULT.md §6 is an `"obsidian"` rule: the same fixture read as
        # `"loose"` mints no attachment node and no `HAS_*` edge.
        g = okf.build(str(VAULT_BUNDLE), dialect="loose", require_frontmatter=False)
        labels = _labels(g)
        assert labels["Image"] == 0
        assert labels["Attachment"] == 0
        assert _edge_types(g)["HAS_IMAGE"] == 0
        assert _edge_types(g)["HAS_ATTACHMENT"] == 0

    # ── `.kglite/vault.yaml` and `.kglite/` (VAULT.md §7-§8) ──────────────

    def test_the_declared_hub_folds_casing_and_titles_by_frequency(self):
        g = self.build()
        rows = g.cypher("MATCH (k:Keyword) RETURN k.id AS id, k.title AS title ORDER BY id").to_list()
        # atlas.md writes `faults`/`horizons`, seismic.md `faults`/`Faults`/
        # `Horizons`: two nodes, titled by the commonest casing and, for the
        # one-all tie, alphabetically.
        assert rows == [
            {"id": "faults", "title": "faults"},
            {"id": "horizons", "title": "Horizons"},
        ]
        edges = sorted(
            (r["a"], r["k"])
            for r in g.cypher("MATCH (a)-[:HAS_KEYWORD]->(k:Keyword) RETURN a.concept_id AS a, k.id AS k").to_list()
        )
        assert edges == [
            ("atlas", "faults"),
            ("atlas", "horizons"),
            ("seismic", "faults"),
            ("seismic", "horizons"),
        ]
        # …and the key is still a property: a hub reads it, it does not drain it.
        assert g.cypher("MATCH (n {concept_id:'atlas'}) RETURN n.keywords AS k").to_list() == [
            {"k": ["faults", "horizons"]}
        ]

    def test_heading_edges_retype_the_related_topics_links(self):
        g = self.build()
        assert g.cypher("MATCH (a)-[:RELATED_TO]->(b) RETURN a.concept_id AS a, b.concept_id AS b").to_list() == [
            {"a": "links", "b": "roadmap"}
        ]
        # The ladder's own rung for that heading is gone, not doubled.
        assert g.cypher("MATCH ()-[r:RELATED]->() RETURN count(r) AS c").to_list()[0]["c"] == 0

    def test_declared_types_win_over_inference(self):
        g = self.build()
        # atlas.md writes `toc_depth: "2"` — a quoted string, which inference
        # would leave a string. The declaration makes it an integer, so it
        # orders and compares as one.
        rows = g.cypher(
            "MATCH (n:Initiative) RETURN n.concept_id AS id, n.toc_depth AS d, n.toc_depth > 1 AS gt ORDER BY id"
        ).to_list()
        assert rows == [{"id": "atlas", "d": 2, "gt": True}, {"id": "seismic", "d": None, "gt": None}]

    def test_declared_indexes_and_text_index_are_installed(self):
        g = self.build()
        rows = sorted((r["name"], r["type"]) for r in g.cypher("CALL db.indexes()").to_list())
        assert rows == [
            ("Initiative.body", "FULLTEXT"),
            ("Initiative.concept_id", "PROPERTY"),
            ("Initiative.toc_depth", "RANGE"),
        ]
        assert g.has_index("Initiative", "concept_id")
        assert g.has_text_index("Initiative", "body")
        # The BM25 index answers, rather than merely existing: only atlas.md's
        # body holds "umbrella".
        hits = [
            r["id"]
            for r in g.cypher(
                "MATCH (n:Initiative) RETURN n.concept_id AS id, text_bm25(n, 'body', 'umbrella') AS s"
            ).to_list()
            if r["s"] > 0
        ]
        assert hits == ["atlas"]

    def test_the_vault_carries_its_own_skill_and_recipe(self):
        g = self.build()
        assert [s["name"] for s in g.list_skills()] == ["vault_overview"]
        assert g.get_skill("vault_overview")["body"].startswith("Notes with no `type:`")
        assert [(r["recipe"], r["name"]) for r in g.list_recipes()] == [("vault", "by_keyword")]
        recipe = g.get_recipe("vault", "by_keyword")
        assert recipe["recipe_description"] == "Navigating the golden vault."
        # The JSON Schema stayed nested — a flattening reader would have stored
        # one property literally named `properties.keyword.type`.
        assert recipe["parameters"]["properties"]["keyword"] == {"type": "string"}
        assert recipe["cypher"].startswith("MATCH (n)-[:HAS_KEYWORD]->")

    def test_okf_and_loose_ignore_the_vault_config(self):
        # The same bundle read as `loose`: no `Article`, no `Keyword`, and no
        # skill or recipe node — `.kglite/` is a vault construct.
        g = okf.build(str(VAULT_BUNDLE), dialect="loose", require_frontmatter=False)
        labels = _labels(g)
        assert labels["Article"] == 0
        assert labels["Keyword"] == 0
        assert labels["KgliteSkill"] == 0
        assert g.cypher("CALL db.indexes()").to_list() == []

    def test_build_is_deterministic(self):
        a, b = self.build(), self.build()
        for q in ("MATCH (n) RETURN count(n) AS c", "MATCH ()-[r]->() RETURN count(r) AS c"):
            assert a.cypher(q).to_list() == b.cypher(q).to_list()


def test_empty_directory_builds_empty_graph(tmp_path):
    g = okf.build(str(tmp_path))
    assert g.cypher("MATCH (n) RETURN count(n) AS c").to_list()[0]["c"] == 0


def test_skip_dirs_prunes_subtrees(tmp_path):
    (tmp_path / "keep").mkdir()
    (tmp_path / "keep" / "a.md").write_text("---\ntype: Note\n---\nkeep", encoding="utf-8")
    (tmp_path / "vendor" / "repos").mkdir(parents=True)
    (tmp_path / "vendor" / "repos" / "b.md").write_text("---\ntype: Note\n---\nclone", encoding="utf-8")
    (tmp_path / "deep" / "cache").mkdir(parents=True)
    (tmp_path / "deep" / "cache" / "c.md").write_text("---\ntype: Note\n---\ndep", encoding="utf-8")
    g = okf.build(str(tmp_path), skip_dirs=["cache", "vendor/repos"])
    ids = {r["id"] for r in g.cypher("MATCH (n) WHERE n.concept_id IS NOT NULL RETURN n.concept_id AS id").to_list()}
    assert ids == {"keep/a"}


def test_kg_skip_excludes_by_default(tmp_path):
    (tmp_path / "keep.md").write_text("---\ntype: Note\n---\nkeep me", encoding="utf-8")
    (tmp_path / "scratch.md").write_text("---\ntype: Note\nkg_skip: true\n---\nignore me", encoding="utf-8")
    # Default: kg_skip files are excluded from the sweep.
    g = okf.build(str(tmp_path))
    ids = {r["id"] for r in g.cypher("MATCH (n) WHERE n.concept_id IS NOT NULL RETURN n.concept_id AS id").to_list()}
    assert ids == {"keep"}
    # respect_skip=False ingests them anyway.
    g2 = okf.build(str(tmp_path), respect_skip=False)
    ids2 = {r["id"] for r in g2.cypher("MATCH (n) WHERE n.concept_id IS NOT NULL RETURN n.concept_id AS id").to_list()}
    assert ids2 == {"keep", "scratch"}
