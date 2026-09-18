"""`examples/html_to_vault.py` — the reference HTML-to-vault converter.

The example is the answer to "how do I write a converter?", so it is held to
the thing it promises: run it over a tiny HTML corpus and the result is a vault
``okf.validate`` passes and ``okf.build`` loads to a known shape.

The fixture under ``tests/fixtures/vault_html/`` is deliberately small and
deliberately awkward — it carries a folder note, a page cross-listed under two
table-of-contents parents, a cross-reference block, a comma-separated meta list,
an image that exists, one that does not, one in a table cell, one in a heading, one wrapped
in a ``<figure>``,
a link to a page outside the corpus, and one page per construct ``VAULT.md`` §13
asks a converter to emit (an admonition of two kinds, a table with a merged
cell, a parameters table, a Sphinx ``<dl>`` API entry, a nested numbered list, a
language-tagged ``<pre>``, a paragraph with an id) — because those are the cases
a converter gets wrong. Three of them were
found by running this converter over a 6917-page vendor corpus: a markdown
converter escapes the `_` in a wikilink and every link to an underscored stem
dangles; it reduces an image inside a table cell to its alt text; and a hub key
the source spells as one value joins no hub, because a hub reads a key's list
(VAULT.md §7).

Skipped unless BeautifulSoup and markdownify are installed: they are the
example's dependencies, not kglite's (``requirements/examples.txt``).
"""

from __future__ import annotations

from collections import Counter
import hashlib
from pathlib import Path
import subprocess
import sys

import pytest

from kglite import okf

pytest.importorskip("bs4")
pytest.importorskip("markdownify")

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "examples" / "html_to_vault.py"
FIXTURE = REPO / "tests" / "fixtures" / "vault_html"

EXPECTED_FILES = [
    ".kglite/vault.yaml",
    "API_reference.md",
    "Guide.md",
    "Guide/Getting_started.md",
    "Guide/Guide_3.md",
    "Guide/Install.md",
    "Guide/Usage.md",
    "Orphan.md",
    "Reference.md",
    "Structure.md",
    "img/diagram.png",
    "img/logo.png",
]
EXPECTED_LABELS = Counter({"Article": 9, "Component": 1, "Image": 3, "Keyword": 3})
EXPECTED_EDGES = Counter(
    {"CHILD_OF": 5, "HAS_IMAGE": 5, "HAS_KEYWORD": 7, "LINKS_TO": 5, "RELATED_TO": 2, "USES_COMPONENT": 1}
)


def convert(out: Path, *extra: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            str(FIXTURE),
            str(out),
            *extra,
            "--toc",
            str(FIXTURE / "toc.json"),
            "--default-label",
            "Article",
            "--hub",
            "keywords=Keyword:HAS_KEYWORD:ci",
            "--hub",
            "component=Component:USES_COMPONENT:ci",
            "--index",
            "Article.domain",
            "--index",
            "Article.toc_depth:range",
            "--embed",
            "Article.description",
            "--block-ids",
        ],
        cwd=REPO,
        capture_output=True,
        text=True,
        timeout=90,
    )


@pytest.fixture(scope="module")
def vault(tmp_path_factory) -> Path:
    out = tmp_path_factory.mktemp("vault_html") / "vault"
    done = convert(out)
    assert done.returncode == 0, done.stdout + done.stderr
    return out


@pytest.fixture(scope="module")
def structured(tmp_path_factory) -> Path:
    """The same corpus with `--emit-structure`, which is off by default.

    `structure:` is an unknown top-level key to any kglite released before it
    and an unknown key fails the build (VAULT.md §7.1), so the flag exists to
    keep the converter's default output loadable by the kglite in hand.
    """
    out = tmp_path_factory.mktemp("vault_structure") / "vault"
    done = convert(out, "--emit-structure", "--api-label", "Article", "--no-validate")
    assert done.returncode == 0, done.stdout + done.stderr
    return out


def snapshot(root: Path) -> dict[str, str]:
    return {
        str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in sorted(root.rglob("*"))
        if p.is_file()
    }


def test_the_converter_writes_the_expected_vault(vault):
    assert sorted(snapshot(vault)) == EXPECTED_FILES


def test_the_folder_note_stands_beside_its_directory(vault):
    # VAULT.md §2.3: `Guide.md` next to `Guide/` is what makes the TOC a
    # hierarchy on disk, so the two names must agree exactly.
    assert (vault / "Guide.md").is_file()
    assert (vault / "Guide").is_dir()


def test_a_toc_child_labelled_like_its_parent_does_not_become_a_second_folder_note(vault):
    # `Guide/Guide.md` is the other spelling of the folder note `Guide.md`
    # already is, and VAULT.md §2.3 makes declaring both an error.
    assert not (vault / "Guide" / "Guide.md").exists()
    assert (vault / "Guide" / "Guide_3.md").is_file()


def test_the_cross_listed_page_is_one_note_with_both_parents(vault):
    usage = (vault / "Guide" / "Usage.md").read_text(encoding="utf-8")
    assert '- "[[Guide]]"' in usage and '- "[[Reference]]"' in usage
    assert not (vault / "Reference").exists()


def test_the_cross_reference_block_becomes_a_related_topics_section(vault):
    install = (vault / "Guide" / "Install.md").read_text(encoding="utf-8")
    assert "## Related topics\n\n- [[Usage]]" in install
    # The parent link is frontmatter, not prose, and the block itself is gone.
    assert "Parent topic" not in install


def test_a_link_out_of_the_corpus_stays_prose(vault):
    usage = (vault / "Guide" / "Usage.md").read_text(encoding="utf-8")
    assert "The changelog lives outside this corpus." in usage
    assert "changelog.html" not in usage


def test_the_meta_list_is_a_yaml_sequence_and_boilerplate_is_dropped(vault):
    guide = (vault / "Guide.md").read_text(encoding="utf-8")
    assert "keywords:\n- alpha\n- Beta\n- gamma\n" in guide
    assert "generator" not in guide


def test_a_repeated_list_valued_meta_key_keeps_every_entry(vault):
    # A page may carry `<meta name="keywords">` more than once, and for a key
    # that is a list either "first wins" or "last wins" silently drops entries:
    # one page of a 6917-page vendor corpus names three components across three
    # tags, and both spellings of the rule kept one of the three.
    guide = (vault / "Guide.md").read_text(encoding="utf-8")
    assert "- gamma" in guide


def test_a_wikilink_keeps_the_underscores_in_the_name_it_spells(vault):
    # A stem is a name, not prose: `[[Getting\\_started]]` resolves to nothing
    # and the build reports a dangling link instead of an edge.
    guide = (vault / "Guide.md").read_text(encoding="utf-8")
    assert "[[Getting_started|Getting started]]" in guide
    assert "\\_" not in guide


def test_an_image_in_a_table_cell_or_a_heading_is_still_an_image(vault):
    # markdownify reduces both to alt text by default, which loses the picture.
    install = (vault / "Guide" / "Install.md").read_text(encoding="utf-8")
    assert "![The widget logo](img/logo.png)" in install
    reference = (vault / "Reference.md").read_text(encoding="utf-8")
    assert "## Settings ![The widget logo](img/logo.png)" in reference


def test_an_image_wrapped_in_a_figure_is_still_an_image(vault):
    # `keep_inline_images_in` is matched against the image's *direct* parent, so
    # `<figure><img></figure>` in a table cell kept the caption and dropped the
    # picture — three of a 6917-page vendor corpus's, in the element HTML
    # defines for exactly this purpose.
    install = (vault / "Guide" / "Install.md").read_text(encoding="utf-8")
    assert "![Signal flow](img/diagram.png)" in install


def test_a_range_index_is_declared_as_the_mapping_the_schema_asks_for(vault):
    # VAULT.md §7 spells a range declaration `{range: <prop>}`. Emitted as a
    # stringified Python dict it is a *string* entry, and the build installs an
    # equality index on a property named `{'range': 'toc_depth'}` — no range
    # index, no complaint.
    config = (vault / ".kglite" / "vault.yaml").read_text(encoding="utf-8")
    assert "- {range: toc_depth}" in config
    graph = okf.build(str(vault), dialect="obsidian")
    assert [i["property"] for i in graph.list_indexes()] == ["domain"]


def test_a_meta_map_entry_that_is_not_a_mapping_is_named(tmp_path):
    # The table is hand-written, so a `"key": "value"` where a mapping belongs is
    # the likely typo; it used to reach `.items()` and die with an AttributeError
    # naming neither the file nor the key.
    table = tmp_path / "meta.json"
    table.write_text('{"domain": "Geology"}', encoding="utf-8")
    done = convert(tmp_path / "vault", "--meta-map", str(table))
    assert done.returncode != 0
    assert "domain" in done.stdout + done.stderr
    assert "AttributeError" not in done.stdout + done.stderr


def test_a_hub_key_the_source_spells_as_one_value_is_written_as_a_list(vault):
    # VAULT.md §7: a hub reads a key's list entries, and a scalar joins no hub.
    start = (vault / "Guide" / "Getting_started.md").read_text(encoding="utf-8")
    assert "component:\n- Widget Core\n" in start


def test_an_admonition_becomes_a_callout_keeping_the_sources_own_word(vault):
    # VAULT.md §5.7: a callout's type is arbitrary, and §13.2 names folding
    # `versionadded` into `note` an anti-pattern. Flattened to a paragraph —
    # which is what a markdown converter does with the div — the admonition is
    # nothing in the graph at all.
    page = (vault / "Structure.md").read_text(encoding="utf-8")
    assert "> [!note] Check the datum\n> Depth values are metres below MSL." in page
    assert "> [!versionadded]\n> Added in version 2.1: the second widget slot." in page


def test_a_callout_inside_a_callout_nests(vault):
    # Callouts nest (VAULT.md §5.7), and DITA puts a `note` inside a step's
    # warning routinely. The inner one is rendered first, so the outer prefixes
    # lines that are already a callout. Its `note tip note_tip` classes name the
    # kind `tip` — `note` is the box, not the kind — and its `Tip:` title says
    # only what the kind already says, which Obsidian displays itself.
    page = (vault / "Structure.md").read_text(encoding="utf-8")
    assert "> [!warning] Mind the gap\n> Stopping mid-write leaves a partial file.\n>\n> > [!tip]\n> > Wait" in page


def test_a_table_becomes_gfm_with_pipes_escaped_and_merges_flattened(vault):
    # Raw HTML is never structure (VAULT.md §1.4), so a `<table>` has to leave
    # as a pipe table: a `|` in a cell would end the cell, a `<br>` would end
    # the row, and a `colspan` has no GFM spelling, so it is written once and
    # the columns it covered are left empty.
    page = (vault / "Structure.md").read_text(encoding="utf-8")
    assert "| Setting | Default | Notes |\n| --- | --- | --- |" in page
    assert "| slots | 4 | per machine and per slot |" in page
    assert "| reserved |  | ignored \\| for now |" in page
    # A pipe table cannot hold a table, so a nested one is its text or it ends
    # the row it sits in.
    assert "| nested | 0 | inner a inner b |" in page


def test_a_wikilink_in_a_cell_keeps_its_target_and_drops_its_display_text(vault):
    # Obsidian's help writes `[[Usage\|the usage guide]]` in a table, but
    # VAULT.md §5.1 reads a wikilink's target up to the first `|` and unescapes
    # nothing, so the escaped form names a note called `Usage\`: 398 of the
    # Petrel corpus's cell links dangled that way. An unescaped `|` would end
    # the cell instead. The target is the link; §5.1 keeps the display text only
    # where `structure:` is declared, so dropping it here costs a `label`.
    page = (vault / "Structure.md").read_text(encoding="utf-8")
    assert "| [[Usage]] | - | see also |" in page
    assert "Usage\\|" not in page


def test_a_parameters_table_is_keyed_by_a_name_column(vault):
    # `tables: {under_heading: Parameters, key_column: name}` keys each row on
    # that column (VAULT.md §7.1); the source calls it `Parameter`, so a rule
    # written against the spec's own example would key on nothing.
    page = (vault / "API_reference.md").read_text(encoding="utf-8")
    assert "| name | Type | Description |" in page
    assert "| a | int | the first operand |" in page


def test_a_symbol_definition_becomes_a_heading_and_its_body_nests_under_it(vault):
    # `<dt>` is prose (§13.2) and `key_from_heading:` relabels a *Section*, so
    # a Sphinx API entry has to arrive as a heading — carrying the qualified
    # name and the parentheses the default `when_matches` regex needs. The
    # `Parameters` heading inside the entry is demoted below it, or it would
    # close the symbol's section instead of nesting in it.
    page = (vault / "API_reference.md").read_text(encoding="utf-8")
    assert "#### pkg.mod.func(a, b)" in page
    assert "##### Parameters" in page


def test_a_definition_list_whose_ids_name_nothing_keeps_its_terms(vault):
    # Every DITA `<dt>` carries a generated anchor, so "has an id" is not the
    # Sphinx shape: reading one as the symbol name replaced 275 of the Petrel
    # corpus's GUI labels with `GUID-…__DLENTRY_…` and threw the label away. An
    # id counts only when it ends in the term's own name.
    page = (vault / "API_reference.md").read_text(encoding="utf-8")
    assert "**Widget**\n\nThe thing on the machine." in page
    assert "DLENTRY" not in page


def test_a_fence_carries_the_language_its_class_named(vault):
    # `langs: [python]` matches nothing when the converter drops the language
    # on the way in (VAULT.md §13.2), and the class sits on Sphinx's wrapper
    # div rather than on the `<pre>` itself.
    page = (vault / "Structure.md").read_text(encoding="utf-8")
    assert "```python\nwidget = Widget()\nwidget.start()\n```" in page


def test_a_paragraph_with_an_id_becomes_a_citable_block(vault):
    # VAULT.md §5.7: a block id holds Latin letters, digits and dashes only, so
    # `id="para_1"` names `^para-1`; `^para_1` is text and links to nothing.
    page = (vault / "Structure.md").read_text(encoding="utf-8")
    assert "A widget writes its state to disk on every stop. ^para-1" in page
    assert "^para_1" not in page


def test_a_nested_list_indents_four_spaces(vault):
    # markdownify indents a nested list by the width of its parent's bullet, so
    # `1.` nests three spaces and `10.` four; the depth of a step should not
    # depend on how many steps came before it.
    page = (vault / "Structure.md").read_text(encoding="utf-8")
    assert "1. Open the panel.\n    1. Select the **Slots** tab.\n    2. Confirm the change.\n2. Press" in page


def test_the_default_vault_declares_no_structure_block(vault):
    config = (vault / ".kglite" / "vault.yaml").read_text(encoding="utf-8")
    assert "structure:" not in config


def test_the_structure_block_declares_only_the_rules_the_corpus_carries(structured):
    # A rule that matched nothing is the fastest defect there is (VAULT.md
    # §13.4), so a rule is declared only for a construct the run actually
    # emitted — and the keys are §7.1's, because an unknown key inside
    # `structure:` fails the build exactly as an unknown top-level key does.
    config = (structured / ".kglite" / "vault.yaml").read_text(encoding="utf-8")
    assert "structure:\n" in config
    assert "  sections: {label: Section, edge: HAS_SECTION, parent: PARENT_SECTION, next: NEXT_SECTION}" in config
    assert "  chunks: {label: Chunk, edge: HAS_CHUNK, next: NEXT_CHUNK, max_words: 650, max_chars: 6000}" in config
    assert "  callouts: {label: Note, edge: HAS_NOTE}" in config
    assert "  code_fences: {label: Example, edge: HAS_EXAMPLE}" in config
    assert "  ordered_lists: {label: ProcedureStep, container: Procedure, edge: HAS_STEP, next: NEXT_STEP}" in config
    assert "  - {under_heading: ^(Parameters|Arguments|Fields)$, label: ApiParameter, key_column: name," in config
    assert "  key_from_heading: {label: ApiSymbol, property: qualified_name, under_label: Article}" in config


def test_the_structure_run_writes_the_same_notes_as_the_default_one(vault, structured):
    # `--emit-structure` declares how the notes are read; it must not change
    # what they say, or the flag would be a second converter.
    assert {name: digest for name, digest in snapshot(structured).items() if name != ".kglite/vault.yaml"} == {
        name: digest for name, digest in snapshot(vault).items() if name != ".kglite/vault.yaml"
    }


def test_the_vault_validates_with_only_the_missing_image_warned(vault):
    report = okf.validate(str(vault), dialect="obsidian")
    assert report.errors == []
    assert len(report.warnings) == 1 and "img/gone.png" in report.warnings[0]
    assert report.ok
    assert report.counts["missing_attachments"] == 1
    assert report.counts["folder_notes"] == 1
    assert report.counts["embed_targets"] == [("Article", "description")]


def test_the_vault_builds_to_the_expected_shape(vault):
    graph = okf.build(str(vault), dialect="obsidian")
    labels = Counter(r["l"] for r in graph.cypher("MATCH (n) RETURN labels(n)[0] AS l").to_list())
    edges = Counter(r["t"] for r in graph.cypher("MATCH ()-[r]->() RETURN type(r) AS t").to_list())
    assert labels == EXPECTED_LABELS
    assert edges == EXPECTED_EDGES
    missing = graph.cypher("MATCH (n {missing: true}) RETURN n.path AS path").to_list()
    assert [r["path"] for r in missing] == ["img/gone.png"]


def test_converting_twice_into_one_directory_is_byte_identical(vault):
    before = snapshot(vault)
    again = convert(vault)
    assert again.returncode == 0, again.stdout + again.stderr
    assert snapshot(vault) == before
