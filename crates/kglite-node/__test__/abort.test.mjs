import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as sleep } from 'node:timers/promises';

// Two workers make "fill the pool" cheap; the pool is created on first use.
process.env.KGLITE_NODE_THREADS = '2';
const { freshGraph } = await import('./helpers.mjs');

const { graph, cleanup } = await freshGraph();
test.after(async () => {
  await graph.close();
  cleanup();
});

/** Runs for minutes unless cancelled. */
const LONG = 'UNWIND range(1, 200000) AS a UNWIND range(1, 200000) AS b RETURN count(*) AS c';
const LONG_WRITE = 'UNWIND range(1, 200000) AS a UNWIND range(1, 200000) AS b CREATE (:Junk {a: a, b: b})';
const BOUND_MS = 5000;

async function rejection(promise) {
  try {
    await promise;
  } catch (e) {
    return e;
  }
  assert.fail('expected a rejection');
}

async function count(label) {
  const r = await graph.executeRead(`MATCH (n:${label}) RETURN count(n) AS c`);
  return Number(r.rows[0].c);
}

test('a pre-aborted signal rejects Cancelled without running', async () => {
  const reason = new Error('nope');
  const ac = new AbortController();
  ac.abort(reason);
  for (const call of [
    () => graph.executeRead('RETURN 1', null, { signal: ac.signal }),
    () => graph.executeWrite('CREATE (:Pre)', null, { signal: ac.signal }),
  ]) {
    const e = await rejection(call());
    assert.equal(e.code, 'Cancelled');
  }
  assert.equal(await count('Pre'), 0);
  const tx = await graph.begin();
  const e = await rejection(tx.run('CREATE (:Pre)', null, { signal: ac.signal }));
  assert.equal(e.code, 'Cancelled');
  await tx.rollback();
  const s = graph.stream('RETURN 1', null, { signal: ac.signal });
  assert.equal((await rejection(s.next())).code, 'Cancelled');
  assert.equal((await s.next()).done, true);
});

test('abort mid-query rejects promptly with the cause and leaves the graph usable', async () => {
  const ac = new AbortController();
  const started = Date.now();
  const pending = graph.executeRead(LONG, null, { signal: ac.signal });
  await sleep(150);
  const reason = new Error('stop');
  ac.abort(reason);
  const timer = sleep(BOUND_MS).then(() => 'timeout');
  const e = await Promise.race([rejection(pending), timer]);
  assert.notEqual(e, 'timeout', `still running ${BOUND_MS} ms after abort`);
  assert.equal(e.code, 'Cancelled');
  assert.equal(e.cause, reason);
  assert.ok(Date.now() - started < BOUND_MS, `took ${Date.now() - started} ms`);
  const r = await graph.executeRead('RETURN 1 AS one');
  assert.equal(r.rows[0].one, 1);
});

test('abort while queued rejects at once and the call never runs', async () => {
  const blockers = [new AbortController(), new AbortController()];
  const running = blockers.map((b) => graph.executeRead(LONG, null, { signal: b.signal }));
  await sleep(200);
  const ac = new AbortController();
  const queued = graph.executeRead('RETURN 1 AS one', null, { signal: ac.signal });
  await sleep(50);
  const t0 = Date.now();
  ac.abort();
  const e = await rejection(queued);
  assert.equal(e.code, 'Cancelled');
  assert.ok(Date.now() - t0 < 1000, 'queued abort settles without waiting for a worker');
  assert.ok(e.cause instanceof Error, 'cause defaults to the signal reason (an AbortError)');
  blockers.forEach((b) => b.abort());
  await Promise.all(running.map(rejection));
});

// Writes run on their own thread, so a queued write waits behind another write,
// not behind the read workers.
test('abort while a write is queued behind a write never runs it', async () => {
  const blocker = new AbortController();
  const running = graph.executeWrite(LONG_WRITE, null, { signal: blocker.signal });
  await sleep(200);
  const ac = new AbortController();
  const queued = graph.executeWrite('CREATE (:Queued)', null, { signal: ac.signal });
  await sleep(50);
  const t0 = Date.now();
  ac.abort();
  const e = await rejection(queued);
  assert.equal(e.code, 'Cancelled');
  assert.ok(Date.now() - t0 < 1000, 'queued abort settles without waiting for the writer');
  blocker.abort();
  await rejection(running);
  assert.equal(await count('Queued'), 0);
});

test('a cancelled executeWrite publishes nothing', async () => {
  const before = await graph.executeRead('MATCH (n) RETURN count(n) AS c');
  const ac = new AbortController();
  const pending = graph.executeWrite(LONG_WRITE, null, { signal: ac.signal });
  await sleep(150);
  ac.abort();
  const e = await rejection(pending);
  assert.equal(e.code, 'Cancelled');
  assert.equal(await count('Junk'), 0);
  const after = await graph.executeRead('MATCH (n) RETURN count(n) AS c');
  assert.equal(after.rows[0].c, before.rows[0].c);
  await graph.executeWrite('CREATE (:AfterCancel)');
  assert.equal(await count('AfterCancel'), 1);
});

test('a cancelled transaction write fails the statement and aborts the transaction', async () => {
  const tx = await graph.begin();
  await tx.run('CREATE (:TxKept)');
  const ac = new AbortController();
  const pending = tx.run(LONG_WRITE, null, { signal: ac.signal });
  await sleep(150);
  ac.abort();
  assert.equal((await rejection(pending)).code, 'Cancelled');
  const e = await rejection(tx.run('RETURN 1'));
  assert.equal(e.code, 'TransactionClosed');
  await rejection(tx.commit());
  assert.equal(await count('TxKept'), 0);
  assert.equal(await count('Junk'), 0);
});

test('a cancelled read in a transaction leaves the transaction open', async () => {
  const tx = await graph.begin();
  const ac = new AbortController();
  const pending = tx.run(LONG, null, { signal: ac.signal });
  await sleep(150);
  ac.abort();
  assert.equal((await rejection(pending)).code, 'Cancelled');
  await tx.run('CREATE (:TxAfterRead)');
  await tx.commit();
  assert.equal(await count('TxAfterRead'), 1);
});

test('aborting a stream stops it and rejects the pending next()', async () => {
  const ac = new AbortController();
  const s = graph.stream(LONG, null, { signal: ac.signal });
  const pending = s.next();
  await sleep(150);
  const t0 = Date.now();
  ac.abort();
  const e = await rejection(pending);
  assert.equal(e.code, 'Cancelled');
  assert.ok(e.cause instanceof Error);
  assert.ok(Date.now() - t0 < BOUND_MS);
  assert.equal((await s.next()).done, true);
});

test('aborting between batches stops further batches', async () => {
  const ac = new AbortController();
  const s = graph.stream('UNWIND range(1, 5000) AS a RETURN a', null, { signal: ac.signal, batchSize: 100 });
  for (let i = 0; i < 150; i++) assert.equal((await s.next()).done, false);
  ac.abort();
  assert.equal((await rejection(s.next())).code, 'Cancelled');
  assert.equal((await s.next()).done, true);
});

test('a finished stream and a settled call remove their listeners', async () => {
  const ac = new AbortController();
  let adds = 0;
  let removes = 0;
  const add = ac.signal.addEventListener.bind(ac.signal);
  const remove = ac.signal.removeEventListener.bind(ac.signal);
  ac.signal.addEventListener = (...a) => (adds++, add(...a));
  ac.signal.removeEventListener = (...a) => (removes++, remove(...a));
  await graph.executeRead('RETURN 1', null, { signal: ac.signal });
  const rows = [];
  for await (const r of graph.stream('UNWIND range(1, 5) AS a RETURN a', null, { signal: ac.signal })) rows.push(r);
  for await (const r of graph.stream('UNWIND range(1, 5) AS a RETURN a', null, { signal: ac.signal })) {
    void r;
    break;
  }
  assert.equal(rows.length, 5);
  assert.equal(adds, 3);
  assert.equal(removes, 3);
});

test('many calls on one signal raise no MaxListeners warning', async () => {
  const warnings = [];
  const onWarning = (w) => warnings.push(w);
  process.on('warning', onWarning);
  const ac = new AbortController();
  await Promise.all(Array.from({ length: 200 }, () => graph.executeRead('RETURN 1', null, { signal: ac.signal })));
  for (let i = 0; i < 200; i++) await graph.executeRead('RETURN 1', null, { signal: ac.signal });
  await sleep(20);
  process.off('warning', onWarning);
  assert.deepEqual(warnings, []);
  ac.abort();
});

test('aborting one query leaves a concurrent one untouched', async () => {
  const a = new AbortController();
  const b = new AbortController();
  const doomed = graph.executeRead(LONG, null, { signal: a.signal });
  const survivor = graph.executeRead(
    'UNWIND range(1, 3000) AS a UNWIND range(1, 1500) AS b RETURN count(*) AS c',
    null,
    { signal: b.signal },
  );
  await sleep(50);
  a.abort();
  assert.equal((await rejection(doomed)).code, 'Cancelled');
  const r = await survivor;
  assert.equal(Number(r.rows[0].c), 4_500_000);
});

test('an invalid signal is an argument error', async () => {
  const e = await rejection(graph.executeRead('RETURN 1', null, { signal: {} }));
  assert.equal(e.code, 'InvalidArgument');
});
