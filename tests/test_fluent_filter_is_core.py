"""The fluent chain evaluates validity only through the core filter.

`date()`, `traverse(at=/during=)` and `valid_at()` / `valid_during()` resolve
to the `ElementFilter` a `FOR VALID_TIME AS OF` statement runs under (through
`kglite::api::fluent::FluentFilter`). A second evaluator — a binding reading
bounds and comparing dates itself, or a fluent step calling the interval
evaluator directly — is how the fluent answers drifted from Cypher's before:
a different label rule, unfiltered traversal targets, a limit applied before
the filter, a local "today". This scans the source so such a path cannot come
back unnoticed:

- the Python binding names no validity evaluator, bound resolver or date
  clock;
- the core files that implement fluent steps reach validity only through
  `FluentFilter`, and each of them still does;
- the interval evaluator is called only by the evaluator itself, the core
  filter and Cypher's `valid_at` / `valid_during` functions.

Every allow-listed file must still match what it is allowed, so a rename or a
move turns the gate red rather than leaving it passing over nothing.
"""

from __future__ import annotations

from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
BINDING = ROOT / "crates" / "kglite-py" / "src"
CORE = ROOT / "crates" / "kglite" / "src"

# Validity evaluation, bound resolution, and the legacy fluent evaluator the
# binding used to hold.
BINDING_FORBIDDEN = re.compile(
    r"\b(interval_contains|interval_overlaps|parse_instant|node_passes_context"
    r"|node_is_temporally_valid|node_overlaps_range|is_temporally_valid(_multi)?"
    r"|overlaps_range(_multi)?|matching_config|NodeValidityRequest|ValidityTest"
    r"|TemporalEdgeFilter|retain_current_level|node_request_config"
    r"|relationship_request_configs|edge_configs|node_config|ElementFilter)\b"
    r"|\bnow\(\)\s*\.\s*date_naive\b"
)

# Core files implementing fluent steps: validity only through FluentFilter.
FLUENT_STEP_FILES = [
    "graph/core/traversal.rs",
    "graph/core/filtering.rs",
    "graph/core/data_retrieval.rs",
    "graph/mutation/subgraph.rs",
]
FLUENT_FORBIDDEN = re.compile(
    r"\b(interval_contains|interval_overlaps|node_bound|edge_bound|TemporalConfig"
    r"|temporal_context|node_config|edge_configs)\b|\btemporal::eval\b|\bnow\(\)\s*\.\s*date_naive\b"
)

EVALUATOR_CALL = re.compile(r"\binterval_(contains|overlaps)\s*\(")
EVALUATOR_CALLERS = {
    "graph/features/temporal/eval.rs",
    "graph/core/graph_filter.rs",
    "graph/languages/cypher/executor/scalar_functions/validity.rs",
}


def _code(path: Path) -> str:
    """The source without `//` comments, which may name anything."""
    return "\n".join(line.split("//", 1)[0] for line in path.read_text(encoding="utf-8").splitlines())


def _production(root: Path) -> list[Path]:
    return [
        path
        for path in sorted(root.rglob("*.rs"))
        if not path.name.endswith("_tests.rs") and "tests" not in path.relative_to(root).parts
    ]


def test_the_patterns_match_what_they_forbid() -> None:
    for planted in (
        "node_passes_context(node, config, ctx)",
        "kglite_core::api::temporal::NodeValidityRequest::new(",
        "chrono::Local::now().date_naive()",
        "Utc::now() . date_naive()",
        "temporal::edge_configs(&self.inner, &t)",
        "fn retain_current_level(",
    ):
        assert BINDING_FORBIDDEN.search(planted), planted
    for allowed in ("set_temporal(", "temporal_context:", "FluentFilter::for_select("):
        assert not BINDING_FORBIDDEN.search(allowed), allowed
    assert FLUENT_FORBIDDEN.search("eval::interval_overlaps(from, to, a, b, c)")
    assert FLUENT_FORBIDDEN.search("temporal::node_bound(graph, idx, &b.from)")
    assert not FLUENT_FORBIDDEN.search("valid_time.admits_hop(graph, e, k, s, f)")
    assert EVALUATOR_CALL.search("eval::interval_contains (from, to, t, c)")


def test_the_binding_evaluates_no_validity() -> None:
    files = _production(BINDING)
    assert len(files) > 50, "the binding source tree moved"
    hits = [
        f"{path.relative_to(ROOT)}:{number}: {line.strip()}"
        for path in files
        for number, line in enumerate(_code(path).splitlines(), 1)
        if BINDING_FORBIDDEN.search(line)
    ]
    assert hits == [], "validity evaluated outside the core filter:\n" + "\n".join(hits)


def test_fluent_steps_reach_validity_only_through_the_core_filter() -> None:
    for relative in FLUENT_STEP_FILES:
        code = _code(CORE / relative)
        hits = [line.strip() for line in code.splitlines() if FLUENT_FORBIDDEN.search(line)]
        assert hits == [], f"{relative} evaluates validity itself: {hits}"
        assert "FluentFilter" in code, f"{relative} no longer routes a fluent step through FluentFilter"
    filter_source = _code(CORE / "graph/core/fluent_filter.rs")
    for required in ("ElementFilter", "GuardTemplate::for_scope", "GraphFilter"):
        assert required in filter_source, f"fluent_filter.rs lost {required}"
    assert not EVALUATOR_CALL.search(filter_source), "fluent_filter.rs calls the interval evaluator itself"


def test_the_interval_evaluator_has_only_its_known_callers() -> None:
    files = _production(CORE)
    assert len(files) > 300, "the core source tree moved"
    callers = {path.relative_to(CORE).as_posix() for path in files if EVALUATOR_CALL.search(_code(path))}
    assert callers == EVALUATOR_CALLERS, (
        f"unexpected interval-evaluator callers {sorted(callers - EVALUATOR_CALLERS)}; "
        f"allow-listed files no longer calling it {sorted(EVALUATOR_CALLERS - callers)}"
    )
