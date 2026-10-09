import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as sleep } from 'node:timers/promises';
import { freshGraph } from './helpers.mjs';

const { graph, cleanup } = await freshGraph();
test.after(async () => {
  await graph.close();
  cleanup();
});

const rowsQuery = (n) => `UNWIND range(1, ${n}) AS a RETURN a, a * 2 AS b, toString(a) AS c`;

async function collect(iterable) {
  const out = [];
  for await (const row of iterable) out.push(row);
  return out;
}

for (const n of [0, 1, 10_000]) {
  test(`stream matches executeRead for ${n} rows`, async () => {
    const q = rowsQuery(n);
    const streamed = await collect(graph.stream(q));
    const eager = await graph.executeRead(q);
    assert.equal(streamed.length, n);
    assert.deepEqual(streamed, eager.rows);
  });
}

test('a result of nodes, parameters and options streams like executeRead', async () => {
  await graph.executeWrite('UNWIND range(1, 50) AS i CREATE (:Item {id: i, name: "n" + toString(i)})');
  const q = 'MATCH (n:Item) WHERE n.id > $min RETURN n, n.name AS name ORDER BY n.id';
  const streamed = await collect(graph.stream(q, { min: 10 }, { timeoutMs: 5000 }));
  const eager = await graph.executeRead(q, { min: 10 });
  assert.equal(streamed.length, 40);
  assert.deepEqual(streamed, eager.rows);
  const limited = await collect(graph.stream(q, { min: 10 }, { rowLimit: 5 }));
  assert.equal(limited.length, 5);
});

test('every batch size delivers every row once, in order', async () => {
  for (const batchSize of [1, 2, 7, 100, 999, 1000, 1001, 100_000]) {
    const rows = await collect(graph.stream(rowsQuery(1000), null, { batchSize }));
    assert.equal(rows.length, 1000, `batchSize ${batchSize}`);
    assert.ok(rows.every((r, i) => r.a === i + 1), `order with batchSize ${batchSize}`);
  }
});

test('batchSize and options are validated and reject the iteration', async () => {
  for (const bad of [{ batchSize: 0 }, { batchSize: -1 }, { batchSize: 1.5 }, { batchSize: '10' }, { nope: 1 }]) {
    await assert.rejects(collect(graph.stream('RETURN 1 AS x', null, bad)), (e) => e.code === 'InvalidArgument', JSON.stringify(bad));
  }
  await assert.rejects(collect(graph.stream(42)), (e) => e.code === 'InvalidArgument');
});

test('the iterator protocol: Symbol.asyncIterator returns the stream and exhausted streams stay done', async () => {
  const s = graph.stream('RETURN 1 AS x');
  assert.equal(s[Symbol.asyncIterator](), s);
  assert.deepEqual(await s.next(), { value: { x: 1 }, done: false });
  assert.deepEqual(await s.next(), { value: undefined, done: true });
  assert.deepEqual(await s.next(), { value: undefined, done: true });
});

test('break stops the stream; writes, a fresh stream and close still work', async () => {
  const { graph: g, cleanup: done } = await freshGraph();
  try {
    let seen = 0;
    for await (const row of g.stream(rowsQuery(100_000), null, { batchSize: 10 })) {
      assert.equal(row.a, ++seen);
      if (seen === 25) break;
    }
    assert.equal(seen, 25);
    const w = await g.executeWrite('CREATE (:After {v: 1}) RETURN 1 AS ok');
    assert.equal(w.rows[0].ok, 1);
    const s = g.stream(rowsQuery(10));
    await s.next();
    assert.deepEqual(await s.return(), { value: undefined, done: true });
    assert.deepEqual(await s.next(), { value: undefined, done: true });
    assert.equal((await collect(g.stream('MATCH (n:After) RETURN n.v AS v')))[0].v, 1);
    await g.close();
    assert.equal(g.closed, true);
  } finally {
    done();
  }
});

test('return() before the first next() never runs the query', async () => {
  const s = graph.stream(rowsQuery(10));
  assert.deepEqual(await s.return(), { value: undefined, done: true });
  assert.deepEqual(await s.next(), { value: undefined, done: true });
});

test('a query error rejects the iteration with the typed error, once', async () => {
  const s = graph.stream('MATCH (n RETURN n');
  const expected = await graph.executeRead('MATCH (n RETURN n').then(() => null, (e) => e);
  await assert.rejects(s.next(), (e) => e.name === 'KgliteError' && e.code === expected.code && e.message === expected.message);
  assert.deepEqual(await s.next(), { value: undefined, done: true });
  await assert.rejects(collect(graph.stream('CREATE (:Nope)')), (e) => e.code === 'InvalidArgument');
  assert.equal((await graph.executeRead('MATCH (n:Nope) RETURN count(n) AS c')).rows[0].c, 0);
});

test('an error part-way through rejects after the earlier rows were delivered', async () => {
  // The division fails on the row where the divisor reaches zero.
  const q = 'UNWIND range(5, 0, -1) AS i RETURN 10 / i AS q';
  const got = [];
  let failure;
  try {
    for await (const row of graph.stream(q, null, { batchSize: 2 })) got.push(row);
  } catch (e) {
    failure = e;
  }
  const eager = await graph.executeRead(q).then(
    (r) => ({ rows: r.rows }),
    (e) => ({ error: e }),
  );
  if (eager.error) {
    assert.ok(failure, 'stream rejects when executeRead rejects');
    assert.equal(failure.code, eager.error.code);
  } else {
    assert.equal(failure, undefined);
    assert.deepEqual(got, eager.rows);
  }
});

test('a closed graph rejects Closed; a readOnly graph streams', async () => {
  const { graph: g, dir, cleanup: done } = await freshGraph({ durability: 'full' });
  try {
    await g.executeWrite('CREATE (:T {v: 7})');
    await g.close();
    await assert.rejects(collect(g.stream('RETURN 1 AS x')), (e) => e.code === 'Closed');
    const { open } = (await import('./helpers.mjs')).kglite;
    const ro = await open(`${dir}/g.kgl`, { readOnly: true });
    assert.deepEqual(await collect(ro.stream('MATCH (n:T) RETURN n.v AS v')), [{ v: 7 }]);
    await ro.close();
  } finally {
    done();
  }
});

test('a stream can be consumed while the graph serves other queries', async () => {
  const s = graph.stream(rowsQuery(5000), null, { batchSize: 100 });
  const consumer = collect(s);
  const others = await Promise.all([1, 2, 3].map((i) => graph.executeRead(`RETURN ${i} AS i`)));
  assert.deepEqual(others.map((r) => r.rows[0].i), [1, 2, 3]);
  assert.equal((await consumer).length, 5000);
});

// Event-loop drift. executeRead converts the whole result in one block (about
// 145 ms for 300k x 3 on a debug build); a stream converts batchSize rows per
// turn. Both are compared in one process, so machine speed cancels out.
const BIG = 300_000;

async function maxGapWhile(work) {
  let last = performance.now();
  let max = 0;
  const timer = setInterval(() => {
    const now = performance.now();
    max = Math.max(max, now - last);
    last = now;
  }, 5);
  try {
    await work();
  } finally {
    clearInterval(timer);
  }
  return Math.max(max, performance.now() - last);
}

test('streaming 300k rows keeps the event loop far more responsive than one conversion', async () => {
  const q = rowsQuery(BIG);
  await graph.executeRead(q); // warm plan cache and allocator
  const eagerGap = await maxGapWhile(() => graph.executeRead(q));
  let count = 0;
  const streamGap = await maxGapWhile(async () => {
    for await (const row of graph.stream(q, null, { batchSize: 500 })) count += row.a > 0 ? 1 : 0;
  });
  assert.equal(count, BIG);
  console.log(`# drift: executeRead max gap ${eagerGap.toFixed(0)} ms, stream(batchSize 500) max gap ${streamGap.toFixed(0)} ms`);
  assert.ok(streamGap < eagerGap / 2, `stream gap ${streamGap.toFixed(0)} ms vs one-shot ${eagerGap.toFixed(0)} ms`);
  assert.ok(streamGap < 100, `stream stalled the loop ${streamGap.toFixed(0)} ms`);
});

test('timers fire between batches while rows are being consumed', async () => {
  let ticks = 0;
  const timer = setInterval(() => ticks++, 1);
  let n = 0;
  for await (const _ of graph.stream(rowsQuery(20_000), null, { batchSize: 100 })) n++;
  clearInterval(timer);
  await sleep(0);
  assert.equal(n, 20_000);
  assert.ok(ticks > 0, 'a 1 ms timer ran during the stream');
});

// The cursor tests below stream `MATCH … RETURN`, the shape the engine produces
// a batch at a time, from a real node store rather than a constant range.
test('a MATCH stream equals executeRead, in order, across batch sizes', async () => {
  const { graph: g, cleanup: done } = await freshGraph();
  try {
    await g.executeWrite("UNWIND range(1, 5000) AS i CREATE (:Item {seq: i, name: 'n' + toString(i)})");
    const q = 'MATCH (n:Item) WHERE n.seq % 7 <> 0 RETURN n.seq AS s, n.name AS name';
    const eager = await g.executeRead(q);
    for (const batchSize of [1, 3, 1000, 100_000]) {
      assert.deepEqual(await collect(g.stream(q, null, { batchSize })), eager.rows, `batchSize ${batchSize}`);
    }
    const sorted = 'MATCH (n:Item) RETURN n.seq AS s ORDER BY s DESC';
    assert.deepEqual(await collect(g.stream(sorted)), (await g.executeRead(sorted)).rows);
  } finally {
    await g.close();
    done();
  }
});

test('next() calls made without awaiting still deliver every row once, in order', async () => {
  const { graph: g, cleanup: done } = await freshGraph();
  try {
    await g.executeWrite('UNWIND range(1, 2500) AS i CREATE (:Item {seq: i})');
    const s = g.stream('MATCH (n:Item) RETURN n.seq AS s', null, { batchSize: 100 });
    const results = await Promise.all(Array.from({ length: 2600 }, () => s.next()));
    const values = results.filter((r) => !r.done).map((r) => r.value.s);
    assert.equal(values.length, 2500);
    assert.deepEqual(values, Array.from({ length: 2500 }, (_, i) => i + 1));
    assert.equal(results.filter((r) => r.done).length, 100);
  } finally {
    await g.close();
    done();
  }
});

test('a stream reads the graph as it was at its first next(), whatever commits meanwhile', async () => {
  const { graph: g, cleanup: done } = await freshGraph();
  try {
    await g.executeWrite('UNWIND range(1, 3000) AS i CREATE (:Item {seq: i})');
    const s = g.stream('MATCH (n:Item) RETURN n.seq AS s', null, { batchSize: 100 });
    assert.equal((await s.next()).value.s, 1);
    await g.executeWrite('UNWIND range(1, 500) AS i CREATE (:Item {seq: 100000 + i})');
    await g.executeWrite('MATCH (n:Item) WHERE n.seq <= 1500 DETACH DELETE n');
    let seen = 1;
    for await (const row of s) {
      seen++;
      assert.ok(row.s <= 3000, 'no row from a later commit');
    }
    assert.equal(seen, 3000);
    assert.equal((await g.executeRead('MATCH (n:Item) RETURN count(n) AS c')).rows[0].c, 1500 + 500);
  } finally {
    await g.close();
    done();
  }
});

// Memory. A plain MATCH … RETURN must not be built whole: streaming 600k rows
// may grow the process by a few batches. executeRead of the same query builds
// roughly the whole result (hundreds of MB in a release build), so the bound
// fails against an engine that materialises first.
test('streaming a large MATCH keeps resident memory bounded', async () => {
  const { graph: g, cleanup: done } = await freshGraph();
  try {
    const total = 600_000;
    for (let from = 1; from <= total; from += 100_000) {
      await g.executeWrite(`UNWIND range(${from}, ${from + 99_999}) AS i CREATE (:Item {seq: i, name: 'name-' + toString(i)})`);
    }
    global.gc?.();
    await sleep(200);
    const base = process.memoryUsage().rss;
    let peak = base;
    let n = 0;
    for await (const row of g.stream('MATCH (n:Item) RETURN n.seq AS s, n.name AS name')) {
      if (++n % 5000 === 0) peak = Math.max(peak, process.memoryUsage().rss);
      assert.ok(row.s > 0);
    }
    assert.equal(n, total);
    const growthMb = (peak - base) / 1048576;
    console.log(`# stream rss growth over ${total} rows: ${growthMb.toFixed(0)} MB`);
    assert.ok(growthMb < 120, `stream grew resident memory by ${growthMb.toFixed(0)} MB`);
  } finally {
    await g.close();
    done();
  }
});
