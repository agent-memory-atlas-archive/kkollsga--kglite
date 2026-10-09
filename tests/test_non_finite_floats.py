"""One rule for NaN and the infinities: lossless wherever the transport can
carry them. Python carries them as floats in both directions; JSON text
(Cypher ``parse_json``, the C ABI) spells them ``{"$float": "NaN" | "inf" | "-inf"}``.
"""

import math

import pytest

import kglite

NON_FINITE = [float("nan"), float("inf"), float("-inf")]


def _same(a, b):
    return (math.isnan(a) and math.isnan(b)) or (a == b and math.copysign(1, a) == math.copysign(1, b))


@pytest.fixture
def graph():
    return kglite.KnowledgeGraph()


@pytest.mark.parametrize("value", [*NON_FINITE, -0.0, 1.5])
def test_float_parameter_round_trips(graph, value):
    got = list(graph.cypher("RETURN $x AS x", params={"x": value}))[0]["x"]
    assert isinstance(got, float) and _same(got, value)


def test_non_finite_parameters_nest_and_persist(graph):
    got = list(graph.cypher("RETURN $x AS x", params={"x": [float("nan"), {"k": float("-inf")}]}))[0]["x"]
    assert math.isnan(got[0]) and got[1]["k"] == float("-inf")
    graph.cypher("CREATE (:F {id: 1, v: $x})", params={"x": float("inf")})
    assert list(graph.cypher("MATCH (n:F) RETURN n.v AS v"))[0]["v"] == float("inf")


@pytest.mark.parametrize(
    ("text", "check"),
    [
        ('{"$float":"NaN"}', math.isnan),
        ('{"$float":"inf"}', lambda f: f == float("inf")),
        ('{"$float":"-inf"}', lambda f: f == float("-inf")),
    ],
)
def test_parse_json_decodes_the_float_tag(graph, text, check):
    got = list(graph.cypher("RETURN parse_json($t) AS v", params={"t": text}))[0]["v"]
    assert isinstance(got, float) and check(got)


def test_parse_json_keeps_an_invalid_float_tag_as_a_map(graph):
    got = list(graph.cypher("RETURN parse_json($t) AS v", params={"t": '{"$float":"nan"}'}))[0]["v"]
    assert got == {"$float": "nan"}
