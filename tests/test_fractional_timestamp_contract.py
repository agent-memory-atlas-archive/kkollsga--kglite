"""Fractions survive supported output formats and Python's microsecond boundary."""

import csv
import datetime as dt
import io
import json
import subprocess
import sys

import pytest

import kglite

UTC = dt.timezone.utc


@pytest.mark.parametrize("kind", ["datetime", "localdatetime"])
@pytest.mark.parametrize("consumer", ["list", "scalar", "frame", "direct_frame", "csv", "string", "concat"])
def test_timestamp_fraction_in_every_public_consumer(kind, consumer):
    """The fraction survives every output for the local kind and the zoned one;
    the zoned kind spells its offset (`Z`) and comes back aware."""
    graph = kglite.KnowledgeGraph()
    expression = f"{kind}('2025-01-02T03:04:05.123456')"
    zoned = kind == "datetime"
    expected = dt.datetime(2025, 1, 2, 3, 4, 5, 123456, tzinfo=UTC if zoned else None)
    text = expected.replace(tzinfo=None).isoformat() + ("Z" if zoned else "")
    if consumer == "csv":
        output = graph.cypher(f"RETURN {expression} AS t FORMAT CSV")
        assert list(csv.DictReader(io.StringIO(output))) == [{"t": text}]
    elif consumer in {"string", "concat"}:
        wrapped = f"toString({expression})" if consumer == "string" else f"'at:' || {expression}"
        prefix = "at:" if consumer == "concat" else ""
        assert graph.cypher(f"RETURN {wrapped} AS t").scalar() == prefix + text
    elif consumer == "direct_frame":
        assert graph.cypher(f"RETURN {expression} AS t", to_df=True).to_dict("records") == [{"t": expected}]
    else:
        result = graph.cypher(f"RETURN {expression} AS t")
        if consumer == "scalar":
            assert result.scalar() == expected
        elif consumer == "frame":
            assert result.to_df().to_dict("records") == [{"t": expected}]
        else:
            assert result.to_list() == [{"t": expected}]


def test_cli_json_preserves_native_nanoseconds_and_whole_second_spelling(tmp_path):
    path = tmp_path / "timestamp.kgl"
    kglite.KnowledgeGraph().save(str(path))
    process = subprocess.run(
        [
            sys.executable,
            "-m",
            "kglite.cli",
            "query",
            str(path),
            "RETURN localdatetime('2025-01-02T03:04:05.123456789') AS fraction, "
            "localdatetime('2025-01-02T03:04:05') AS whole, "
            "datetime('2025-01-02T03:04:05.123456789+01:00') AS zoned",
            "--format",
            "json",
        ],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    assert process.returncode == 0, process.stderr
    assert json.loads(process.stdout) == [
        {
            "fraction": "2025-01-02T03:04:05.123456789",
            "whole": "2025-01-02T03:04:05",
            "zoned": "2025-01-02T03:04:05.123456789+01:00",
        }
    ]


@pytest.mark.parametrize("kind,suffix", [("localdatetime", ""), ("datetime", "Z")])
@pytest.mark.parametrize("text", ["2025-01-02T03:04:05", "2025-01-02T03:04:05.123456789"])
@pytest.mark.parametrize("operator", ["toString", "concat"])
def test_nested_semantic_timestamp_text_is_exact(kind, suffix, text, operator):
    graph = kglite.KnowledgeGraph()
    expression = f"[{{t:{kind}('{text}')}}]"
    wrapped = f"toString({expression})" if operator == "toString" else f"'at:' || {expression}"
    prefix = "at:" if operator == "concat" else ""
    assert graph.cypher(f"RETURN {wrapped} AS t").scalar() == prefix + '[{t: "' + text + suffix + '"}]'


def test_nested_query_csv_keeps_timestamp_fraction():
    graph = kglite.KnowledgeGraph()
    output = graph.cypher(
        "RETURN [localdatetime('2025-01-02T03:04:05.123456789')] AS t, "
        "[datetime('2025-01-02T03:04:05.123456789Z')] AS z FORMAT CSV"
    )
    assert list(csv.DictReader(io.StringIO(output))) == [
        {"t": '["2025-01-02T03:04:05.123456789"]', "z": '["2025-01-02T03:04:05.123456789Z"]'}
    ]
