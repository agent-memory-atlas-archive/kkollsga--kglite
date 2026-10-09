// Driven by tests/test_node_python_parity.py (not by `node --test`).
//
//   node parity_harness.mjs write <path> <durability> <plan-json>
//   node parity_harness.mjs read  <path> <writer|readonly>
//
// A plan is {batches: [{people, knows}, ...], ending: "close" | "abandon"}. Each
// batch but the last is followed by checkpoint(). `close` ends with close();
// `abandon` ends with sync() and a hard exit, leaving the last batch only in
// the write-ahead log. Integers beyond 2^53 travel as {"$bigint": "<digits>"}.
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const kglite = require('../index.js');

const revive = (_k, v) => (v && typeof v === 'object' && '$bigint' in v ? BigInt(v.$bigint) : v);
const encode = (_k, v) => (typeof v === 'bigint' ? { $bigint: v.toString() } : v);

const CREATE_PEOPLE =
  'UNWIND $people AS p CREATE (:Person {id: p.id, name: p.name, score: p.score, active: p.active, born: date(p.born), big: p.big})';
const CREATE_KNOWS =
  'UNWIND $knows AS k MATCH (a:Person {id: k.from}), (b:Person {id: k.to}) CREATE (a)-[:KNOWS {since: k.since}]->(b)';

const [mode, path, arg3, arg4] = process.argv.slice(2);

if (mode === 'write') {
  const plan = JSON.parse(arg4, revive);
  const graph = await kglite.open(path, { durability: arg3 });
  for (const [i, batch] of plan.batches.entries()) {
    await graph.executeWrite(CREATE_PEOPLE, { people: batch.people });
    await graph.executeWrite(CREATE_KNOWS, { knows: batch.knows });
    if (i < plan.batches.length - 1) await graph.checkpoint();
  }
  if (plan.ending === 'close') {
    await graph.close();
  } else {
    await graph.sync();
    process.exit(0);
  }
} else {
  const graph = await kglite.open(path, arg3 === 'readonly' ? { readOnly: true } : { durability: 'full' });
  const people = (
    await graph.executeRead(
      'MATCH (p:Person) RETURN p.id AS id, p.name AS name, p.score AS score, p.active AS active, toString(p.born) AS born, p.big AS big ORDER BY id',
    )
  ).rows;
  const knows = (
    await graph.executeRead('MATCH (a:Person)-[r:KNOWS]->(b:Person) RETURN a.id AS from, b.id AS to, r.since AS since ORDER BY from')
  ).rows;
  console.log(`PARITY_JSON:${JSON.stringify({ people, knows }, encode)}`);
  await graph.close();
}
