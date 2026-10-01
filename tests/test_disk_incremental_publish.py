"""A disk save links the column file of every node type it did not change from
the generation it replaces, and prunes generations older than the previous one.

The guarantees under test are the ones a hard link could break: the previous
generation must stay byte-for-byte what it was (a write through a shared inode
would alter two generations at once), a crash between linking and publishing
must leave the previous generation selected and intact, and retention must not
delete a generation a live reader still maps or break a generation whose files
another shares.

Org-chart data: ``Employee`` is the type every change is applied to;
``Department`` and ``Office`` are untouched siblings whose files are linked.

Run: pytest tests/test_disk_incremental_publish.py
"""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import textwrap
import time
import warnings

import pandas as pd
import pytest

import kglite

EMPLOYEES = 60_000
GENERATIONS = "generations"

posix_only = pytest.mark.skipif(os.name != "posix", reason="hard-link counts and SIGKILL are POSIX")


def _employees(count: int = EMPLOYEES) -> pd.DataFrame:
    return pd.DataFrame(
        {
            "id": range(count),
            "name": [f"Employee {i}" for i in range(count)],
            "grade": [i % 9 for i in range(count)],
        }
    )


def _named(prefix: str, count: int) -> pd.DataFrame:
    return pd.DataFrame({"id": range(count), "name": [f"{prefix} {i}" for i in range(count)]})


def build(path: Path, employees: int = EMPLOYEES) -> None:
    """Three types saved as one generation, then closed."""
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        graph = kglite.KnowledgeGraph(storage="disk", path=str(path))
        graph.add_nodes(_employees(employees), "Employee", "id", "name")
        graph.add_nodes(_named("Department", 300), "Department", "id", "name")
        graph.add_nodes(_named("Office", 40), "Office", "id", "name")
        graph.save()
        del graph


def current(path: Path) -> Path:
    return path / GENERATIONS / (path / "CURRENT").read_text(encoding="utf-8").strip()


def generation_names(path: Path) -> list[str]:
    return sorted(p.name for p in (path / GENERATIONS).iterdir() if p.name.startswith("gen_"))


def tree_hash(generation: Path) -> dict[str, str]:
    out = {}
    for file in sorted(generation.rglob("*")):
        if file.is_file():
            out[str(file.relative_to(generation))] = hashlib.sha256(file.read_bytes()).hexdigest()
    return out


def grade(graph, employee: int):
    return graph.cypher(f"MATCH (e:Employee {{id: {employee}}}) RETURN e.grade AS g").scalar()


def set_grade(graph, employee: int, value: int) -> None:
    graph.cypher(f"MATCH (e:Employee {{id: {employee}}}) SET e.grade = {value}")


def type_files(generation: Path) -> dict[str, Path]:
    import json

    meta = json.loads((generation / "seg_000" / "columns_meta.json").read_text(encoding="utf-8"))
    return {name: generation / "seg_000" / rel for name, rel in meta["files"].items()}


def test_a_set_and_save_link_the_untouched_types_and_leave_the_previous_generation_byte_identical(tmp_path):
    path = tmp_path / "graph"
    build(path)
    first = current(path)
    before = tree_hash(first)
    first_files = type_files(first)

    graph = kglite.load(str(path))
    set_grade(graph, 3, 70)
    graph.save()
    second = current(path)
    assert second != first

    second_files = type_files(second)
    for sibling in ("Department", "Office"):
        assert os.path.samefile(first_files[sibling], second_files[sibling]), (
            f"{sibling} was rewritten instead of linked"
        )
        assert os.stat(second_files[sibling]).st_nlink >= 2
    assert not os.path.samefile(first_files["Employee"], second_files["Employee"])

    assert tree_hash(first) == before, "saving the next generation altered the previous one"
    # A second cycle: the linked files are now shared by three generations' worth of saves.
    graph = kglite.load(str(path))
    set_grade(graph, 4, 71)
    graph.save()
    assert tree_hash(second) != {}, "generation 2 is the previous one and still there"
    reopened = kglite.load(str(path))
    assert grade(reopened, 3) == 70 and grade(reopened, 4) == 71
    assert reopened.cypher("MATCH (o:Office) RETURN count(o) AS c").scalar() == 40


@posix_only
def test_retention_keeps_the_current_generation_and_one_previous_and_every_answer_survives(tmp_path):
    path = tmp_path / "graph"
    build(path)
    graph = kglite.load(str(path))
    shared_inodes = {name: os.stat(file).st_ino for name, file in type_files(current(path)).items()}
    for round_ in range(5):
        set_grade(graph, round_, 50 + round_)
        graph.save()
    names = generation_names(path)
    assert len(names) == 2, names
    assert current(path).name == names[-1]

    latest = type_files(current(path))
    # The untouched types are the files the first generation wrote, and the
    # unlink of every generation between them and now did not take them.
    for sibling in ("Department", "Office"):
        assert os.stat(latest[sibling]).st_ino == shared_inodes[sibling]
        assert latest[sibling].stat().st_size > 0
    del graph
    reopened = kglite.load(str(path))
    assert [grade(reopened, i) for i in range(5)] == [50, 51, 52, 53, 54]
    assert reopened.cypher("MATCH (d:Department) WHERE d.id = 299 RETURN d.name AS n").scalar() == "Department 299"
    assert reopened.cypher("MATCH (e:Employee) RETURN count(e) AS c").scalar() == EMPLOYEES


@pytest.mark.parametrize(
    "setting, expected",
    [("0", 1), ("3", 4), ("all", 7), ("not-a-number", 2)],
)
def test_the_retention_setting_is_read_from_the_environment(tmp_path, setting, expected):
    path = tmp_path / "graph"
    build(path, employees=500)
    script = textwrap.dedent(
        f"""
        import kglite
        g = kglite.load({str(path)!r})
        for i in range(5):
            g.cypher(f"MATCH (e:Employee {{{{id: {{i}}}}}}) SET e.grade = {{100 + i}}")
            g.save()
        """
    )
    env = {**os.environ, "KGLITE_KEEP_GENERATIONS": setting}
    done = subprocess.run([sys.executable, "-c", script], env=env, capture_output=True, text=True)
    assert done.returncode == 0, done.stderr
    assert len(generation_names(path)) == expected, (setting, generation_names(path))
    reopened = kglite.load(str(path))
    assert [grade(reopened, i) for i in range(5)] == [100, 101, 102, 103, 104]


def test_a_held_read_only_view_reads_correctly_across_saves_and_retention(tmp_path):
    path = tmp_path / "graph"
    build(path)
    reader = kglite.load(str(path))
    reader_generation = current(path)
    expected = grade(reader, 10)

    writer = kglite.load(str(path))
    for round_ in range(4):
        set_grade(writer, 10, 900 + round_)
        writer.save()
    assert reader_generation.exists(), "the generation a live reader maps was pruned under it"
    assert grade(reader, 10) == expected
    assert reader.cypher("MATCH (e:Employee) RETURN count(e) AS c").scalar() == EMPLOYEES
    assert reader.cypher("MATCH (e:Employee {id: 59999}) RETURN e.name AS n").scalar() == "Employee 59999"
    # A copy remaps the reader's files by path: they must still be there.
    forked = reader.copy()
    assert forked.cypher("MATCH (o:Office) RETURN count(o) AS c").scalar() == 40
    del forked

    del reader
    import gc

    gc.collect()
    set_grade(writer, 10, 999)
    writer.save()
    assert not reader_generation.exists(), "with the reader gone the next save prunes its generation"
    assert len(generation_names(path)) == 2
    assert grade(kglite.load(str(path)), 10) == 999


CHILD = textwrap.dedent(
    """
    import sys, kglite
    g = kglite.load(sys.argv[1])
    g.cypher("MATCH (e:Employee {id: 3}) SET e.grade = 123")
    print("ready", flush=True)
    g.save()
    print("saved", flush=True)
    """
)


def _stage_holds_a_linked_file(path: Path) -> bool:
    for stage in (path / GENERATIONS).glob(".stage-*"):
        for file in stage.glob("seg_000/type_columns/*.bin"):
            try:
                if file.stat().st_nlink >= 2:
                    return True
            except FileNotFoundError:
                continue
    return False


@posix_only
def test_a_sigkill_between_linking_and_the_pointer_swap_leaves_the_previous_generation_selected(tmp_path):
    pristine = tmp_path / "pristine"
    build(pristine, employees=150_000)
    pointer_before = (pristine / "CURRENT").read_text(encoding="utf-8")
    previous_hash = tree_hash(current(pristine))

    for attempt in range(4):
        path = tmp_path / f"attempt_{attempt}"
        shutil.copytree(pristine, path, symlinks=True)
        child = subprocess.Popen(
            [sys.executable, "-c", CHILD, str(path)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
        )
        try:
            assert child.stdout.readline().strip() == "ready", child.stderr.read()
            deadline = time.monotonic() + 60
            killed_mid_save = False
            while time.monotonic() < deadline and child.poll() is None:
                if _stage_holds_a_linked_file(path):
                    killed_mid_save = (path / "CURRENT").read_text(encoding="utf-8") == pointer_before
                    child.send_signal(signal.SIGKILL)
                    break
            child.wait(timeout=30)
        finally:
            if child.poll() is None:
                child.kill()
        if killed_mid_save:
            break
    else:
        pytest.fail("the save finished before it could be killed after linking, four times")

    assert child.returncode == -signal.SIGKILL
    assert (path / "CURRENT").read_text(encoding="utf-8") == pointer_before, (
        "CURRENT moved although the save never finished"
    )
    assert tree_hash(current(path)) == previous_hash, "the crashed save altered the previous generation"

    reopened = kglite.load(str(path))
    assert grade(reopened, 3) == 3, "the unsaved SET is not in the reopened graph"
    assert reopened.cypher("MATCH (o:Office) RETURN count(o) AS c").scalar() == 40
    assert reopened.cypher("MATCH (e:Employee) RETURN count(e) AS c").scalar() == 150_000

    # The next writer publishes over the dead one's leftovers.
    set_grade(reopened, 3, 456)
    reopened.save()
    assert not list((path / GENERATIONS).glob(".stage-*")), "a dead writer's stage survived the next save"
    assert grade(kglite.load(str(path)), 3) == 456
    assert tree_hash(current(pristine)) == previous_hash, "the pristine copy the attempts were cloned from changed"
