"""Tests for graph traversal and connection property filtering."""


class TestBasicTraversal:
    def test_traverse_outgoing(self, small_graph):
        alice = small_graph.select("Person").where({"title": "Alice"})
        friends = alice.traverse(connection_type="KNOWS", direction="outgoing")
        assert friends.len() == 2  # Bob and Charlie

    def test_traverse_incoming(self, small_graph):
        charlie = small_graph.select("Person").where({"title": "Charlie"})
        known_by = charlie.traverse(connection_type="KNOWS", direction="incoming")
        assert known_by.len() >= 1  # At least Alice and Bob know Charlie

    def test_traverse_cross_type(self, social_graph):
        person = social_graph.select("Person").where({"title": "Person_1"})
        companies = person.traverse(connection_type="WORKS_AT", direction="outgoing")
        assert companies.len() >= 1

    def test_traverse_with_max_nodes(self, social_graph):
        # max_nodes limits per-parent, not globally — test with a single person
        person = social_graph.select("Person").where({"title": "Person_1"})
        friends = person.traverse(connection_type="KNOWS", limit=5)
        assert friends.len() <= 5


class TestTraversalFiltering:
    def test_where_target(self, social_graph):
        people = social_graph.select("Person")
        old_friends = people.traverse(
            connection_type="KNOWS",
            where={"age": {">": 35}},
        )
        nodes = old_friends.collect()
        for n in nodes:
            assert n["age"] > 35

    def test_where_connection_properties(self, portfolio_graph):
        proposals = portfolio_graph.select("Proposal")
        high_share = proposals.traverse(
            connection_type="BECAME_INITIATIVE",
            where_connection={"share_pct": {">=": 70.0}},
        )
        assert high_share.len() > 0

    def test_where_connection_null(self, portfolio_graph):
        proposals = portfolio_graph.select("Proposal")
        with_weight = proposals.traverse(
            connection_type="HAS_ESTIMATE",
            where_connection={"weight": {"is_not_null": True}},
        )
        assert with_weight.len() > 0

    def test_sort_target(self, social_graph):
        people = social_graph.select("Person").where({"title": "Person_1"})
        friends = people.traverse(
            connection_type="KNOWS",
            sort_target="age",
        )
        nodes = friends.collect()
        if len(nodes) > 1:
            ages = [n["age"] for n in nodes]
            assert ages == sorted(ages)


class TestTraversalChaining:
    def test_multi_hop_traversal(self, portfolio_graph):
        plays = portfolio_graph.select("Portfolio")
        proposals = plays.traverse(connection_type="HAS_PROPOSAL")
        initiatives = proposals.traverse(connection_type="BECAME_INITIATIVE")
        assert initiatives.len() > 0

    def test_traverse_then_filter(self, social_graph):
        person = social_graph.select("Person").where({"title": "Person_1"})
        friends = person.traverse(connection_type="KNOWS")
        old_friends = friends.where({"age": {">": 30}})
        nodes = old_friends.collect()
        for n in nodes:
            assert n["age"] > 30
