"""Tests for the show() display method."""

import pandas as pd
import pytest

import kglite


@pytest.fixture
def initiative_graph():
    """Graph with Initiative -> Proposal -> Site chains."""
    g = kglite.KnowledgeGraph()
    # Initiatives
    df_init = pd.DataFrame(
        {
            "id": [1, 2, 3],
            "title": ["Juniper", "Tundra", "Ember"],
            "status": ["producing", "producing", "producing"],
        }
    )
    g.add_nodes(df_init, "Initiative", "id", "title")

    # Proposals
    df_pros = pd.DataFrame(
        {
            "id": [10, 20, 30],
            "title": ["Alpha", "Beta", "Gamma"],
            "area": ["North Sea", "North Sea", "Barents"],
        }
    )
    g.add_nodes(df_pros, "Proposal", "id", "title")

    # Sites
    df_well = pd.DataFrame(
        {
            "id": [100, 200, 300],
            "title": ["W1", "W2", "W3"],
            "depth": [1500, 2500, 3500],
        }
    )
    g.add_nodes(df_well, "Site", "id", "title")

    # Initiative -> Proposal connections
    g.cypher("MATCH (d:Initiative {id: 1}), (p:Proposal {id: 10}) CREATE (d)-[:HAS_PROPOSAL]->(p)")
    g.cypher("MATCH (d:Initiative {id: 1}), (p:Proposal {id: 20}) CREATE (d)-[:HAS_PROPOSAL]->(p)")
    g.cypher("MATCH (d:Initiative {id: 2}), (p:Proposal {id: 30}) CREATE (d)-[:HAS_PROPOSAL]->(p)")
    # Proposal -> Site connections
    g.cypher("MATCH (p:Proposal {id: 10}), (w:Site {id: 100}) CREATE (p)-[:TESTED_BY]->(w)")
    g.cypher("MATCH (p:Proposal {id: 20}), (w:Site {id: 200}) CREATE (p)-[:TESTED_BY]->(w)")
    g.cypher("MATCH (p:Proposal {id: 30}), (w:Site {id: 300}) CREATE (p)-[:TESTED_BY]->(w)")

    return g


class TestShowSingleLevel:
    """show() on a selection without traversals."""

    def test_basic_id_title(self, initiative_graph):
        output = initiative_graph.select("Initiative").show(["id", "title"])
        assert "Initiative(1, Juniper)" in output
        assert "Initiative(2, Tundra)" in output
        assert "Initiative(3, Ember)" in output

    def test_single_column(self, initiative_graph):
        output = initiative_graph.select("Initiative").show(["id"])
        assert "Initiative(1)" in output
        assert "Initiative(2)" in output

    def test_default_columns(self, initiative_graph):
        """Default columns are id and title."""
        output = initiative_graph.select("Initiative").show()
        assert "Initiative(1, Juniper)" in output

    def test_extra_property(self, initiative_graph):
        output = initiative_graph.select("Initiative").show(["title", "status"])
        assert "Initiative(Juniper, producing)" in output

    def test_missing_property_skipped(self, initiative_graph):
        """Properties not on a type are silently skipped."""
        output = initiative_graph.select("Initiative").show(["id", "nonexistent"])
        assert "Initiative(1)" in output

    def test_empty_selection(self, initiative_graph):
        output = initiative_graph.select("NonExistent").show()
        assert "empty" in output.lower()

    def test_limit(self, initiative_graph):
        output = initiative_graph.select("Initiative").show(["id"], limit=2)
        lines = [line for line in output.strip().split("\n") if line.startswith("Initiative")]
        assert len(lines) == 2
        assert "... and 1 more" in output


class TestShowMultiLevel:
    """show() after traverse() — displays traversal chains."""

    def test_two_level_chain(self, initiative_graph):
        output = initiative_graph.select("Initiative").traverse("HAS_PROPOSAL").show(["id", "title"])
        # Initiative 1 connects to Proposal 10 and 20
        assert "Initiative(1, Juniper) -> Proposal(" in output
        # Initiative 2 connects to Proposal 30
        assert "Initiative(2, Tundra) -> Proposal(30, Gamma)" in output

    def test_three_level_chain(self, initiative_graph):
        output = (
            initiative_graph.select("Initiative").traverse("HAS_PROPOSAL").traverse("TESTED_BY").show(["id", "title"])
        )
        # Full chain: Initiative -> Proposal -> Site
        assert "->" in output
        # Should contain site info
        assert "Site(" in output

    def test_single_column_chain(self, initiative_graph):
        output = initiative_graph.select("Initiative").traverse("HAS_PROPOSAL").show(["id"])
        assert "Initiative(1) -> Proposal(" in output

    def test_dead_end_omitted(self, initiative_graph):
        """Roots with no traversal results are omitted."""
        output = initiative_graph.select("Initiative").traverse("HAS_PROPOSAL").show(["id"])
        # Initiative 3 has no HAS_PROPOSAL connections → not in output
        assert "Initiative(3)" not in output

    def test_chain_limit(self, initiative_graph):
        output = initiative_graph.select("Initiative").traverse("HAS_PROPOSAL").show(["id"], limit=1)
        chain_lines = [line for line in output.strip().split("\n") if "->" in line]
        assert len(chain_lines) == 1

    def test_no_results(self):
        """Traversal that produces no matching targets."""
        g = kglite.KnowledgeGraph()
        df1 = pd.DataFrame({"id": [1], "title": ["A"]})
        g.add_nodes(df1, "Source", "id", "title")
        df2 = pd.DataFrame({"id": [2], "title": ["B"]})
        g.add_nodes(df2, "Target", "id", "title")
        # Create connection type but not from our source node
        g.cypher("MATCH (t:Target {id: 2}), (s:Source {id: 1}) CREATE (t)-[:LINK]->(s)")
        # Traverse outgoing LINK from Source — no targets
        output = g.select("Source").traverse("LINK", direction="outgoing").show()
        assert "no traversal" in output.lower() or "empty" in output.lower()


class TestShowWithAliases:
    """show() should resolve field aliases."""

    def test_alias_id(self):
        g = kglite.KnowledgeGraph()
        df = pd.DataFrame({"npdid": [1, 2], "proposal_name": ["A", "B"]})
        g.add_nodes(df, "Proposal", "npdid", "proposal_name")
        output = g.select("Proposal").show(["npdid"])
        assert "Proposal(1)" in output
        assert "Proposal(2)" in output

    def test_alias_title(self):
        g = kglite.KnowledgeGraph()
        df = pd.DataFrame({"npdid": [1], "proposal_name": ["Alpha"]})
        g.add_nodes(df, "Proposal", "npdid", "proposal_name")
        output = g.select("Proposal").show(["proposal_name"])
        assert "Proposal(Alpha)" in output
