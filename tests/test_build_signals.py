"""Build-time signals reach the caller once, in the right place.

The duplicate-id warning used to be capped for the whole process and fired on
versioned rows of a type the blueprint declares temporal; `from_records`
dropped its report; `verbose` wrote to file descriptor 1 and counted twice;
the stub advisory never knew a label was about to be declared; and a blueprint
warning was attributed to the library's own shim.
"""

import contextlib
import io
import json
import warnings

from kglite import from_blueprint, from_records


def _write(tmp_path, nodes, csvs):
    for name, text in csvs.items():
        (tmp_path / name).write_text(text, encoding="utf-8")
    bp = {"settings": {"root": str(tmp_path)}, "nodes": nodes}
    path = tmp_path / "bp.json"
    path.write_text(json.dumps(bp), encoding="utf-8")
    return path


def _build(path, **kw):
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g = from_blueprint(path, save=False, **kw)
    return g, caught


def _dup_nodes():
    return {"Person": {"csv": "p.csv", "pk": "pid", "title": "name", "properties": {}}}


DUP_CSV = {"p.csv": "pid,name\n1,Ann\n1,Ann again\n2,Bo\n"}


def test_duplicate_id_warning_is_not_capped_across_builds(tmp_path):
    for i in range(8):
        d = tmp_path / f"b{i}"
        d.mkdir()
        _, caught = _build(_write(d, _dup_nodes(), DUP_CSV))
        msgs = [str(w.message) for w in caught if "duplicate id" in str(w.message)]
        assert len(msgs) == 1, (i, [str(w.message) for w in caught])


def _versioned_nodes():
    return {
        "Department": {
            "csv": "d.csv",
            "pk": "did",
            "title": "name",
            "properties": {"vf": "date", "vt": "date"},
            "temporal": {"from": "vf", "to": "vt", "convention": "closed"},
        },
        "Person": {"csv": "p.csv", "pk": "pid", "title": "name", "properties": {}},
    }


def test_versioned_rows_of_a_declared_type_do_not_warn_but_undeclared_duplicates_do(tmp_path):
    csvs = dict(DUP_CSV)
    csvs["d.csv"] = "did,name,vf,vt\n1,Sales,2010-01-01,2014-12-31\n1,Sales,2015-01-01,\n"
    _, caught = _build(_write(tmp_path, _versioned_nodes(), csvs))
    dup = [str(w.message) for w in caught if "duplicate id" in str(w.message)]
    assert len(dup) == 1 and "'Person'" in dup[0], dup
    assert not any("Department" in m for m in dup)


def test_from_records_warnings_are_emitted(tmp_path):
    spec = {
        "nodes": [
            {
                "type": "Person",
                "id_field": "id",
                "title_field": "name",
                "records": [{"id": 1, "name": "A"}, {"id": 1, "name": "B"}, {"id": 2, "name": "C"}],
            }
        ]
    }
    for _ in range(7):
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            from_records(spec)
        dup = [w for w in caught if "duplicate id" in str(w.message)]
        assert len(dup) == 1, [str(w.message) for w in caught]
        assert dup[0].filename == __file__


def test_verbose_output_is_capturable_and_counts_once(tmp_path):
    csvs = dict(DUP_CSV)
    path = _write(tmp_path, _dup_nodes(), csvs)
    buf = io.StringIO()
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        with contextlib.redirect_stdout(buf):
            from_blueprint(path, save=False, verbose=True)
    out = buf.getvalue()
    assert out.count("Person: ") == 1, out
    assert out.count("Loaded 3 nodes") == 1, out


def test_verbose_output_reaches_real_stdout_only_through_python(tmp_path, capfd):
    path = _write(tmp_path, _dup_nodes(), DUP_CSV)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        from_blueprint(path, save=False, verbose=True)
    out = capfd.readouterr().out
    assert out.count("Loaded 3 nodes") == 1, out


def test_stub_advisory_of_a_declared_label_says_the_stub_is_valid_at_every_instant(tmp_path):
    nodes = _versioned_nodes()
    nodes["Person"]["connections"] = {"fk_edges": {"IN_DEPT": {"target": "Department", "fk": "did"}}}
    csvs = {
        "d.csv": "did,name,vf,vt\n1,Sales,2010-01-01,\n",
        "p.csv": "pid,name,did\n1,Ann,1\n2,Bo,99\n",
    }
    _, caught = _build(_write(tmp_path, nodes, csvs))
    msgs = [str(w.message) for w in caught if "stub node" in str(w.message)]
    assert len(msgs) == 1, [str(w.message) for w in caught]
    assert "declared label 'Department'" in msgs[0] and "valid at every instant" in msgs[0], msgs


def test_stub_advisory_of_an_undeclared_label_stays_plain(tmp_path):
    nodes = {
        "Department": {"csv": "d.csv", "pk": "did", "title": "name", "properties": {}},
        "Person": {
            "csv": "p.csv",
            "pk": "pid",
            "title": "name",
            "properties": {},
            "connections": {"fk_edges": {"IN_DEPT": {"target": "Department", "fk": "did"}}},
        },
    }
    csvs = {"d.csv": "did,name\n1,Sales\n", "p.csv": "pid,name,did\n1,Ann,1\n2,Bo,99\n"}
    _, caught = _build(_write(tmp_path, nodes, csvs))
    msgs = [str(w.message) for w in caught if "stub node" in str(w.message)]
    assert len(msgs) == 1 and "valid at every instant" not in msgs[0], msgs


def test_blueprint_warnings_point_at_the_callers_line(tmp_path):
    path = _write(tmp_path, _dup_nodes(), DUP_CSV)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        from_blueprint(path, save=False)
    assert caught, "expected the duplicate-id warning"
    assert {w.filename for w in caught} == {__file__}, [w.filename for w in caught]
