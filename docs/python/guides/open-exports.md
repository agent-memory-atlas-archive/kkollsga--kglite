# Open exports

**A graph leaves KGLite in an open format and comes back unchanged.** Besides the
`.kgl` file, a graph can be written as a lossless CSV tree or as RDF 1.2, and both
carry enough metadata to restore what plain tables or triples cannot: valid-time
declarations, secondary labels, parent types, and the id and title kinds.

The examples share one small org chart: people with versioned roles, a department,
and a `WORKS_IN` relationship that carries a property.

```python
import kglite

g = kglite.KnowledgeGraph()
g.cypher("""
CREATE (:Department {id: 'eng', title: 'Engineering'}),
       (:Person {id: 1, title: 'Ada', level: 7,
                 valid_from: date('2020-01-01'), valid_to: date('2023-01-01')}),
       (:Person {id: 1, title: 'Ada', level: 8, valid_from: date('2023-01-01')})
""")
g.cypher("""
MATCH (p:Person {id: 1}), (d:Department {id: 'eng'}) WHERE p.level = 8
CREATE (p)-[:WORKS_IN {role: 'lead'}]->(d)
""")
g.cypher("""
CALL db.temporal.declare({node: 'Person', from: 'valid_from', to: 'valid_to',
                          convention: 'half_open'}) YIELD declared RETURN declared
""")
```

## Which format

| You want | Use | Read back with |
| --- | --- | --- |
| A complete backup, any storage mode, all engine state | `.kgl` (`save`) | `kglite.load` |
| Tables for a spreadsheet, a warehouse or a hand edit, restorable | lossless CSV + blueprint (`export_csv`) | `kglite.from_blueprint` |
| Triples for another graph store or a linked-data pipeline | RDF 1.2 N-Quads / TriG (`export_rdf`) | `kglite.load_rdf` |

A `.kgl` is the only complete backup: the open formats omit engine-specific state
such as indexes, embeddings and schema locks. The open formats are for moving the
*data*, with its declared meaning, somewhere else. From the command line the same
exports are `kglite export` (see the {doc}`CLI guide </operators/cli>`), and the
C ABI offers `kglite_export_csv` and `kglite_export_rdf`.

## Lossless CSV

```python
summary = g.export_csv("org-csv")
# {'output_dir': 'org-csv', 'nodes': {'Department': 1, 'Person': 2},
#  'connections': {'WORKS_IN': 1}, 'files_written': 5}

back = kglite.from_blueprint("org-csv/blueprint.json")
```

The tree holds one CSV per node type, one per relationship type, `blueprint.json`
(the loader description) and `manifest.json` (format `kglite-export/1`). The
blueprint points at the manifest, so `from_blueprint` restores valid-time
declarations, secondary labels, id and title kinds and every property's type, and
keeps the empty string distinct from null. Rows stream through a bounded buffer
(`KGLITE_EXPORT_BATCH_ROWS`, default 8192), so memory does not grow with the graph.
Follow the paths in the blueprint rather than building file names from type names.

## RDF 1.2

```python
g.export_rdf("org.nq")                       # N-Quads
g.export_rdf("org.trig")                     # TriG, inferred from the extension
g.export_rdf("org.out", format="trig")       # or named
g.export_rdf("org.nq", base="https://hr.example.org/")

back = kglite.load_rdf("org.nq")
```

`g.export("org.nq")` infers the format the same way. Like the CSV export it streams
statement by statement in bounded batches.

### The `kg:` vocabulary

Every generated IRI lives under a **base** (default `https://kglite.example/`; it
must end with `/` or `#` and must not lie inside a well-known namespace such as
schema.org or FOAF). The layout is fixed, and each variable segment is
percent-encoded:

| Thing | Spelling |
| --- | --- |
| Node | `<base>node/<Type>/<id>` |
| Node type | `<base>type/<Type>`, as the object of `rdf:type` |
| Title | `rdfs:label` |
| Property | `<base>prop/<name>` |
| Relationship | `<base>rel/<TYPE>` |
| Manifest | the statement `<base>meta kg:manifest "<JSON>"^^kg:json` in the named graph `<base>meta` |

The `kg:` namespace is `https://kglite.readthedocs.io/ns/kg#`, with the predicate
`kg:manifest` and the datatypes `kg:json` (a list or map as JSON, in which a date, timestamp, duration, point or
non-finite float is a one-key tagged object such as `{"$date": "2020-01-01"}`) and
`kg:duration` (a duration whose months, days and seconds disagree in sign,
spelled `months,days,seconds`). Nodes that share an id, such as the two versions of
Ada above, are told apart by a `;<node index>` suffix on all but one of them. No
prefixes are declared.

A relationship is written once as a plain statement, `<Ada> <WORKS_IN> <eng>`. An
**edge with properties** also gets a reifier, an RDF 1.2 triple-term statement
that carries the properties:

```text
<base>node/Person/1 <base>rel/WORKS_IN <base>node/Department/eng> .
_:e1 rdf:reifies <<( <base>node/Person/1 <base>rel/WORKS_IN <base>node/Department/eng> )>> .
_:e1 <base>prop/role "lead" .
```

Parallel edges get one reifier each, so their properties stay apart. Property
values are typed literals (`xsd:integer`, `xsd:double`, `xsd:boolean`, `xsd:date`,
`xsd:dateTime` without zone, `geo:wktLiteral` as `POINT(lon lat)`, `xsd:duration`).

### The opt-in schema.org alias

```python
g.export_rdf("org.nq", schema_org=True)
```

adds `schema:validFrom` and `schema:validThrough` statements for the bounds of
declared valid-time intervals, for consumers that look for them. Off by default:
the manifest remains the source of truth because it carries the closed or
half-open convention that schema.org cannot express. The aliases re-import as
ordinary `schema__validFrom` / `schema__validThrough` properties.

### Importing RDF

`load_rdf` reads Turtle, N-Triples, N-Quads and TriG, including RDF 1.2 reifiers
(their properties become edge properties). A file that carries a `kg:manifest`
statement, that is, a KGLite export, gets everything back: declarations, labels,
parent types, ids and titles. The importer adds a `uri` property holding the node
IRI only for RDF *without* a manifest; an exported property that is itself named
`uri` survives.

Language-tagged literals are dropped to plain values by default. Opt in to keep
them as a map per property:

```python
labels = kglite.load_rdf("departments.ttl", language_maps=True)
# n.motto == {"en": "Build", "de": "Bauen"}
```

The `languages` filter applies first. The same switch is the `language_maps`
argument of the C ABI's `kglite_load_rdf_with_options`.

## Limits

- **Not restored** by `export_csv`: an edge attached to an earlier version of a
  node whose id repeats (it re-attaches to the latest), a point on a relationship
  property (text), a secondary label carried by only some nodes of a type, and a
  property column mixing value kinds (text). A node type whose **ids** mix kinds
  (`1` and `'1'`) is refused with an error, since a CSV id column cannot keep them
  apart; export it as RDF or save a `.kgl`. A type whose **titles** mix kinds keeps
  each title's kind.
- **Not restored** by `export_rdf` / `load_rdf`: a language-map property (an
  ordinary map, written as `kg:json`), a secondary label carried by only some nodes
  of a type, an id of a kind other than int or string (the node gets a dense id),
  and a property column mixing value kinds. Mixed int and string ids are kept
  apart, and a null title stays null.
- **Nested typed values** in a list or map (a date, timestamp, duration, point or
  non-finite float, at any depth) keep their type in both formats; see the tagged
  JSON note above.
- A date or timestamp in a CSV cell or an `xsd:date` / `xsd:dateTime` literal
  whose year lies outside 1..9999 is not a date: the CSV cell loads as null with a
  warning, the RDF literal stays text with a warning.
- Parallel edges *without* properties are written as repeated identical
  statements, which a set-semantics RDF consumer collapses.
- `load_rdf` builds an in-memory graph whatever storage the source used.
- The C ABI and the CLI export the whole graph; selections exist only in the
  Python API (`selection_only`).
