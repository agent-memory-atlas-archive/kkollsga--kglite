# Ontology (declared semantic layer)

An ontology gives type names a declared "kind of" structure. `Student` *is a* `Person`; `Contract` *is a* `Agreement`. It also adds machine-readable semantics for relationships:

- which types an edge connects
- which properties it must carry
- its inverse's name
- its cardinality

KGLite persists these declarations with the graph. It wires them into `describe()`, the rule procedures, blueprint builds and, optionally, label matching itself.

**The scope fence: annotations, not axioms.** In spirit this is SKOS, not OWL. The ontology never invents facts and never changes what a query matches on its own. There is no entailment, no open-world semantics and no reasoning.

The ontology has three jobs:

- State the concept model machines can read.
- Provide defaults for validators you already have.
- Act, opt-in, as a data-quality contract at build time and at write time.

It is also deliberately **not** `set_parent_type`. That map is presentation *ownership*: which types are supporting detail in `describe()` tiering. The ontology is semantic *kind-of*. `ProjectCore → Project` is ownership; `Contract is_a Agreement` is an ontology fact. Neither is derived from the other.

## Declaring

```python
g.define_ontology({
    "classes": {
        "Person":  {"abstract": True, "description": "Any human actor"},
        "Student": {"is_a": "Person"},
        "Teacher": {"is_a": "Person"},
        # documentation-only discriminator (rendered as unenforced):
        "Project": {"by": "projectKind"},
    },
    "relationships": {
        "ENROLLED_IN": {
            "domain": "Student", "range": "Class",
            "inverse_name": "HAS_STUDENT",      # reading-direction alias
            "inverse_enforced": True,            # opt-in: audit physical pairing
            "cardinality": {"min": 1},
            "required": True,
            "required_properties": ["since"],    # audited per edge
            "property_types": {"since": "integer"},
            "enforcement": "warn",               # or per-check: {"required_properties": "error"}
            "description": "Active enrolment",
        },
        "STRAT_PARENT": {"domain": "Stratigraphy", "range": "Stratigraphy",
                          "ancestry": True},   # parent pointers, walked with *1..
    },
})
```

KGLite checks the rules below when it installs a declaration.

### Classes and hierarchy

- `is_a` is a **forest**: one parent per class, no cycles, and parents must be declared. You model multi-role nodes with secondary labels on nodes, not with multiple inheritance in the class graph.
- Class names share the label namespace. This keeps `MATCH (n:X)` at exactly one meaning per name:
  - A class naming a live node type is *concrete*.
  - A class naming none must usually be `abstract: True`.
  - A non-abstract class naming no live type is a returned warning.
  - An abstract class shadowing a live type is an error.
- The class cap is **512 classes** (`MAX_ONTOLOGY_CLASSES`), and it is enforced. It is a hard number, so capacity planning needs no experiment. The layer is for schema-level vocabularies.
- A million-class taxonomy (Wikidata's P279) is **data**. Keep it as edges, declare the relationship `ancestry: True`, and walk it with `*1..` paths. That boundary is a feature. Do **not** reach for `transitive: True` there; see the next bullet.
- `transitive: True` and `ancestry: True` both say "this edge is a hierarchy". They are mutually exclusive, and declaring both is refused.
  - `transitive` is a **promise that the closure is stored**. It enrolls `transitivity_violation`, which flags every `a→b→c` with no stored `a→c` edge. A taxonomy that stores only parent pointers therefore reports 100% violations.
  - `ancestry` is the annotation for that shape. It records that the chain is meaningful and is walked with `*1..`, shows up in `describe()`, and enrolls no check.

### Relationship declarations

- `cardinality` / `required` describe **outgoing** edges of the domain type.
- `symmetric: True` lowers to an inverse check of the relationship against itself.
- `inverse_name` is a **reading-direction alias**. No second edge exists or is implied, and it enrolls no check.
  - `inverse_enforced: True` opts into auditing physical pairing: each edge must have a stored inverse partner.
  - `symmetric: True` keeps its self-inverse check regardless, because symmetry *is* a physical claim.

### Property contracts

- `property_types` accepts `list` (alias `array`, case-insensitive) for any native list, including empty, mixed and nested lists. This checks the outer container only; it does not declare an element type. Missing and null values are left to `required_properties`.
- `required_properties` and `property_types` apply to nodes in class declarations and to edges in relationship declarations.
  - Required values must be present and non-null.
  - Type checks ignore absent and null values.
  - Repeated required names count once.
  - Type names are validated on declaration.
  - Edge properties use stored names, including a blueprint's renamed output column.
  - Node properties use the actual primary type's loader aliases and id/title resolution.

### Enforcement and exemptions

- `enforcement` is `advisory` (the default), `warn` or `error`. `warn` and `error` bind every write; see [Write-time enforcement](#write-time-enforcement). `advisory` only feeds the audit and the rule procedures.
- A top-level `closed_labels: True` adds the allowed-labels rule. A top-level `enforcement` sets its severity, and is the default for declarations that state none.
- `enforcement` also accepts a per-check map, such as `{"required_properties": "error", "domain": "warn"}`. Unlisted checks keep the advisory base.
- The map keys are the check names the audit's `rule` column uses: `domain`, `range`, `required`, `required_properties`, `property_types`, `cardinality`, `inverse`, `symmetric`, `transitive`.
- `exempt` names, per check, source classes whose violations are counted separately instead of against severity. See [Exempting an upstream source](#exempting-an-upstream-source).

### Reading and removing the store

`g.ontology()` returns the store as a dict. `g.clear_ontology()` removes it, withdrawing any materialized labels first. From Cypher, `CALL db.ontology.declare({ontology: $doc})` and `CALL db.ontology.clear()` do the same, so a Bolt client can declare without the Python API. `CALL db.ontology.show() YIELD ontology, locked, enforcement` reads the declared document back as a map that `db.ontology.declare` accepts unchanged (Null when nothing is declared); `locked` is the operator's `--ontology` lock. The store persists in the `.kgl` and travels with `save_subset` / `to_subgraph`.

## Node property contracts

```python
g.define_ontology({"classes": {
    "Study": {"required_properties": ["design", "tags"],
              "property_types": {"tags": "list"},
              "enforcement": {"required_properties": "error"}},
    "Trial": {"is_a": "Study", "required_properties": ["registration"]},
}})
```

Each declaring class governs nodes whose primary class is itself or a declared descendant.

- Parent and child contracts are independent and additive.
- Unrelated secondary labels do not enroll a node.
- Class enforcement accepts only `required_properties` and `property_types`.
- Class enforcement defaults to advisory and has no edge-style exemptions.
- Blueprint builds apply the same warn/error behavior to node contracts before publishing output.

`CALL ontology_audit()` returns `entity_kind` (`node` or `edge`) beside `rule`. Together they identify a rule even when a class and a relationship have the same name.

Node rules in the audit follow these conventions:

- A node rule's denominator is its covered live nodes.
- Exemptions are zero.
- An empty denominator yields zero violations and `0.0` percent.
- Property breakdowns count a node under every failed field, including zero-count declared fields.
- Aggregate counts count that node once.
- Domain-class breakdowns group violations by actual primary type.

```cypher
CALL node_property_violation()
YIELD class, check, node, property, properties
RETURN class, check, node.id AS id, properties
```

`node_property_violation()` takes no parameters. It returns one row per violating node per declaring class/check. `properties` lists all failed fields and `property` is the first. The node binding composes with subsequent query clauses.

No ontology is an error. An ontology without node contracts returns no findings.

`SHOW ONTOLOGY` exposes `required_properties`, `property_types` and enforcement for classes and relationships. The contracts persist with the graph. Legacy class declarations without these fields retain empty/advisory defaults.

## Reading it back

- `SHOW ONTOLOGY` returns one row per class and relationship.
- `describe()` renders an `<ontology>` section whenever a store is declared. Focused mode narrows to the classes touching the requested types. There is no new mode or parameter: absence of the section *is* the "no ontology" signal.
- `describe(cypher=["ontology"])` returns the agent-facing topic documentation.

## Declaration-driven validators

Call the six declaration-backed rule procedures with **no arguments** to check every relevant declaration. Each row carries a `rule` column naming the declaration it came from:

```cypher
CALL type_domain_violation() YIELD source, target, rule
CALL missing_required_edge() YIELD node, rule
CALL inverse_violation() YIELD a, b, rule
```

A `domain`/`range` naming an **abstract class widens to its declared descendants**. This is the union-endpoint case a flat schema cannot declare. `MANAGED_BY` from six concrete source types becomes `domain: "Agreement"`, and the existing checks finally reach it.

### The scorecard

`ontology_audit()` rolls every declared check up into one call. It returns one row per declared check, carrying:

- its violation count
- its denominator
- its percentage
- its declared severity
- the count its `exempt` classes excused

```python
from kglite import KnowledgeGraph

g = KnowledgeGraph()
g.cypher("""
CREATE (s:Student {id: 's1'}), (t:Teacher {id: 't1'}), (a:Alumnus {id: 'a1'}),
       (c:Class {id: 'c1'})
CREATE (s)-[:ENROLLED_IN {since: 2024}]->(c)
CREATE (t)-[:ENROLLED_IN {since: 2023}]->(c)
CREATE (a)-[:ENROLLED_IN {since: 2019}]->(c)
""")
g.define_ontology({
    "classes": {"Person": {"abstract": True}, "Student": {"is_a": "Person"},
                "Teacher": {"is_a": "Person"}, "Alumnus": {"is_a": "Person"},
                "Class": {}},
    "relationships": {
        "ENROLLED_IN": {"domain": "Student", "range": "Class",
                        "enforcement": "warn"},
    },
})

for row in g.cypher(
    "CALL ontology_audit() YIELD rule, severity, violations, exempted, total, pct"
):
    print(row)
# {'rule': 'ENROLLED_IN.domain', 'severity': 'warn', 'violations': 2, 'exempted': 0, 'total': 3, 'pct': 66.7}
# {'rule': 'ENROLLED_IN.range', 'severity': 'warn', 'violations': 0, 'exempted': 0, 'total': 3, 'pct': 0.0}
```

Run it after every rebuild and you have data-quality-over-time for free. An agent can call it cold and qualify its own answers.

### Which source types are violating

`{by: 'domain_class'}` answers the question every scorecard raises next. Each rule's row fans out into one row per primary node type its violations come from. Each row carries that class's share of `violations` and `pct`; they sum back to the rule's aggregate. `severity`, `exempted` and `total` keep their per-rule values.

- Exempted rows are left out, so a class whose every violation is excused gets no row at all.
- A rule with nothing to break down keeps its single aggregate row.
- Without the parameter, `domain_class` is `None` on every row.
- A bare `CALL ontology_audit()` and the `{by: …}` forms all return the nine columns, including `entity_kind` to distinguish node and edge rules.

```python
for row in g.cypher(
    "CALL ontology_audit({by: 'domain_class'}) YIELD rule, domain_class, violations, pct"
):
    print(row)
# {'rule': 'ENROLLED_IN.domain', 'domain_class': 'Alumnus', 'violations': 1, 'pct': 33.3}
# {'rule': 'ENROLLED_IN.domain', 'domain_class': 'Teacher', 'violations': 1, 'pct': 33.3}
# {'rule': 'ENROLLED_IN.range', 'domain_class': None, 'violations': 0, 'pct': 0.0}
```

The domain-side class is:

- the edge's source for `domain` / `range` / `required_properties` / `property_types`
- the node itself for `required` / `cardinality`
- the first bound node for the pair and triple shapes (`inverse`, `symmetric`, `transitive`)

### Which fields are missing

`{by: 'property'}` fans the `required_properties` and `property_types` rules into one row per **declared** property.

- `violations` counts the nodes or edges failing that property.
- `total` is the rule's covered nodes or relationship edges.
- `pct` is the share failing it.
- Every other rule keeps its aggregate row with a `None` property.

```python
for row in g.cypher(
    "CALL ontology_audit({by: 'property'}) YIELD rule, property, violations, pct"
):
    print(row)
# {'rule': 'ENROLLED_IN.required_properties', 'property': 'since', 'violations': 1, 'pct': 33.3}
# {'rule': 'ENROLLED_IN.required_properties', 'property': 'grade', 'violations': 2, 'pct': 66.7}
# {'rule': 'ENROLLED_IN.domain', 'property': None, 'violations': 0, 'pct': 0.0}
```

```{important}
The two breakdowns answer different shapes of question, and reading one as
the other double-counts. `domain_class` **partitions** a rule — every
violating row has exactly one source class, so the rows sum back to the
aggregate `violations`. `property` is a **census** — one node or edge missing three
declared properties is counted under all three, so the rows sum to *at least*
the aggregate and adding them up does not give the rule's violation count.
A declared property nothing fails still gets a row, at zero; "this field is
complete" is the answer a census is asked for. One axis applies at a time:
the column you did not ask for is `None`.
```

## Write-time enforcement

A declaration at `warn` or `error` binds every write. The audit and the write gate read the same predicates, so a rule that `ontology_audit()` counts is a rule a write is judged on.

```python
g.define_ontology({
    "classes": {
        "Person": {"required_properties": ["name"],
                   "property_types": {"name": "string"},
                   "enforcement": "error"},
        "Company": {},
    },
    "relationships": {
        "WORKS_AT": {"domain": "Person", "range": "Company", "enforcement": "error"},
    },
})
g.cypher("CREATE (:Person {id: 1, name: 'A'})")     # accepted
g.cypher("CREATE (:Person {id: 2})")                # OntologyViolationError
```

### What `error` and `warn` do

- `error` refuses the write and rolls it back. Nothing the write did is kept, and the raised `OntologyViolationError` carries `rule`, `entity`, `entity_type` and `property`.
- `warn` accepts the write and reports each violation. A Cypher statement reports through `result.diagnostics["warnings"]`; a bulk loader raises a Python `UserWarning`. Over Bolt it is the `kglite.ontology` key of the result summary plus a server log line.
- `advisory` judges nothing at write time.
- A transaction that hits `error` in its third statement rolls back the first two as well when it is used as a context manager (`with g.begin() as tx:`) or when the driver closes it. Catching the error and committing keeps the earlier statements.

### Rules that are enforced

| Rule | Declared as | Refuses |
|---|---|---|
| `required_property` | class or relationship `required_properties` | An absent or null value. |
| `property_type` | class or relationship `property_types` | A present, non-null value of another type. |
| `closed_labels` | top-level `closed_labels: True` | A node whose primary label is not a declared class. |
| `domain` | relationship `domain` | An edge whose source primary type is not the domain. |
| `range` | relationship `range` | An edge whose target primary type is not the range. |

- A node is judged on its **primary** label only. It answers to its own class and to every declared ancestor, each at that class's severity. Secondary labels never enroll a node and `closed_labels` never reads them. Materialized (managed) labels are engine-written and are not judged.
- `domain` and `range` naming an abstract class widen to its declared descendants.
- Types are permissive, exactly as the audit counts them: `float` admits integers, an unknown type name passes, and `list` checks the outer container only. For strict typing use `CREATE CONSTRAINT ... IS ::`.
- `exempt` excuses the write as well as the audit count, under the same predicate.
- A Cypher statement is judged once its clauses have run, so a later `SET` repairs an earlier `CREATE` in the same statement.
- Bulk loaders (`add_nodes`, `add_connections`, `update`, `store_as`, `add_properties`, `extend`, `attach_rows` and the relationship-named twins) judge the whole frame before writing anything. A refused frame leaves the graph unchanged.
- `attach_rows` is atomic. A refused edge step leaves none of its row nodes behind.

### A synthesised title does not satisfy a required `name`

KGLite mints a title for a node loaded without one (`<Label>_<id>`, or the id on an untitled type), and `name` reads as a soft alias of the title. The rules discard a minted title, so `required_properties: ["name"]` is not satisfied by it.

**Blind spot:** a title you supply that equals `<Label>_<id>` exactly, such as `Person_7` on a `Person` with id 7, is read as synthesised and does not satisfy the rule. Give such a node a distinct name or set `name` explicitly.

### `load_ntriples`

N-Triples are loaded without per-row judgement. The loader is refused while any node or relationship rule is declared at `error`. Under `warn` it loads and reports that per-row judgement was skipped (`NTriplesStats.warnings`). Load into a graph without the ontology, or declare it afterwards: the declaration then checks what was loaded.

### Declaring over existing data

Declaring checks stored data first, at each rule's severity:

- A rule at `error` that stored data already breaks **refuses the whole declaration**. The previous ontology stays and nothing is changed.
- A rule at `warn` installs, and the findings come back as warnings.
- A rule at `advisory` costs no scan.
- `.kgl` load and write-ahead-log replay restore an accepted declaration without re-checking it.

The refusal is an `OntologyViolationError` whose `report` lists one entry per rule, type and property with the count of stored entities breaking it:

```python
g.cypher("CREATE (:Person {id: 1})")
try:
    g.define_ontology({"classes": {"Person": {"required_properties": ["name"],
                                              "enforcement": "error"}}})
except kglite.OntologyViolationError as e:
    print(e.report)
    # [{'rule': 'required_property', 'entity': 'node',
    #   'entity_type': 'Person', 'property': 'name', 'count': 1}]
```

Fix the data, or declare the rule at `warn` first and promote it once the report is clean. The refusal arrives the same way from `define_ontology()`, `CALL db.ontology.declare()`, the C ABI (status 22) and Bolt.

### Surfaces

| Surface | Declare | Refusal |
|---|---|---|
| Python | `g.define_ontology(doc)`, `g.clear_ontology()` | `OntologyViolationError`, a subclass of `ConstraintViolationError`. |
| Cypher | `CALL db.ontology.declare({ontology: $doc})`, `CALL db.ontology.clear()` | The same typed error. |
| C ABI | `kglite_session_define_ontology`, `kglite_session_clear_ontology` | Status `KGLITE_STATUS_CODE_ONTOLOGY_VIOLATION` (22). |
| Bolt server | `--ontology FILE` at startup (see the [Bolt server guide](../../operators/bolt-server.md#enforced-ontology)) | `Neo.ClientError.Schema.ConstraintValidationFailed`. |

`SHOW ONTOLOGY` reads the declaration back on every surface. A backup carries the ontology: a graph backed up at `error` refuses the same writes when opened.

### Cost

Enforcement adds work only to writes it judges. With no ontology, or only `advisory` rules, the write path is unchanged (measured within noise of 0.19.5). With an `error` ontology and all-valid data, the measured cost on a release build was:

| Write | Added cost |
|---|---|
| `CREATE` of nodes | about 14-17% |
| `SET` | about 30-32% |
| `add_nodes` | about 12-18% |
| `add_connections` | about 40-45% |
| `CREATE` of edges | about 5-9% |
| `MERGE` | none measured |

The cost scales with the size of the write, not the size of the graph.

## The blueprint gate (observe → fix → enforce)

Reference the document from a blueprint and the declarations become a build-time contract:

```json
{ "ontology": "school.ontology.json", "nodes": { ... } }
```

The gate runs as a final build phase, after all loading and before anything is saved. Per-declaration severity decides what a violation does:

| `enforcement` | On violation |
|---|---|
| `advisory` | nothing at build time — available on demand via the `CALL`s |
| `warn` | one summary line per rule in the build report |
| `error` | **report every violation, then fail once** — no output file is written |

The intended lifecycle has three steps:

1. Start every rule at `advisory`.
2. Read the report as your cleanup worklist and fix the data.
3. Flip the rules you own to `error`, so the debt can never silently return.

Rules describing *upstream* data reality stay `warn` forever. They belong in the build log, not the exit code.

## Exempting an upstream source

An abstract `domain` lets one declaration cover a union edge, such as `MANAGED_BY` from every `Agreement`. It also lets a single nonconforming source poison the whole rule.

Suppose one upstream source never carried the date the others do. Then `required_properties: ["validFrom"]` can never be promoted past `advisory`. The rule you want to enforce for the sources you control is permanently red because of a source you do not control.

`exempt` is the seam. It is a **per-check map** of source classes whose violations are counted separately instead of against severity:

```python
from kglite import KnowledgeGraph

g = KnowledgeGraph()
g.cypher("""
CREATE (a:Contract {id: 'C001'}), (b:Contract {id: 'C002'}),
       (p:LegacyContract {id: 'L900'}), (c:Company {id: 'ACME'})
CREATE (a)-[:MANAGED_BY {validFrom: 1995}]->(c)
CREATE (b)-[:MANAGED_BY]->(c)
CREATE (p)-[:MANAGED_BY]->(c)
""")
g.define_ontology({
    "classes": {"Agreement": {"abstract": True},
                "Contract": {"is_a": "Agreement"},
                "LegacyContract": {"is_a": "Agreement"},
                "Company": {}},
    "relationships": {
        "MANAGED_BY": {
            "domain": "Agreement", "range": "Company",
            "required_properties": ["validFrom"],
            "enforcement": {"required_properties": "error"},
            # the legacy source has no start date; the others must have one
            "exempt": {"required_properties": ["LegacyContract"]},
        },
    },
})

for row in g.cypher(
    "CALL ontology_audit() YIELD rule, severity, violations, exempted, total"
):
    print(row)
# {'rule': 'MANAGED_BY.domain', 'severity': 'advisory', 'violations': 0, 'exempted': 0, 'total': 3}
# {'rule': 'MANAGED_BY.range', 'severity': 'advisory', 'violations': 0, 'exempted': 0, 'total': 3}
# {'rule': 'MANAGED_BY.required_properties', 'severity': 'error', 'violations': 1, 'exempted': 1, 'total': 3}
```

Both edges lack `validFrom`, but only the `Contract` one counts as a violation. The rule can sit at `error` and still block exactly the debt you own.

What the form guarantees:

- **Per-check, never flat.** `exempt: ["LegacyContract"]` is refused. An exemption spread silently across every check is not something you can reason about later. Name the check it applies to.
- **`required_properties` and `property_types` only.** These are the two checks where "the class to exempt" unambiguously means the edge's *source* type. Any other check name under `exempt` is refused at declaration time with the reason, not just an accept-list.
- **Ancestor-widening.** A class matches when it is the edge source's primary type *or* one of its declared ancestors, the same widening `domain`/`range` get. Exempting `Agreement` exempts the whole subtree.
- **The class must be declared.** An undeclared name is refused. Matching widens over the `is_a` forest, so a typo would silently exempt nothing, which is the exact failure the feature exists to remove.

### Drilling down to the flagged edges

`exempted` never hides rows. `violations + exempted` is everything the check flagged. `edge_property_violation()` lists those individual edges: the row-level drill-down behind the `required_properties` / `property_types` counts. Its `exempt` column marks which side of the line each row fell on:

```python
for row in g.cypher("""
    CALL edge_property_violation() YIELD check, source, property, properties, exempt
    RETURN check, source.id AS source, property, properties, exempt
"""):
    print(row)
# {'check': 'required_properties', 'source': 'C002', 'property': 'validFrom',
#  'properties': ['validFrom'], 'exempt': False}
# {'check': 'required_properties', 'source': 'L900', 'property': 'validFrom',
#  'properties': ['validFrom'], 'exempt': True}
```

`properties` lists every declared property the edge fails, and `property` is the first of them. An edge missing three is still one row, so the listing keeps reconciling with the scorecard. Use `UNWIND properties AS p` for the per-field tally the row listing does not give you, or ask `ontology_audit({by: 'property'})` for it directly.

At the blueprint gate the exempted count is reported, never dropped. Every summary line carries a `(+N exempted)` tail. A rule declared `error` whose violations are *all* exempted is reported as a **warning** rather than passing silently. An exemption that quietly absorbed every flagged row would make a passing gate indistinguishable from a clean graph.

## Materialization (making supertypes matchable)

Everything above changes no query semantics. Materialization does, by explicit opt-in and through completely ordinary machinery:

```python
g.materialize_ontology()
g.cypher("MATCH (p:Person) RETURN p.name")   # finds Students and Teachers
```

`Student is_a Person` is stamped as the **real secondary label** `:Person` on every `Student` node. It goes through the same bulk label path every label write uses. So `MATCH (p:Person)` works with today's semantics, today's candidate index and today's `EXPLAIN`. `labels(n)`, CDC, exports and Bolt clients never disagree with what queries see.

From then on the write paths maintain the closure:

- A created `Student` carries `:Person` from birth.
- Creating a node of a declared *abstract* class is refused, naming the concrete subtypes.

Each materialized label is **managed**, in one of two states. `g.ontology_diff()` reports them.

- **`closed`**: the engine is the bucket's only writer, and the label holds exactly the declared closure. Closure-reliant optimizations may trust it. A property-filtered supertype match (`MATCH (p:Person {name: 'Ann'})`) runs per-descendant index probes instead of scanning.
- **`open`**: something outside the closure touched the label. Examples are a manual `SET n:Person` on a non-member, an adopted pre-existing bucket, and an extend-graph union. Everything stays *correct*; the optimizations switch off for that label.

Writers downgrade to `open` rather than refuse, so the result is a performance cliff instead of a wrong-answer cliff. The one refusal is manual `REMOVE` of a managed label, because an under-complete bucket has no safe state. `g.dematerialize_ontology()` is the exit, and it recovers correctly through the write-ahead log like every other write.

### Where you write the label decides the plan

A materialized supertype is worth having only if your queries reach it through the label engine. That depends on *where in the query the label sits*, not on whether the name is spelled the same:

```python
from kglite import KnowledgeGraph

g = KnowledgeGraph()
g.cypher("""CREATE (:Student {id: 1, title: 'Ann'}), (:Teacher {id: 2, title: 'Bo'}),
                  (:Class {id: 3, title: 'Math'})""")
g.define_ontology({"classes": {"Person": {"abstract": True},
                               "Student": {"is_a": "Person"},
                               "Teacher": {"is_a": "Person"}}})
g.materialize_ontology()
g.create_index("Student", "title")     # every live member must be covered
g.create_index("Teacher", "title")

for row in g.cypher("EXPLAIN MATCH (p:Person {title: 'Ann'}) RETURN p.id"):
    print(row)
# {'step': 1, 'operation': 'Match :Person', 'estimated_rows': 2}
# {'step': 2, 'operation': 'ClosureProbe :Person (Student, Teacher)', 'estimated_rows': None}
# {'step': 3, 'operation': 'Return', 'estimated_rows': None}

for row in g.cypher("EXPLAIN MATCH (p) WHERE p:Person AND p.title = 'Ann' RETURN p.id"):
    print(row)
# {'step': 1, 'operation': 'Match', 'estimated_rows': 3}
# {'step': 2, 'operation': 'Where', 'estimated_rows': None}
# {'step': 3, 'operation': 'Return', 'estimated_rows': None}
# {'step': 4, 'operation': 'OptimizerPass push_where_into_match.1', 'estimated_rows': None}
```

**Pattern position: `MATCH (p:Person)`** is the label engine, because the label *is* the candidate set.

- On a `closed` label whose every live member type carries an index for the filtered property, a property-filtered supertype match runs per-member index probes instead of scanning.
- `EXPLAIN` says so with a `ClosureProbe :Person (Student, Teacher)` row naming the members it would visit.
- No row means no probe: the label is `open`, a member is unindexed, or the label is not materialized at all. The match then falls back to a scan that is still correct.
- A value written as a parameter, `{title: $t}`, is unresolved when the plan renders, so the marker stays off. The runtime probe still applies.

**`WHERE p:Person`** is an ordinary post-candidate predicate.

- The pattern binds every node in the graph and checks the label per row. Note the unlabelled `Match` above, estimating all 3 nodes rather than the 2 that carry `:Person`.
- Nothing rewrites a `WHERE`-position label check into a candidate set, so this shape never probes.
- Move the label into the pattern.

**Alternation: `MATCH (p:Student|Teacher)`** is the *unmaterialized* alternative.

- It matches the union of the branches with no labels stamped and no closure to maintain.
- It carries no `ClosureProbe` either, because there is no managed bucket to trust.
- It names the members literally, so a subtype added to the class forest later will not be in it.
- Prefer it when you want the union once. Materialize when the supertype is a first-class thing your queries name repeatedly.

Materializing onto a graph whose label buckets already have members the closure cannot explain is refused unless you pass `materialize_ontology(adopt=True)`. The label is then managed `open`.

## Serving it over MCP

```yaml
extensions:
  ontology:
    file: school.ontology.json
    materialize: true        # optional
```

The server installs (and optionally materializes) the declarations at boot, **memory-only**. Nothing in the server auto-saves, so the source `.kgl` is untouched. That gives you adoption with zero build-script changes. An agent explicitly calling the `save_graph` tool persists them, which is then correct.

## How this maps to RDFS / OWL / SHACL

For readers coming from the semantic-web stack, here is the honest positioning, including three places where a familiar word carries *different* semantics here:

| Concept | There | Here |
|---|---|---|
| `is_a` | `rdfs:subClassOf`, a DAG (multiple inheritance) | a **forest** — one parent per class. Multi-role is modelled on *nodes* (secondary labels; a materialized node carries the union of its labels' ancestries), not in the class graph. |
| `domain` / `range` | RDFS **infers**: using an edge *entails* the subject's class membership | **checked, never inferred** — the SHACL reading. A violating edge is reported (or refused at the build gate); nothing ever gains a class from edge use. If you expect RDFS semantics, this is the one difference to internalize. |
| Validation | SHACL, deliberately separate from ontology/inference | the same separation, built in: `enforcement: advisory \| warn \| error` maps onto `sh:Info` / `sh:Warning` / `sh:Violation`. |
| Subclass queries | entailment regimes; production stores typically **materialize** entailments | the same technique: `materialize_ontology()` stamps ancestor labels; no query rewriting, no reasoner. |
| `inverse_name` | `owl:inverseOf` creates/entails the inverse triples | naming only — Cypher already traverses both directions, so no second edge exists or is implied. `inverse_enforced: True` opts into auditing stored pairing instead. |
| `transitive` | `owl:TransitiveProperty`, entailed closures | nothing is entailed: it declares that the closure is **stored**, and `transitivity_violation` audits that claim (every `a→b→c` needs a stored `a→c`). |
| `ancestry` | no counterpart — a reasoner would entail the chain | documentation only: the chain is meaningful and is *walked* (`*1..`), never stored. This is what a parent-pointer taxonomy declares. |
| "abstract" | not an ontology notion (any class may have instances) | borrowed from the schema world: a class that names no node type and cannot be instantiated directly. |

**Deliberate non-goals** (not omissions):

- entailment of any kind
- open-world semantics
- equivalence classes (`owl:equivalentClass`): within one graph, two names for one concept is a rebuild, not an axiom
- restriction classes
- property hierarchies (`rdfs:subPropertyOf`)
- disjointness axioms (low value under single primary types)

RDFS/SKOS import/export is tracked as future interop, adopting the vocabulary, not the entailment.

## When not to use it

- **Large taxonomies as classes.** The class cap enforces this away; keep them as edges (see the declaration rules above).
- **Entity resolution.** The ontology relates *types*, never records. `Student is_a Person` says nothing about whether two Alice Smiths are one human.
- **As a data-cleaning tool.** The gate *finds and then guards* cleanup. The fixing itself belongs in your load pipeline.

## See also

- {doc}`blueprints` — the build pipeline the gate plugs into.
- {doc}`traversal-hierarchy` — `set_parent_type`, the *presentation*
  hierarchy this layer is deliberately not.
- {doc}`/concepts/multi-label-rationale` — the label model the
  materialization builds on.
