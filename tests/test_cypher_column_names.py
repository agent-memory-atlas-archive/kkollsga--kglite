"""An unaliased RETURN item is named by the text the query wrote.

``RETURN toInteger('3'), 'a', 1+2`` names its columns ``toInteger('3')``,
``'a'`` and ``1+2`` — the source text of each item, trimmed, with the
author's casing, quotes and spacing — not a re-rendering of the parsed
expression. ORDER BY, HAVING, grouping and UNION arm matching are unaffected:
they resolve on the internal name, so every query that ran before still runs.
"""

import pytest

from kglite import KnowledgeGraph


@pytest.fixture
def people():
    g = KnowledgeGraph()
    g.cypher(
        "CREATE (:P {id: 1, name: 'ann', age: 30}), (:P {id: 2, name: 'bob', age: 20}), "
        "(:P {id: 3, name: 'cy', age: 30})"
    )
    return g


def cols(g, query, **params):
    return list(g.cypher(query, params=params).columns)


def test_mixed_scalar_items_keep_their_written_form(people):
    result = people.cypher("RETURN toInteger('3'), size([1,2]), 'a', 1+2")
    assert list(result.columns) == ["toInteger('3')", "size([1,2])", "'a'", "1+2"]
    assert result.to_list() == [{"toInteger('3')": 3, "size([1,2])": 2, "'a'": "a", "1+2": 3}]


def test_whitespace_and_case_are_taken_verbatim(people):
    assert cols(people, "RETURN   size( [1,2] )  ,TOUPPER( 'x' )") == ["size( [1,2] )", "TOUPPER( 'x' )"]


def test_multiline_expression_is_taken_verbatim(people):
    assert cols(people, "RETURN 1 +\n   2, 5") == ["1 +\n   2", "5"]


def test_parameter_items(people):
    assert cols(people, "RETURN -5, $x, $(x)+2, $x+1", x=4) == ["-5", "$x", "$(x)+2", "$x+1"]
    assert people.cypher("RETURN $x+1", params={"x": 4}).to_list() == [{"$x+1": 5}]


def test_nested_calls_and_property_access(people):
    got = people.cypher("MATCH (n:P) WHERE n.id = 1 RETURN toString(toInteger('7')), n.name, n . age").to_list()
    assert got == [{"toString(toInteger('7'))": "7", "n.name": "ann", "n . age": 30}]


def test_aggregate_spelling_is_kept(people):
    assert people.cypher("MATCH (n:P) RETURN COUNT(*), Count(n)").to_list() == [{"COUNT(*)": 3, "Count(n)": 3}]
    assert people.cypher("MATCH (n:P) RETURN count(n)").to_list() == [{"count(n)": 3}]


def test_alias_still_wins(people):
    assert cols(people, "RETURN toInteger('3') AS x, toInteger('4')") == ["x", "toInteger('4')"]


def test_order_by_resolves_on_the_expression_not_the_column_name(people):
    q = "MATCH (n:P) RETURN toUpper(n.name), n.age+1 ORDER BY toUpper(n.name) DESC"
    result = people.cypher(q)
    assert list(result.columns) == ["toUpper(n.name)", "n.age+1"]
    assert result.to_list() == [
        {"toUpper(n.name)": "CY", "n.age+1": 31},
        {"toUpper(n.name)": "BOB", "n.age+1": 21},
        {"toUpper(n.name)": "ANN", "n.age+1": 31},
    ]
    # Another spelling of the same expression resolves to the same column.
    assert [
        r["toUpper(n.name)"] for r in people.cypher(q.replace("toUpper(n.name) DESC", "TOUPPER(n.name)")).to_list()
    ] == [
        "ANN",
        "BOB",
        "CY",
    ]


def test_grouped_aggregate_with_order_by_and_having(people):
    q = "MATCH (n:P) RETURN n.age, COUNT(n) ORDER BY COUNT(n) DESC, n.age"
    assert people.cypher(q).to_list() == [{"n.age": 30, "COUNT(n)": 2}, {"n.age": 20, "COUNT(n)": 1}]
    q = "MATCH (n:P) RETURN toUpper(n.name), n.age+0, count(*) HAVING count(*) > 0 ORDER BY toUpper(n.name)"
    assert [list(r) for r in people.cypher(q).to_list()][:1] == [["toUpper(n.name)", "n.age+0", "count(*)"]]


def test_distinct_and_dataframe_columns(people):
    result = people.cypher("MATCH (n:P) RETURN DISTINCT n.age*2")
    assert list(result.columns) == ["n.age*2"]
    assert sorted(r["n.age*2"] for r in result.to_list()) == [40, 60]
    assert list(people.cypher("MATCH (n:P) RETURN n.age*2, toLower('X')").to_df().columns) == [
        "n.age*2",
        "toLower('X')",
    ]


def test_with_items_keep_their_names_for_later_clauses(people):
    result = people.cypher("MATCH (n:P) WITH n.name AS nm, n.age*2 AS a2 RETURN nm, a2+1 ORDER BY nm")
    assert list(result.columns) == ["nm", "a2+1"]
    assert result.to_list()[0] == {"nm": "ann", "a2+1": 61}


def test_union_names_come_from_the_left_arm(people):
    assert cols(people, "RETURN toInteger('3') UNION RETURN toInteger('3')") == ["toInteger('3')"]
    # Arms match on the internal name, so respelling a function still unions.
    result = people.cypher("RETURN toInteger('3') UNION RETURN tointeger('3')")
    assert result.to_list() == [{"toInteger('3')": 3}]


def test_union_still_rejects_different_columns(people):
    with pytest.raises(Exception, match="same return column names"):
        people.cypher("RETURN 1+2 UNION RETURN 3+4")


def test_subquery_body_column_is_referenced_by_its_alias(people):
    result = people.cypher("CALL { RETURN 1+2 AS v } RETURN v, v+1")
    assert list(result.columns) == ["v", "v+1"]


def test_duplicate_unaliased_columns_are_still_rejected(people):
    with pytest.raises(Exception, match="Multiple result columns"):
        people.cypher("MATCH (n:P) RETURN n.age, n.age")


def test_write_query_return_uses_written_form(people):
    result = people.cypher("CREATE (n:Q {id: 9}) RETURN n.id+1, toInteger('3')")
    assert result.to_list() == [{"n.id+1": 10, "toInteger('3')": 3}]


def test_return_star_with_extra_item(people):
    result = people.cypher("MATCH (n:P) WHERE n.id = 1 WITH n.name AS nm RETURN *, 1+1")
    assert result.to_list() == [{"nm": "ann", "1+1": 2}]


def test_comment_between_item_and_next_token_is_not_part_of_the_name(people):
    assert cols(people, "RETURN 1 + 2 // trailing\n, 'a//b' // another\n, 3") == ["1 + 2", "'a//b'", "3"]
