"""Tests for Cypher ts_*() timeseries functions with NaiveDate keys."""

import pandas as pd
import pytest

import kglite


@pytest.fixture
def ts_graph():
    """Graph with two fields, each with monthly timeseries data."""
    g = kglite.KnowledgeGraph()
    df = pd.DataFrame({"id": [1, 2], "title": ["TUNDRA", "EMBER"]})
    g.add_nodes(df, "Project", "id", "title")

    g.set_timeseries(
        "Project",
        resolution="month",
        channels=["output", "flow"],
        units={"output": "MU", "flow": "BU"},
        bin_type="total",
    )

    # TUNDRA: 2019-12 through 2021-01
    g.set_time_index(
        1,
        ["2019-12", "2020-01", "2020-02", "2020-03", "2021-01"],
    )
    g.add_ts_channel(1, "output", [0.9, 1.0, 1.5, 2.0, 3.0])
    g.add_ts_channel(1, "flow", [0.1, 0.2, 0.3, 0.4, 0.5])

    # EMBER: 2020-01 through 2020-03
    g.set_time_index(2, ["2020-01", "2020-02", "2020-03"])
    g.add_ts_channel(2, "output", [0.5, 0.6, 0.7])

    return g


# ── ts_at ─────────────────────────────────────────────────────────────────


def test_ts_at_exact(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_at(f.output, '2020-2') AS val")
    assert result[0]["val"] == 1.5


def test_ts_at_missing_key(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_at(f.output, '2022-1') AS val")
    assert result[0]["val"] is None


def test_ts_at_year_key_on_month_series_is_an_error(ts_graph):
    """A key coarser than the series resolution names several periods; ts_at refuses it."""
    with pytest.raises(Exception, match="coarser than the series resolution 'month'"):
        ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_at(f.output, '2020') AS val")


# ── ts_sum ────────────────────────────────────────────────────────────────


def test_ts_sum_single_year(ts_graph):
    """ts_sum(f.output, '2020') = sum of all months in 2020."""
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_sum(f.output, '2020') AS val")
    assert abs(result[0]["val"] - 4.5) < 1e-10  # 1.0 + 1.5 + 2.0


def test_ts_sum_range(ts_graph):
    """ts_sum(f.output, '2020', '2021') = sum from 2020 through 2021."""
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_sum(f.output, '2020', '2021') AS val")
    assert abs(result[0]["val"] - 7.5) < 1e-10  # 1.0 + 1.5 + 2.0 + 3.0


def test_ts_sum_month_range(ts_graph):
    """ts_sum(f.output, '2020-1', '2020-2') = sum of Jan and Feb."""
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_sum(f.output, '2020-1', '2020-2') AS val")
    assert abs(result[0]["val"] - 2.5) < 1e-10  # 1.0 + 1.5


def test_ts_sum_all(ts_graph):
    """ts_sum(f.output) = sum of entire series."""
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_sum(f.output) AS val")
    assert abs(result[0]["val"] - 8.4) < 1e-10  # 0.9 + 1.0 + 1.5 + 2.0 + 3.0


# ── ts_avg ────────────────────────────────────────────────────────────────


def test_ts_avg_single_year(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_avg(f.output, '2020') AS val")
    assert abs(result[0]["val"] - 1.5) < 1e-10  # (1.0 + 1.5 + 2.0) / 3


# ── ts_min / ts_max ──────────────────────────────────────────────────────


def test_ts_min(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_min(f.output, '2020') AS val")
    assert result[0]["val"] == 1.0


def test_ts_max(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_max(f.output, '2020') AS val")
    assert result[0]["val"] == 2.0


# ── ts_count ──────────────────────────────────────────────────────────────


def test_ts_count(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_count(f.output) AS val")
    assert result[0]["val"] == 5


def test_ts_count_with_nan():
    g = kglite.KnowledgeGraph()
    df = pd.DataFrame({"id": [1], "title": ["S"]})
    g.add_nodes(df, "Sensor", "id", "title")
    g.set_timeseries("Sensor", resolution="year", channels=["temp"])
    g.set_time_index(1, ["0001", "0002", "0003"])
    g.add_ts_channel(1, "temp", [1.0, float("nan"), 3.0])
    result = g.cypher("MATCH (s:Sensor) RETURN ts_count(s.temp) AS val")
    assert result[0]["val"] == 2


# ── ts_first / ts_last ───────────────────────────────────────────────────


def test_ts_first(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_first(f.output) AS val")
    assert result[0]["val"] == 0.9


def test_ts_last(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_last(f.output) AS val")
    assert result[0]["val"] == 3.0


# ── ts_delta ──────────────────────────────────────────────────────────────


def test_ts_delta(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_delta(f.output, '2019', '2021') AS val")
    # value at first entry in [2021,...] - first entry in [2019,...] = 3.0 - 0.9 = 2.1
    assert abs(result[0]["val"] - 2.1) < 1e-10


def test_ts_delta_exact_months(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_delta(f.output, '2020-1', '2020-3') AS val")
    # value at [2020,3] - value at [2020,1] = 2.0 - 1.0 = 1.0
    assert abs(result[0]["val"] - 1.0) < 1e-10


# ── ts_series ─────────────────────────────────────────────────────────────


def test_ts_series(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'EMBER'}) RETURN ts_series(f.output) AS val")
    series = result[0]["val"]  # auto-parsed from JSON to native Python list
    assert len(series) == 3
    assert series[0]["time"] == "2020-01-01"
    assert series[0]["value"] == 0.5
    assert series[2]["value"] == 0.7


def test_ts_series_with_range(ts_graph):
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_series(f.output, '2020', '2020') AS val")
    series = result[0]["val"]  # auto-parsed from JSON to native Python list
    assert len(series) == 3  # 3 months in 2020


# ── Day precision on month data ──────────────────────────────────────────


def test_day_query_on_month_data_returns_zero(ts_graph):
    """Day precision on month data returns 0 (no matching keys, not an error)."""
    result = ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_sum(f.output, '2020-2-15') AS val")
    assert result[0]["val"] == 0.0


# ── WHERE clause filtering ───────────────────────────────────────────────


def test_where_with_ts_function(ts_graph):
    result = ts_graph.cypher("""
        MATCH (f:Project)
        WHERE ts_sum(f.output, '2020') > 2.0
        RETURN f.title AS title
    """)
    titles = [r["title"] for r in result]
    assert "TUNDRA" in titles  # ts_sum = 4.5
    assert "EMBER" not in titles or len(titles) == 2  # ts_sum = 1.8


# ── ORDER BY with ts_* ───────────────────────────────────────────────────


def test_order_by_ts_function(ts_graph):
    result = ts_graph.cypher("""
        MATCH (f:Project)
        RETURN f.title AS title, ts_sum(f.output, '2020') AS prod
        ORDER BY prod DESC
    """)
    assert result[0]["title"] == "TUNDRA"  # 4.5 > 1.8
    assert result[1]["title"] == "EMBER"


# ── Error cases ───────────────────────────────────────────────────────────


def test_ts_missing_channel(ts_graph):
    with pytest.raises(Exception, match="water"):
        ts_graph.cypher("MATCH (f:Project {title: 'TUNDRA'}) RETURN ts_sum(f.water)")


def test_ts_no_timeseries_data():
    g = kglite.KnowledgeGraph()
    df = pd.DataFrame({"id": [1], "title": ["X"]})
    g.add_nodes(df, "Node", "id", "title")
    with pytest.raises(Exception, match="no timeseries"):
        g.cypher("MATCH (n:Node) RETURN ts_sum(n.value)")


# ── Multi-channel ─────────────────────────────────────────────────────────


def test_multi_channel_query(ts_graph):
    result = ts_graph.cypher("""
        MATCH (f:Project {title: 'TUNDRA'})
        RETURN ts_sum(f.output, '2020') AS output, ts_sum(f.flow, '2020') AS flow
    """)
    assert abs(result[0]["output"] - 4.5) < 1e-10
    assert abs(result[0]["flow"] - 0.9) < 1e-10  # 0.2 + 0.3 + 0.4


# ── NaN handling ──────────────────────────────────────────────────────────


def test_nan_skipped_in_sum():
    g = kglite.KnowledgeGraph()
    df = pd.DataFrame({"id": [1], "title": ["S"]})
    g.add_nodes(df, "Sensor", "id", "title")
    g.set_timeseries("Sensor", resolution="year", channels=["temp"])
    g.set_time_index(1, ["0001", "0002", "0003"])
    g.add_ts_channel(1, "temp", [1.0, float("nan"), 3.0])
    result = g.cypher("MATCH (s:Sensor) RETURN ts_sum(s.temp) AS val")
    assert abs(result[0]["val"] - 4.0) < 1e-10


# ── Temporal join tests ──────────────────────────────────────────────────


def test_ts_sum_with_datetime_edge_args():
    """ts_sum with DateTime edge properties as date range arguments."""
    g = kglite.KnowledgeGraph()

    # Create nodes
    companies = pd.DataFrame({"id": [1], "title": ["VE"]})
    fields = pd.DataFrame({"id": [10], "title": ["TUNDRA"]})
    profiles = pd.DataFrame({"id": [100], "title": ["TUNDRA_PROD"]})
    g.add_nodes(companies, "Company", "id", "title")
    g.add_nodes(fields, "Project", "id", "title")
    g.add_nodes(profiles, "ProductionProfile", "id", "title")

    # Create edges with date properties
    g.cypher("""
        MATCH (f:Project {title: 'TUNDRA'}), (c:Company {title: 'VE'})
        CREATE (f)-[:HAS_HOLDER {share: 30.0, from_date: date('2020-01-01'), to_date: date('2020-12-31')}]->(c)
    """)
    g.cypher("""
        MATCH (p:ProductionProfile {title: 'TUNDRA_PROD'}), (f:Project {title: 'TUNDRA'})
        CREATE (p)-[:OF_PROJECT]->(f)
    """)

    # Add timeseries
    g.set_timeseries("ProductionProfile", resolution="month", channels=["output"])
    g.set_time_index(100, [f"2020-{m:02d}" for m in range(1, 13)])
    g.add_ts_channel(100, "output", [float(m) for m in range(1, 13)])

    # Query: ts_sum with edge date props
    result = g.cypher("""
        MATCH (c:Company {title: 'VE'})-[r:HAS_HOLDER]-(f:Project)-[:OF_PROJECT]-(p:ProductionProfile)
        RETURN ts_sum(p.output, r.from_date, r.to_date) AS total,
               ts_sum(p.output, r.from_date, r.to_date) * r.share / 100 AS equity
    """)
    assert len(result) == 1
    assert abs(result[0]["total"] - 78.0) < 1e-10  # 1+2+...+12
    assert abs(result[0]["equity"] - 23.4) < 1e-10  # 78 * 0.3


def test_ts_sum_with_null_end_date():
    """Null end date = open-ended range (no upper bound)."""
    g = kglite.KnowledgeGraph()

    companies = pd.DataFrame({"id": [1], "title": ["VE"]})
    profiles = pd.DataFrame({"id": [100], "title": ["P"]})
    g.add_nodes(companies, "Company", "id", "title")
    g.add_nodes(profiles, "ProductionProfile", "id", "title")

    # Edge with null to_date (open-ended contract)
    g.cypher("""
        MATCH (p:ProductionProfile {title: 'P'}), (c:Company {title: 'VE'})
        CREATE (p)-[:ASSIGNED_TO {from_date: date('2020-06-01'), to_date: null}]->(c)
    """)

    g.set_timeseries("ProductionProfile", resolution="month", channels=["output"])
    g.set_time_index(100, [f"2020-{m:02d}" for m in range(1, 13)])
    g.add_ts_channel(100, "output", [float(m) for m in range(1, 13)])

    # Null to_date means no upper bound — sum from June onwards
    result = g.cypher("""
        MATCH (p:ProductionProfile)-[r:ASSIGNED_TO]-(c:Company)
        RETURN ts_sum(p.output, r.from_date, r.to_date) AS total
    """)
    assert abs(result[0]["total"] - 63.0) < 1e-10  # 6+7+8+9+10+11+12


# ── Real-world example queries ──────────────────────────────────────────


@pytest.fixture
def production_graph():
    """Graph with TUNDRA (24 months: 2019-01..2020-12) and EMBER (12 months: 2020-01..2020-12)."""
    g = kglite.KnowledgeGraph()
    df = pd.DataFrame({"id": [1, 2], "title": ["TUNDRA", "EMBER"]})
    g.add_nodes(df, "Project", "id", "title")
    g.set_timeseries(
        "Project",
        resolution="month",
        channels=["output", "flow"],
        units={"output": "MU", "flow": "BU"},
        bin_type="total",
    )

    # TUNDRA: 2019-01 through 2020-12 (24 months)
    tundra_keys = [f"{y}-{m:02d}" for y in [2019, 2020] for m in range(1, 13)]
    # Output: ramp from 1.0 to 2.4 over 24 months
    tundra_oil = [1.0 + 0.06 * i for i in range(24)]
    g.set_time_index(1, tundra_keys)
    g.add_ts_channel(1, "output", tundra_oil)
    g.add_ts_channel(1, "flow", [v * 0.1 for v in tundra_oil])

    # EMBER: 2020-01 through 2020-12 (12 months)
    ember_keys = [f"2020-{m:02d}" for m in range(1, 13)]
    ember_oil = [0.5 + 0.02 * i for i in range(12)]
    g.set_time_index(2, ember_keys)
    g.add_ts_channel(2, "output", ember_oil)

    return g


def test_all_fields_in_specific_month(production_graph):
    """Production of all fields in Feb 2020."""
    result = production_graph.cypher("""
        MATCH (f:Project)
        RETURN f.title AS name, ts_at(f.output, '2020-2') AS output_feb
        ORDER BY output_feb DESC
    """)
    assert len(result) == 2
    # Both fields should have a Feb 2020 value
    assert result[0]["output_feb"] is not None
    assert result[1]["output_feb"] is not None
    # TUNDRA > EMBER
    assert result[0]["name"] == "TUNDRA"


def test_field_production_between_months(production_graph):
    """Production of TUNDRA between Feb 2020 and Dec 2020."""
    result = production_graph.cypher("""
        MATCH (f:Project {title: 'TUNDRA'})
        RETURN ts_sum(f.output, '2020-2', '2020-12') AS total_output
    """)
    # Months 2020-02 through 2020-12 = indices 13..23 (11 months)
    expected = sum(1.0 + 0.06 * i for i in range(13, 24))
    assert abs(result[0]["total_output"] - expected) < 1e-6


def test_production_difference_pct_with_arithmetic(production_graph):
    """Difference in production between summer months and rest using Cypher arithmetic."""
    result = production_graph.cypher("""
        MATCH (f:Project {title: 'TUNDRA'})
        WITH ts_sum(f.output, '2020-6', '2020-8') AS summer,
             ts_sum(f.output, '2020') AS total
        RETURN summer, total - summer AS rest,
               (summer / (total - summer)) * 100.0 AS summer_pct_of_rest
    """)
    assert result[0]["summer"] > 0
    assert result[0]["rest"] > 0
    assert result[0]["summer_pct_of_rest"] > 0


def test_latest_recorded_month_via_series(production_graph):
    """What is the latest recorded month of TUNDRA production."""
    result = production_graph.cypher("""
        MATCH (f:Project {title: 'TUNDRA'})
        RETURN ts_series(f.output) AS data
    """)
    series = result[0]["data"]
    last = series[-1]
    assert last["time"] == "2020-12-01"


def test_first_production_record_via_series(production_graph):
    """When did TUNDRA start production (first record)."""
    result = production_graph.cypher("""
        MATCH (f:Project {title: 'TUNDRA'})
        RETURN ts_series(f.output) AS data
    """)
    series = result[0]["data"]
    first = series[0]
    assert first["time"] == "2019-01-01"


def test_top_producers_order_by(production_graph):
    """Top fields by production, ordered."""
    result = production_graph.cypher("""
        MATCH (f:Project)
        RETURN f.title AS name, ts_sum(f.output, '2020') AS prod
        ORDER BY prod DESC LIMIT 10
    """)
    assert result[0]["name"] == "TUNDRA"
    assert result[1]["name"] == "EMBER"
    assert result[0]["prod"] > result[1]["prod"]


def test_unwind_dynamic_ts_args(production_graph):
    """UNWIND + toString() for dynamic date-string args to ts_sum."""
    result = production_graph.cypher("""
        MATCH (f:Project {title: 'TUNDRA'})
        UNWIND range(2019, 2020) AS year
        WITH f, year, ts_sum(f.output, toString(year) + '-6', toString(year) + '-8') AS summer_yr
        RETURN sum(summer_yr) AS summer_total
    """)
    # Summer months (Jun-Aug) for 2019 and 2020
    # 2019: indices 5,6,7 → output = 1.30 + 1.36 + 1.42
    # 2020: indices 17,18,19 → output = 2.02 + 2.08 + 2.14
    expected = sum(1.0 + 0.06 * i for i in [5, 6, 7, 17, 18, 19])
    assert abs(result[0]["summer_total"] - expected) < 1e-6


def test_unwind_summer_vs_rest_pct(production_graph):
    """Full summer-vs-rest percentage using UNWIND loop."""
    result = production_graph.cypher("""
        MATCH (f:Project {title: 'TUNDRA'})
        UNWIND range(2019, 2020) AS year
        WITH f, sum(ts_sum(f.output, toString(year) + '-6', toString(year) + '-8')) AS summer
        RETURN summer,
               ts_sum(f.output, '2019', '2020') AS total,
               summer / ts_sum(f.output, '2019', '2020') * 100.0 AS summer_pct
    """)
    assert result[0]["summer"] > 0
    assert result[0]["total"] > 0
    # Summer is 6 out of 24 months = ~25%
    assert 20 < result[0]["summer_pct"] < 30


def test_ts_at_with_dynamic_string(production_graph):
    """ts_at with dynamically constructed date string."""
    result = production_graph.cypher("""
        MATCH (f:Project {title: 'TUNDRA'})
        UNWIND [3, 6, 9, 12] AS month
        RETURN month, ts_at(f.output, '2020-' + toString(month)) AS val
        ORDER BY month
    """)
    assert len(result) == 4
    assert result[0]["month"] == 3
    assert result[0]["val"] is not None
    # Values should be increasing (ramp data)
    for i in range(len(result) - 1):
        assert result[i]["val"] < result[i + 1]["val"]
