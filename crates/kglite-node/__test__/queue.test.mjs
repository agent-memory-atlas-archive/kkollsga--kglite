import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as sleep } from 'node:timers/promises';

const { freshGraph } = await import('./helpers.mjs');

/** Occupies the single writer thread until aborted. */
const LONG_WRITE = 'UNWIND range(1, 200000) AS a UNWIND range(1, 200000) AS b CREATE (:Junk {a: a, b: b})';
const CAPACITY = 4096;

/** Start a write that holds the writer thread; resolves to a `release` function. */
async function blockWriter(graph) {
  const ac = new AbortController();
  const held = graph.executeWrite(LONG_WRITE, null, { signal: ac.signal }).catch((e) => e);
  await sleep(300);
  return async () => {
    ac.abort();
    const e = await held;
    assert.equal(e.code, 'Cancelled');
  };
}

async function open(options) {
  const g = await freshGraph(options);
  test.after(async () => {
    await g.graph.close();
    g.cleanup();
  });
  return g.graph;
}

test('the default policy rejects QueueFull past the queue capacity', async () => {
  const graph = await open();
  const release = await blockWriter(graph);
  const calls = [];
  for (let i = 0; i < CAPACITY + 500; i++) calls.push(graph.executeWrite('CREATE (:Dflt {i: $i})', { i }));
  const settled = await Promise.allSettled(calls.slice(CAPACITY));
  assert.ok(settled.length > 0 && settled.every((r) => r.status === 'rejected' && r.reason.code === 'QueueFull'));
  await release();
  await Promise.allSettled(calls);
});

test("onQueueFull: 'wait' runs a 20k burst in order with no QueueFull", async () => {
  const graph = await open({ onQueueFull: 'wait' });
  const release = await blockWriter(graph);
  const calls = [];
  for (let i = 0; i < 20000; i++) calls.push(graph.executeWrite('CREATE (:Burst {seq: $i})', { i }));
  await sleep(100);
  await release();
  const settled = await Promise.allSettled(calls);
  const failed = settled.filter((r) => r.status === 'rejected');
  assert.equal(failed.length, 0, failed[0] && String(failed[0].reason));
  const rows = (await graph.executeRead('MATCH (n:Burst) RETURN n.seq AS s')).rows.map((r) => Number(r.s));
  assert.equal(rows.length, 20000);
  assert.ok(rows.every((s, i) => s === i), 'writes ran in arrival order');
});

test("a call waiting under 'wait' stays abortable", async () => {
  const graph = await open({ onQueueFull: 'wait' });
  const release = await blockWriter(graph);
  const filler = [];
  for (let i = 0; i < CAPACITY; i++) filler.push(graph.executeWrite('CREATE (:Filler {i: $i})', { i }));
  const ac = new AbortController();
  const waiting = graph.executeWrite('CREATE (:Aborted)', null, { signal: ac.signal });
  const extra = graph.executeWrite('CREATE (:Late)');
  await sleep(50);
  const reason = new Error('stop waiting');
  ac.abort(reason);
  const started = Date.now();
  const e = await waiting.catch((err) => err);
  assert.equal(e.code, 'Cancelled');
  assert.equal(e.cause, reason);
  assert.ok(Date.now() - started < 1000, 'rejected while the writer was still blocked');
  await release();
  await Promise.all([...filler, extra]);
  const count = async (label) =>
    Number((await graph.executeRead(`MATCH (n:${label}) RETURN count(n) AS c`)).rows[0].c);
  assert.equal(await count('Aborted'), 0, 'the aborted call never ran');
  assert.equal(await count('Late'), 1);
});

test('onQueueFull rejects an unknown value', async () => {
  await assert.rejects(freshGraph({ onQueueFull: 'drop' }), (e) => e.code === 'InvalidArgument');
});
