// One rule for NaN and the infinities: a JS number carries them in both directions.
import test from 'node:test';
import assert from 'node:assert/strict';
import { freshGraph } from './helpers.mjs';

const { graph, cleanup } = await freshGraph();
test.after(cleanup);

async function roundTrip(value) {
  const r = await graph.executeRead('RETURN $v AS v', { v: value });
  return r.rows[0].v;
}

test('NaN, +Infinity and -Infinity round-trip as numbers', async () => {
  assert.ok(Number.isNaN(await roundTrip(NaN)));
  assert.equal(await roundTrip(Infinity), Infinity);
  assert.equal(await roundTrip(-Infinity), -Infinity);
});

test('negative zero keeps its sign', async () => {
  assert.ok(Object.is(await roundTrip(-0), -0));
});

test('non-finite values nest in lists and survive storage', async () => {
  const list = await roundTrip([NaN, Infinity, -Infinity, 1.5]);
  assert.ok(Number.isNaN(list[0]));
  assert.deepEqual(list.slice(1), [Infinity, -Infinity, 1.5]);
  await graph.executeWrite('CREATE (:F {v: $v})', { v: -Infinity });
  const r = await graph.executeRead('MATCH (n:F) RETURN n.v AS v');
  assert.equal(r.rows[0].v, -Infinity);
});
