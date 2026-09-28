"""The fluent date context answers as `FOR VALID_TIME AS OF` does.

`date()`, `traverse(at=/during=)` and `valid_at()` / `valid_during()` resolve
to the filter a `FOR VALID_TIME AS OF` statement runs under: a node passes
only when it is valid under every declared label it carries, a relationship is
judged by the declaration keyed on its own source, a hop keeps its far node
only when that node passes too, and a relationship type holding several
unkeyed declarations is refused. Every fluent step that walks the graph under
the context — `select`, `traverse`, `expand`, `where_connected`,
`relationships`, `compare`, `to_subgraph` — goes through it.

Regression rationale: the fluent path used to be a second evaluator that read
only the selected type's own declaration, never tested a traversal's target
nodes, applied `select(limit=)` before the date filter, and read "today" in
local time.
"""

from __future__ import annotations

import pytest

import kglite

MODES = [None, "mapped", "disk"]
MODE_IDS = ["memory", "mapped", "disk"]


def _new(storage, tmp_path) -> kglite.KnowledgeGraph:
    if storage == "disk":
        return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    if storage == "mapped":
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph()


def _titles(selection) -> list[str]:
    return sorted(row["title"] for row in selection.collect())


def _cypher_titles(g, query: str) -> list[str]:
    return sorted(row["t"] for row in g.cypher(query).to_list())


@pytest.fixture(params=MODES, ids=MODE_IDS)
def employment(request, tmp_path):
    """Acme closed at the end of 2008; the employment edge to it runs to
    mid-2009, so a date in between reaches Acme only through a valid edge."""
    g = _new(request.param, tmp_path)
    g.cypher(
        """
        CREATE (p:Person {id: 1, title: 'Pat'}),
               (a:Company {id: 10, title: 'Acme', vf: '2000-01-01', vt: '2008-12-31'}),
               (b:Company {id: 11, title: 'Beta', vf: '2009-07-01', vt: null}),
               (c:City {id: 20, title: 'Oslo'}),
               (p)-[:EMPLOYED_AT {vf: '2005-01-01', vt: '2009-06-30'}]->(a),
               (p)-[:EMPLOYED_AT {vf: '2009-07-01', vt: null}]->(b),
               (a)-[:LOCATED_IN {vf: '2000-01-01', vt: null}]->(c),
               (b)-[:LOCATED_IN {vf: '2009-07-01', vt: '2099-01-01'}]->(c)
        """
    ).to_list()
    for name in ("Company", "EMPLOYED_AT", "LOCATED_IN"):
        g.set_temporal(name, "vf", "vt")
    return g


class TestTraverseFiltersTargets:
    def test_a_valid_edge_to_a_closed_company_reaches_nothing(self, employment):
        for day in ("2009-03-01", "2009-06-15"):
            reached = employment.date(day).select("Person").traverse("EMPLOYED_AT")
            assert _titles(reached) == [], day
            assert _titles(reached.traverse("LOCATED_IN")) == [], day
            cypher = _cypher_titles(
                employment,
                f"FOR VALID_TIME AS OF date('{day}') MATCH (:Person)-[:EMPLOYED_AT]->(c) RETURN c.title AS t",
            )
            assert cypher == [], day

    def test_the_valid_company_is_reached(self, employment):
        reached = employment.date("2009-09-01").select("Person").traverse("EMPLOYED_AT")
        assert _titles(reached) == ["Beta"]
        assert _titles(reached.traverse("LOCATED_IN")) == ["Oslo"]

    def test_at_filters_targets_too_and_temporal_false_is_the_escape_hatch(self, employment):
        person = employment.select("Person")
        assert _titles(person.traverse("EMPLOYED_AT", at="2009-03-01")) == []
        assert _titles(person.traverse("EMPLOYED_AT", temporal=False)) == ["Acme", "Beta"]
        everything = employment.date("all").select("Person").traverse("EMPLOYED_AT")
        assert _titles(everything) == ["Acme", "Beta"]


class TestContextWalks:
    """Every step that walks the graph under a context reads the same
    filter: a closed company is not reached, a closed edge not followed."""

    def test_expand(self, employment):
        expanded = employment.date("2009-03-01").select("Person").expand(2)
        assert _titles(expanded) == ["Pat"]
        expanded = employment.date("2009-09-01").select("Person").expand(2)
        assert _titles(expanded) == ["Beta", "Oslo", "Pat"]

    def test_where_connected(self, employment):
        before = employment.date("2009-03-01").select("Person").where_connected("EMPLOYED_AT")
        assert _titles(before) == []
        after = employment.date("2009-09-01").select("Person").where_connected("EMPLOYED_AT")
        assert _titles(after) == ["Pat"]

    def test_relationships(self, employment):
        rels = employment.date("2009-09-01").select("Person").relationships()
        assert sorted(rels["Pat"]["outgoing"]["EMPLOYED_AT"]) == ["Beta"]
        everything = employment.date("all").select("Person").relationships()
        assert sorted(everything["Pat"]["outgoing"]["EMPLOYED_AT"]) == ["Acme", "Beta"]

    def test_to_subgraph_keeps_only_visible_relationships(self, employment):
        chosen = employment.date("2009-09-01").select("Person").expand(1)
        sub = chosen.to_subgraph()
        rows = sub.cypher("MATCH (a)-[r]->(b) RETURN a.title AS a, b.title AS b").to_list()
        assert rows == [{"a": "Pat", "b": "Beta"}]
        everything = employment.date("all").select("Person", temporal=False).expand(1).to_subgraph()
        count = everything.cypher("MATCH ()-[r]->() RETURN count(r) AS c").to_list()
        assert count == [{"c": 2}]


@pytest.fixture
def statuses():
    g = kglite.KnowledgeGraph()
    g.cypher(
        """
        UNWIND [
          {id: 1, title: 'Producing', vf: '2000-01-01', vt: '2010-06-01'},
          {id: 2, title: 'Shut down', vf: '2010-06-01', vt: null}
        ] AS r CREATE (:Status {id: r.id, title: r.title, vf: r.vf, vt: r.vt})
        """
    ).to_list()
    g.cypher("CALL db.temporal.declare({node: 'Status', from: 'vf', to: 'vt', convention: 'half_open'})")
    return g


class TestSelect:
    def test_the_limit_counts_visible_nodes(self, statuses):
        """The limit used to run before the date filter, which then dropped
        the one row it had kept."""
        assert _titles(statuses.date("2010-06-01").select("Status", limit=1)) == ["Shut down"]
        assert _titles(statuses.date("2010-05-31").select("Status", limit=1, sort="title")) == ["Producing"]

    def test_the_label_rule_reads_every_declared_label(self):
        g = kglite.KnowledgeGraph()
        g.cypher(
            "CREATE (:Status:Tracked {id: 1, title: 's', vf: '2000', vt: '2001'}),"
            " (:Other:Tracked {id: 2, title: 'o', vf: '2003', vt: null})"
        ).to_list()
        g.cypher("CALL db.temporal.declare({node: 'Tracked', from: 'vf', to: 'vt', convention: 'closed'})")
        assert _titles(g.date("2005").select("Status")) == []
        assert _titles(g.date("2005").select("Tracked", include_secondary=True)) == ["o"]
        cypher = _cypher_titles(g, "FOR VALID_TIME AS OF date('2005-01-01') MATCH (n:Status) RETURN n.title AS t")
        assert cypher == []


class TestRequests:
    @pytest.fixture
    def graph(self):
        g = kglite.KnowledgeGraph()
        g.cypher(
            "CREATE (a:F {title: 'a', vf: date('2002-06-01'), vt: date('2003-01-01')}), (p:P {title: 'p'}),"
            " (p)-[:IN {vf: date('2002-06-01'), vt: date('2003-01-01')}]->(a)"
        ).to_list()
        g.set_temporal("F", "vf", "vt")
        g.set_temporal("IN", "vf", "vt")
        return g

    def test_a_partial_end_covers_its_whole_period(self, graph):
        """`valid_during('2001', '2002')` reaches through 2002-12-31, as
        `date('2001', '2002')` always did."""
        assert _titles(graph.select("F", temporal=False).valid_during("2001", "2002")) == ["a"]
        assert _titles(graph.select("P").traverse("IN", during=("2001", "2002"))) == ["a"]
        assert _titles(graph.date("2001", "2002").select("F")) == ["a"]

    def test_valid_at_with_no_date_reads_a_range_context(self, graph):
        ranged = graph.date("2001", "2002").select("F", temporal=False)
        assert _titles(ranged.valid_at()) == ["a"]


class TestErrors:
    def test_an_unreadable_bound_names_the_step(self, statuses):
        # Refused as a write; a graph an earlier version saved can hold it,
        # written here through the one writer the check does not judge.
        with pytest.raises(kglite.CypherExecutionError, match="node '2', property 'vt'"):
            statuses.cypher("MATCH (s:Status {id: 2}) SET s.vt = 20210101").to_list()
        held = statuses.select("Status", temporal=False).where({"id": 2}).update({"vt": 20210101})["graph"]
        with pytest.raises(ValueError, match=r"select\(\): node '2', property 'vt'"):
            held.date("2011").select("Status")


class TestNamedBounds:
    """`valid_at` / `valid_during` naming bounds nothing declares read every
    node's own properties. A pair of typed date columns is read as days (a
    date column holds nothing unreadable); any other column keeps the checked
    read, where an unreadable bound raises whatever the instant."""

    @pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
    def test_a_bad_to_raises_on_a_row_whose_from_excludes_the_instant(self, storage, tmp_path):
        g = _new(storage, tmp_path)
        g.cypher(
            "CREATE (:W {title: 'a', vf: date('2001-01-01'), vt: date('2003-01-01')}),"
            " (:W {title: 'late', vf: date('2020-01-01'), vt: 'not a date'})"
        ).to_list()
        with pytest.raises(ValueError, match=r"valid_at\(\): node .*'vt'"):
            g.select("W").valid_at("2002-01-01", "vf", "vt")

    @pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
    def test_date_columns_answer_as_the_scalar_does(self, storage, tmp_path):
        g = _new(storage, tmp_path)
        g.cypher(
            "UNWIND range(0, 39) AS i CREATE (:W {title: 'w' + i,"
            " vf: CASE WHEN i % 7 = 3 THEN null ELSE date('2000-01-01') + duration({days: 120 * i}) END,"
            " vt: CASE WHEN i % 5 = 3 THEN null ELSE date('2001-01-01') + duration({days: 150 * i}) END})"
        ).to_list()
        # Node 0's column values come first, so both columns are typed dates;
        # the instants include a node's exact `from` and `to` day.
        for day in ("2000-01-01", "2001-01-01", "2003-06-30", "2006-01-01", "2010-12-31"):
            expected = _cypher_titles(
                g, f"MATCH (n:W) WHERE valid_at(n, date('{day}'), 'vf', 'vt') RETURN n.title AS t"
            )
            assert _titles(g.select("W").valid_at(day, "vf", "vt")) == expected, day
        overlap = _cypher_titles(
            g,
            "MATCH (n:W) WHERE valid_during(n, date('2003-01-01'), date('2004-12-31'), 'vf', 'vt') RETURN n.title AS t",
        )
        assert _titles(g.select("W").valid_during("2003-01-01", "2004-12-31", "vf", "vt")) == overlap


class TestCountsAndOrphans:
    def test_degrees_count_only_valid_relationships_to_valid_nodes(self, employment):
        assert employment.date("2009-03-01").select("Person").degrees() == {"Pat": 0}
        assert employment.date("2009-09-01").select("Person").degrees() == {"Pat": 1}
        assert employment.date("all").select("Person").degrees() == {"Pat": 2}

    def test_where_orphans_counts_only_valid_relationships(self, employment):
        people = employment.date("2009-03-01").select("Person")
        assert _titles(people.where_orphans(include_orphans=True)) == ["Pat"]
        assert _titles(employment.date("2009-09-01").select("Person").where_orphans(include_orphans=True)) == []


def _two_labels() -> kglite.KnowledgeGraph:
    """`x` carries A and B: valid under A at 2010, not under B. `old` (A only)
    has ended, so A is not timeless then; `y` (B only) is valid."""
    g = kglite.KnowledgeGraph()
    g.cypher(
        "CREATE (:A:B {id: 1, title: 'x', a_from: '2000-01-01', a_to: '2030-01-01', c_from: '2000-01-01',"
        " b_from: '2000-01-01', b_to: '2005-01-01'}),"
        " (:A {id: 3, title: 'old', a_from: '2000-01-01', a_to: '2001-01-01', c_from: '2000-01-01'}),"
        " (:B {id: 2, title: 'y', b_from: '2000-01-01', b_to: '2030-01-01'})"
    ).to_list()
    g.cypher("CALL db.temporal.declare({node: 'A', from: 'a_from', to: 'a_to', convention: 'closed'})")
    g.cypher("CALL db.temporal.declare({node: 'B', from: 'b_from', to: 'b_to', convention: 'closed'})")
    return g


DAY = "2010-01-01"
WARMUPS = {
    "none": lambda g: None,
    "fluent_context": lambda g: g.date(DAY).select("A").collect(),
    "cypher_b": lambda g: g.cypher(f"FOR VALID_TIME AS OF date('{DAY}') MATCH (n:B) RETURN n").to_list(),
    "cypher_ab": lambda g: g.cypher(f"FOR VALID_TIME AS OF date('{DAY}') MATCH (a:A)-->(b:B) RETURN a").to_list(),
}


class TestRequestsFollowTheLabelRule:
    """`valid_at()` / `valid_during()` judge a node under every declared label
    it carries, as the date context and `FOR VALID_TIME AS OF` do. They used to
    read only the primary type's bounds, so the answer depended on which
    cached mask an earlier query had left: `['x']` on a fresh graph, `[]`
    after any query at the same instant that reached B."""

    @pytest.mark.parametrize("warmup", list(WARMUPS))
    def test_the_answer_does_not_depend_on_earlier_queries(self, warmup):
        g = _two_labels()
        WARMUPS[warmup](g)
        everything = g.select("A", temporal=False)
        assert _titles(everything.valid_at(DAY)) == []
        assert _titles(everything.valid_during(DAY, DAY)) == []
        named = everything.valid_at(DAY, date_from_field="c_from", date_to_field="a_to")
        assert _titles(named) == []
        assert _cypher_titles(g, f"FOR VALID_TIME AS OF date('{DAY}') MATCH (n:A) RETURN n.title AS t") == []

    def test_a_mixed_level_judges_each_node_by_its_own_labels(self):
        g = _two_labels()
        level = g.select("B", include_secondary=True, temporal=False)
        assert _titles(level) == ["x", "y"]
        assert _titles(level.valid_at(DAY)) == ["y"]
        assert _cypher_titles(g, f"FOR VALID_TIME AS OF date('{DAY}') MATCH (n:B) RETURN n.title AS t") == ["y"]

    @pytest.mark.parametrize("warmup", list(WARMUPS))
    def test_a_context_query_does_not_depend_on_earlier_queries(self, warmup):
        g = _two_labels()
        WARMUPS[warmup](g)
        query = f"FOR VALID_TIME AS OF date('{DAY}') MATCH (n:A) RETURN n.title AS t"
        assert _cypher_titles(g, query) == []
        assert _titles(g.date(DAY).select("A")) == []


def _edges(g) -> list[tuple[str, str]]:
    rows = g.cypher("MATCH (a)-[r]->(b) RETURN a.title AS a, b.title AS b").to_list()
    return sorted((row["a"], row["b"]) for row in rows)


class TestSaveSubset:
    """`save_subset()` writes what `to_subgraph()` keeps: under a date context
    only the relationships valid then. It used to write every relationship
    between the selected nodes."""

    def test_the_saved_file_equals_to_subgraph(self, employment, tmp_path):
        chosen = employment.date("all").select("Person").expand(1).date("2009-09-01")
        path = tmp_path / "subset.kgl"
        chosen.save_subset(str(path))
        saved = kglite.load(str(path))
        extracted = chosen.to_subgraph()
        assert _edges(extracted) == [("Pat", "Beta")]
        assert _edges(saved) == _edges(extracted)
        assert sorted(n["title"] for n in saved.cypher("MATCH (n) RETURN n.title AS title").to_list()) == [
            "Acme",
            "Beta",
            "Pat",
        ]

    def test_without_a_context_the_file_is_unchanged(self, employment, tmp_path):
        chosen = employment.date("all").select("Person").expand(1)
        path = tmp_path / "subset.kgl"
        chosen.save_subset(str(path))
        assert _edges(kglite.load(str(path))) == [("Pat", "Acme"), ("Pat", "Beta")]
