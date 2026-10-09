// Issue #222 acceptance criteria through the Node binding: declareOntology, write-time
// enforcement, the structured OntologyViolation error, declare-over-data refusal.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { kglite, freshGraph } from './helpers.mjs';

const ontology = (level = 'error') => ({
  classes: {
    Person: { required_properties: ['name'], property_types: { name: 'string' }, enforcement: level },
    Company: {},
  },
  relationships: { WORKS_AT: { domain: 'Person', range: 'Company', enforcement: level } },
});

async function declared(t, level = 'error') {
  const g = await freshGraph();
  t.after(async () => {
    await g.graph.close();
    g.cleanup();
  });
  const out = await g.graph.declareOntology(ontology(level));
  return { ...g, out };
}

const count = async (graph) => (await graph.executeRead('MATCH (n) RETURN count(n) AS c')).rows[0].c;

const refused = (graph, cypher, want) =>
  assert.rejects(graph.executeWrite(cypher), (e) => {
    assert.equal(e.name, 'KgliteError');
    assert.equal(e.code, 'OntologyViolation');
    assert.deepEqual([e.rule, e.entity, e.entityType, e.property], want);
    assert.deepEqual(e.report, []);
    return true;
  });

test('AC2: a valid write succeeds and declare returns warnings', async (t) => {
  const { graph, out } = await declared(t);
  assert.deepEqual(Object.keys(out), ['warnings']);
  // Declaring before any data exists draws the engine's "concrete class has no nodes" findings.
  assert.ok(out.warnings.length > 0 && out.warnings.every((w) => typeof w === 'string'));
  assert.ok(out.warnings.some((w) => w.includes('Person')));
  await graph.executeWrite("CREATE (:Person {id: 1, name: 'A'})");
  assert.equal(await count(graph), 1);
});

test('AC3/AC7: a missing required property fails, names rule/type/property, nothing persisted', async (t) => {
  const { graph } = await declared(t);
  await refused(graph, 'CREATE (:Person {id: 2})', ['required_property', 'node', 'Person', 'name']);
  assert.equal(await count(graph), 0);
});

test('AC4: a wrong property type fails', async (t) => {
  const { graph } = await declared(t);
  await refused(graph, 'CREATE (:Person {id: 3, name: 5})', ['property_type', 'node', 'Person', 'name']);
  assert.equal(await count(graph), 0);
});

test('AC5: relationship domain and range are enforced', async (t) => {
  const { graph } = await declared(t);
  await refused(
    graph,
    "CREATE (:Company {id: 11, name: 'Acme'})-[:WORKS_AT]->(:Person {id: 4, name: 'B'})",
    ['domain', 'relationship', 'WORKS_AT', null],
  );
  assert.equal(await count(graph), 0);
  await graph.executeWrite("CREATE (:Person {id: 5, name: 'B'})-[:WORKS_AT]->(:Company {id: 12})");
  assert.equal(await count(graph), 2);
});

test('AC6: a violation in statement 3 of a transaction rolls back statements 1 and 2', async (t) => {
  const { graph } = await declared(t);
  const tx = await graph.begin();
  await tx.run("CREATE (:Person {id: 1, name: 'A'})");
  await tx.run("CREATE (:Person {id: 2, name: 'B'})");
  await assert.rejects(tx.run('CREATE (:Person {id: 3})'), (e) => {
    assert.equal(e.code, 'OntologyViolation');
    assert.equal(e.rule, 'required_property');
    assert.equal(e.entityType, 'Person');
    assert.equal(e.property, 'name');
    return true;
  });
  await assert.rejects(tx.commit());
  assert.equal(await count(graph), 0);

  await assert.rejects(
    graph.transaction(async (inner) => {
      await inner.run("CREATE (:Person {id: 1, name: 'A'})");
      await inner.run('CREATE (:Person {id: 9, name: 9})');
    }),
    (e) => e.code === 'OntologyViolation' && e.rule === 'property_type',
  );
  assert.equal(await count(graph), 0);
});

test('AC8: warn level accepts the write and reports the violation', async (t) => {
  const { graph } = await declared(t, 'warn');
  const result = await graph.executeWrite('CREATE (:Person {id: 2})');
  assert.equal(await count(graph), 1);
  assert.ok(
    result.warnings.some((w) => /name/.test(w)),
    `expected a warning naming the property, got ${JSON.stringify(result.warnings)}`,
  );
});

test('AC9: declaring over violating data is refused with a report; the old ontology stays', async (t) => {
  const g = await freshGraph();
  t.after(async () => {
    await g.graph.close();
    g.cleanup();
  });
  const { graph } = g;
  await graph.executeWrite('CREATE (:Person {id: 1}), (:Person {id: 2})');
  await assert.rejects(graph.declareOntology(ontology()), (e) => {
    assert.equal(e.code, 'OntologyViolation');
    assert.equal(e.rule, 'required_property');
    assert.equal(e.entityType, 'Person');
    assert.equal(e.property, 'name');
    assert.ok(Array.isArray(e.report) && e.report.length > 0);
    const entry = e.report.find((r) => r.rule === 'required_property');
    assert.deepEqual(entry, {
      rule: 'required_property',
      entity: 'node',
      entityType: 'Person',
      property: 'name',
      count: 2,
    });
    return true;
  });
  // Nothing was installed: an invalid write is still accepted.
  await graph.executeWrite('CREATE (:Person {id: 3})');
  assert.equal(await count(graph), 3);
});

test('a JSON string declares the same ontology as the object', async (t) => {
  const g = await freshGraph();
  t.after(async () => {
    await g.graph.close();
    g.cleanup();
  });
  await g.graph.declareOntology(JSON.stringify(ontology()));
  await refused(g.graph, 'CREATE (:Person {id: 2})', ['required_property', 'node', 'Person', 'name']);
});

test('clearOntology lifts enforcement; a bad declaration is InvalidArgument', async (t) => {
  const { graph } = await declared(t);
  await graph.clearOntology();
  await graph.executeWrite('CREATE (:Person {id: 2})');
  await assert.rejects(graph.declareOntology('{not json'), (e) => e.code === 'InvalidArgument');
  await assert.rejects(graph.declareOntology(5), (e) => e.code === 'InvalidArgument');
  await assert.rejects(graph.declareOntology({ classes: 'no' }), (e) => e.code === 'InvalidArgument');
});

test('AC1: the declaration is persisted and survives a reopen', async (t) => {
  const dir = mkdtempSync(join(tmpdir(), 'kglite-node-ont-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const path = join(dir, 'g.kgl');
  const first = await kglite.open(path, { durability: 'normal' });
  await first.declareOntology(ontology());
  await first.close();
  const again = await kglite.open(path, { durability: 'normal' });
  t.after(() => again.close());
  await refused(again, 'CREATE (:Person {id: 2})', ['required_property', 'node', 'Person', 'name']);
});

test('a readOnly graph rejects declareOntology and clearOntology with ReadOnly', async (t) => {
  const dir = mkdtempSync(join(tmpdir(), 'kglite-node-ont-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const path = join(dir, 'g.kgl');
  const w = await kglite.open(path, { durability: 'off' });
  await w.close();
  const r = await kglite.open(path, { readOnly: true });
  t.after(() => r.close());
  await assert.rejects(r.declareOntology(ontology()), (e) => e.code === 'ReadOnly');
  await assert.rejects(r.clearOntology(), (e) => e.code === 'ReadOnly');
});
