"""Blueprint build advisories are classified once, in the core, and reach the
caller grouped by severity.

``BuildReport.diagnostics`` carries ``{group, kind, message}`` per advisory;
Python raises one ``UserWarning`` per non-empty group (most severe first) and
``graph_info()['build']`` keeps the summary, saved with the graph.
"""

import json
import warnings

import pytest

import kglite
from kglite import from_blueprint, from_records

GROUPS = ["declarations", "stubs", "data_shape", "data_quality", "cosmetic"]


def _write(tmp_path, nodes, csvs):
    for name, text in csvs.items():
        (tmp_path / name).write_text(text, encoding="utf-8")
    bp = {"settings": {"root": str(tmp_path)}, "nodes": nodes}
    path = tmp_path / "bp.json"
    path.write_text(json.dumps(bp), encoding="utf-8")
    return path


def _build(blueprint, **kw):
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        graph = from_blueprint(blueprint, save=False, **kw)
    return graph, [str(w.message) for w in caught if issubclass(w.category, UserWarning)]


def _all_groups(tmp_path, monkeypatch):
    """One advisory (or more) in every group: a typed-only validity pair
    (declarations), a person naming a team that is not loaded (stubs), a stray
    key (data_shape), a repeated id (data_quality) and an untyped column in an
    input read in several chunks (cosmetic)."""
    monkeypatch.setenv("KGLITE_BLUEPRINT_STREAMING_THRESHOLD_MB", "0")
    monkeypatch.setenv("KGLITE_BLUEPRINT_NODE_CHUNK_SIZE", "2")
    nodes = {
        "Department": {
            "csv": "d.csv",
            "pk": "did",
            "title": "name",
            "properties": {"vf": "validFrom", "vt": "validTo"},
        },
        "Team": {"csv": "t.csv", "pk": "tid", "title": "name", "properties": {}},
        "Person": {
            "csv": "p.csv",
            "pk": "pid",
            "title": "name",
            "properties": {},
            "lables": ["Human"],
            "connections": {"fk_edges": {"MEMBER_OF": {"target": "Team", "fk": "team"}}},
        },
    }
    csvs = {
        "d.csv": "did,name,vf,vt\n1,Ops,2020-01-01,2021-01-01\n",
        "t.csv": "tid,name\nt1,Core\n",
        "p.csv": "pid,name,team,age\n1,Ann,t1,30\n1,Ann again,t1,31\n2,Bo,t9,32\n3,Cy,t1,33\n",
    }
    return _write(tmp_path, nodes, csvs)


def test_one_warning_per_non_empty_group_most_severe_first(tmp_path, monkeypatch):
    _, messages = _build(_all_groups(tmp_path, monkeypatch))
    tagged = [m for m in messages if m.startswith("from_blueprint [")]
    assert [m.split("[")[1].split("]")[0] for m in tagged] == GROUPS, messages
    assert len(tagged) == len(messages), "every build warning is a group warning"
    # The typed-only validity pair is the first thing the caller reads.
    assert "no validity interval is declared" in messages[0]
    assert "[declarations] 1 warning(s):" in messages[0]


def test_summary_counts_every_group(tmp_path, monkeypatch):
    graph, _ = _build(_all_groups(tmp_path, monkeypatch))
    build = graph.graph_info()["build"]
    assert build["summary"] == {
        "declarations": 1,
        "stubs": 1,
        "data_shape": 1,
        "data_quality": 1,
        "cosmetic": 1,
    }
    kinds = {d["kind"]: d["group"] for d in build["diagnostics"]}
    assert kinds == {
        "typed_only_no_validity": "declarations",
        "stubs_vivified": "stubs",
        "unknown_key": "data_shape",
        "duplicate_id": "data_quality",
        "undeclared_types_extra_read": "cosmetic",
    }
    assert [d["group"] for d in build["diagnostics"]] == GROUPS


def test_build_survives_save_and_load(tmp_path, monkeypatch):
    graph, _ = _build(_all_groups(tmp_path, monkeypatch))
    saved = tmp_path / "g.kgl"
    graph.save(str(saved))
    loaded = kglite.load(str(saved))
    assert loaded.graph_info()["build"] == graph.graph_info()["build"]


@pytest.mark.parametrize("storage", ["mapped", "disk"])
def test_build_is_recorded_in_every_storage_mode(tmp_path, monkeypatch, storage):
    kw = {"storage": storage}
    if storage == "disk":
        kw["path"] = str(tmp_path / "disk_graph")
    graph, _ = _build(_all_groups(tmp_path, monkeypatch), **kw)
    live = graph.graph_info()["build"]
    assert live["summary"]["declarations"] == 1
    saved = tmp_path / f"{storage}.kgl"
    graph.save(str(saved))
    assert kglite.load(str(saved)).graph_info()["build"] == live


def test_a_clean_build_records_an_empty_summary_and_warns_nothing(tmp_path):
    nodes = {"Person": {"csv": "p.csv", "pk": "pid", "title": "name", "properties": {"age": "int"}}}
    graph, messages = _build(_write(tmp_path, nodes, {"p.csv": "pid,name,age\n1,Ann,30\n2,Bo,31\n"}))
    assert messages == []
    assert graph.graph_info()["build"] == {"summary": {}, "diagnostics": []}


def test_a_graph_not_built_from_a_blueprint_has_no_build_key(tmp_path):
    g = kglite.KnowledgeGraph()
    assert "build" not in g.graph_info()
    saved = tmp_path / "plain.kgl"
    g.save(str(saved))
    assert "build" not in kglite.load(str(saved)).graph_info()


def test_a_long_group_lists_ten_items_and_counts_the_rest(tmp_path):
    nodes = {
        f"Kind{i}": {"csv": "k.csv", "pk": "id", "title": "name", "properties": {}, "lables": ["X"]} for i in range(12)
    }
    graph, messages = _build(_write(tmp_path, nodes, {"k.csv": "id,name\n1,a\n"}))
    shape = [m for m in messages if "[data_shape]" in m]
    assert len(shape) == 1, messages
    assert "12 warning(s)" in shape[0]
    assert shape[0].count("\n  - ") == 10
    assert shape[0].endswith("… and 2 more")
    assert graph.graph_info()["build"]["summary"] == {"data_shape": 12}


def test_warnings_are_attributed_to_the_caller(tmp_path, monkeypatch):
    path = _all_groups(tmp_path, monkeypatch)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        from_blueprint(path, save=False)
    assert caught and all(w.filename == __file__ for w in caught), [w.filename for w in caught]


def test_from_records_groups_its_warnings_too():
    spec = {
        "nodes": [
            {
                "type": "Person",
                "id_field": "pid",
                "title_field": "name",
                "records": [
                    {"pid": 1, "name": "Ann"},
                    {"pid": 1, "name": "Ann again"},
                ],
            }
        ]
    }
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        from_records(spec)
    messages = [str(w.message) for w in caught]
    assert len(messages) == 1, messages
    assert messages[0].startswith("from_records [data_quality] 1 warning(s):")
