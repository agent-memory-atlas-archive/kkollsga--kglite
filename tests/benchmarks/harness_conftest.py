"""The `_nodeid` compatibility hook for the benchmark harness.

pytest 9.2 removed the private `Node._nodeid` attribute; pytest-benchmark 5.3.0
(the latest release) still reads `node._nodeid` when it builds a fixture, so
every benchmark errors at setup. Hooks only run from a conftest or plugin, and
the CI perf harnesses are copied out of the checkout one file at a time, so the
workflows copy this file beside each of them as `conftest.py`
(`tests/benchmarks/conftest.py` re-exports it for in-tree runs).

Delete it, its copy steps and the re-export once pytest-benchmark reads
`nodeid`.
"""


def pytest_itemcollected(item):
    if not hasattr(item, "_nodeid"):
        item._nodeid = item.nodeid
