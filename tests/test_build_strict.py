"""Strict mode: a build that raises an advisory in a strict group fails after
the full report is assembled, and nothing is saved."""

import json
import warnings

import pytest

from kglite import from_blueprint

DECLARATION_PROBLEM = {
    "Department": {
        "csv": "d.csv",
        "pk": "did",
        "title": "name",
        "properties": {"vf": "validFrom", "vt": "validTo"},
    }
}
STUB_PROBLEM = {
    "Team": {"csv": "t.csv", "pk": "tid", "title": "name", "properties": {}},
    "Person": {
        "csv": "p.csv",
        "pk": "pid",
        "title": "name",
        "properties": {},
        "connections": {"fk_edges": {"MEMBER_OF": {"target": "Team", "fk": "team"}}},
    },
}
QUALITY_PROBLEM = {"Team": {"csv": "t2.csv", "pk": "tid", "title": "name", "properties": {}}}
CSVS = {
    "d.csv": "did,name,vf,vt\n1,Ops,2020-01-01,2021-01-01\n",
    "t.csv": "tid,name\nt1,Core\n",
    "p.csv": "pid,name,team\n1,Ann,t1\n2,Bo,t9\n",
    "t2.csv": "tid,name\nt1,Core\nt1,Core again\n",
}


def _blueprint(tmp_path, nodes, **settings):
    for name, text in CSVS.items():
        (tmp_path / name).write_text(text, encoding="utf-8")
    settings = {"root": str(tmp_path), "output": str(tmp_path / "out.kgl"), **settings}
    path = tmp_path / "bp.json"
    path.write_text(json.dumps({"settings": settings, "nodes": nodes}), encoding="utf-8")
    return path


def _build(path, **kw):
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        return from_blueprint(path, **kw)


@pytest.mark.parametrize(
    "nodes, group",
    [(DECLARATION_PROBLEM, "declarations"), (STUB_PROBLEM, "stubs")],
)
def test_strict_true_fails_and_writes_nothing(tmp_path, nodes, group):
    path = _blueprint(tmp_path, nodes)
    with pytest.raises(ValueError, match=r"strict build failed") as err:
        _build(path, strict=True, save=True)
    assert f"[{group}] 1 advisory" in str(err.value)
    assert not (tmp_path / "out.kgl").exists()


def test_the_message_carries_counts_and_items_of_failing_groups_only(tmp_path):
    path = _blueprint(tmp_path, {**DECLARATION_PROBLEM, **STUB_PROBLEM, **{}})
    with pytest.raises(ValueError) as err:
        _build(path, strict=True)
    text = str(err.value)
    assert "[declarations]" in text and "[stubs]" in text
    assert "t9" in text or "Team" in text
    assert "[data_quality]" not in text


def test_a_named_group_fails_only_for_that_group(tmp_path):
    path = _blueprint(tmp_path, QUALITY_PROBLEM)
    graph = _build(path, strict=True)  # data_quality is not in the default set
    assert graph is not None
    with pytest.raises(ValueError, match=r"\[data_quality\]"):
        _build(path, strict=["data_quality"])
    stubs = _blueprint(tmp_path, STUB_PROBLEM)
    _build(stubs, strict=["data_quality"])  # a stubs problem passes


@pytest.mark.parametrize("strict", [None, False])
def test_off_never_raises(tmp_path, strict):
    path = _blueprint(tmp_path, {**DECLARATION_PROBLEM, **STUB_PROBLEM})
    _build(path, strict=strict)
    assert (tmp_path / "out.kgl").exists()


def test_the_setting_is_honoured_and_the_argument_overrides_it(tmp_path):
    path = _blueprint(tmp_path, STUB_PROBLEM, strict=True)
    with pytest.raises(ValueError, match=r"\[stubs\]"):
        _build(path)
    with pytest.raises(ValueError, match=r"\[stubs\]"):
        _build(path, strict=None)
    _build(path, strict=False)
    _build(path, strict=["data_quality"])
    listed = _blueprint(tmp_path, STUB_PROBLEM, strict=["stubs"])
    with pytest.raises(ValueError, match=r"\[stubs\]"):
        _build(listed)


def test_unknown_group_names_are_an_error(tmp_path):
    path = _blueprint(tmp_path, STUB_PROBLEM)
    with pytest.raises(ValueError, match=r"unknown diagnostic group 'stub'.*data_shape"):
        _build(path, strict=["stub"])
    bad = _blueprint(tmp_path, STUB_PROBLEM, strict=["nope"])
    with pytest.raises(ValueError, match=r"unknown diagnostic group 'nope'"):
        _build(bad)
    with pytest.raises(TypeError):
        _build(path, strict="stubs")


def test_a_clean_build_passes_strict(tmp_path):
    nodes = {"Team": {"csv": "t.csv", "pk": "tid", "title": "name", "properties": {}}}
    path = _blueprint(tmp_path, nodes)
    _build(path, strict=True, save=True)
    assert (tmp_path / "out.kgl").exists()
