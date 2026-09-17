"""`examples/html_to_vault.py` — the reference HTML-to-vault converter.

The example is the answer to "how do I write a converter?", so it is held to
the thing it promises: run it over a tiny HTML corpus and the result is a vault
``okf.validate`` passes and ``okf.build`` loads to a known shape.

The fixture under ``tests/fixtures/vault_html/`` is deliberately small and
deliberately awkward — it carries a folder note, a page cross-listed under two
table-of-contents parents, a cross-reference block, a comma-separated meta list,
an image that exists, one that does not, one in a table cell, one in a heading, one wrapped
in a ``<figure>``,
and a link to a page outside the corpus — because those are the cases a converter gets wrong. Three of them were
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
    "Guide.md",
    "Guide/Getting_started.md",
    "Guide/Guide_3.md",
    "Guide/Install.md",
    "Guide/Usage.md",
    "Orphan.md",
    "Reference.md",
    "img/diagram.png",
    "img/logo.png",
]
EXPECTED_LABELS = Counter({"Article": 7, "Component": 1, "Image": 3, "Keyword": 3})
EXPECTED_EDGES = Counter(
    {"CHILD_OF": 5, "HAS_IMAGE": 5, "HAS_KEYWORD": 7, "LINKS_TO": 4, "RELATED_TO": 2, "USES_COMPONENT": 1}
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
