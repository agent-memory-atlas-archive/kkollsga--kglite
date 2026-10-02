"""Random values survive the CSV cell grammar and the RDF literal grammar.

Each example is one graph of nodes carrying every scalar kind (control
characters, non-finite floats, timestamps across the supported range, mixed-sign
durations, unicode, commas, quotes, newlines, backslash-`e` text) plus a list
holding typed values. The graph is exported and re-imported, and every
property must come back equal. The example count is bounded to keep the file
near two seconds; counterexamples persist in `.hypothesis/`.
"""

import datetime as dt
import math
import os
import tempfile

from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st
import pytest

import kglite
from kglite import KnowledgeGraph

# Lone surrogates cannot be encoded in UTF-8 files; NUL is excluded because
# no text format carries it.
TEXT = st.text(
    alphabet=st.characters(blacklist_categories=("Cs",), blacklist_characters="\x00"),
    max_size=12,
) | st.sampled_from(
    ["", "\\", "\\e", "\\\\e", "a,b", '"q"', "line\nbreak", "cr\r\nlf", " x ", "\t", "\x1b[0m", "$date"]
)

DATES = st.dates(min_value=dt.date(1, 1, 1), max_value=dt.date(9999, 12, 31))
TIMESTAMPS = st.datetimes(min_value=dt.datetime(1, 1, 1), max_value=dt.datetime(9999, 12, 31, 23, 59, 59, 999999))
FLOATS = st.floats(allow_nan=True, allow_infinity=True)

ROW = st.fixed_dictionaries(
    {
        "i": st.integers(min_value=-(2**63), max_value=2**63 - 1),
        "f": FLOATS,
        "b": st.booleans(),
        "s": TEXT,
        "d": DATES,
        "ts": TIMESTAMPS,
        "months": st.integers(-1000, 1000),
        "days": st.integers(-1000, 1000),
        "secs": st.integers(-(10**9), 10**9),
        "lat": st.floats(-90, 90),
        "lon": st.floats(-180, 180),
        "lf": FLOATS,
        "ld": DATES,
        "lt": TEXT,
    }
)

CREATE = (
    "CREATE (:Person {id: $n, title: $s, i: $i, f: $f, b: $b, s: $s, d: date($d), ts: datetime($ts), "
    "du: duration({months: $months, days: $days, seconds: $secs}), "
    "pt: point({latitude: $lat, longitude: $lon}), "
    "l: [$lf, date($ld), $lt, duration({days: $days}), $i], "
    "m: {k: $lt, at: datetime($ts), nested: [$lf]}})"
)


def _norm(value):
    if isinstance(value, float):
        return ("float", "nan" if math.isnan(value) else repr(value))
    if isinstance(value, dict):
        return {k: _norm(v) for k, v in sorted(value.items())}
    if isinstance(value, (list, tuple)):
        return [_norm(v) for v in value]
    return (type(value).__name__, value)


def _dump(g):
    return [
        _norm(r["p"]) for r in g.cypher("MATCH (n:Person) RETURN n.id AS id, properties(n) AS p ORDER BY id").to_list()
    ]


def _graph(rows):
    g = KnowledgeGraph()
    for n, row in enumerate(rows):
        params = dict(row, n=n, d=row["d"].isoformat(), ld=row["ld"].isoformat(), ts=row["ts"].isoformat())
        g.cypher(CREATE, params=params)
    return g


GRAMMAR = settings(max_examples=60, deadline=None, suppress_health_check=[HealthCheck.too_slow])


@given(rows=st.lists(ROW, min_size=1, max_size=4))
@GRAMMAR
def test_csv_cell_grammar_is_the_identity(rows):
    g = _graph(rows)
    with tempfile.TemporaryDirectory() as tmp:
        g.export_csv(tmp)
        back = kglite.from_blueprint(os.path.join(tmp, "blueprint.json"), save=False)
    assert _dump(back) == _dump(g)


@pytest.mark.parametrize("fmt", ["nq", "trig"])
def test_rdf_literal_grammar_is_the_identity(fmt):
    @given(rows=st.lists(ROW, min_size=1, max_size=4))
    @GRAMMAR
    def check(rows):
        g = _graph(rows)
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, f"g.{fmt}")
            g.export_rdf(path, format=fmt)
            back = kglite.load_rdf(path)
        assert _dump(back) == _dump(g)

    check()
