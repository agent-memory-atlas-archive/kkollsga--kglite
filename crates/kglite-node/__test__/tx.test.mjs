import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { runInNewContext } from 'node:vm';
import { setFlagsFromString } from 'node:v8';
import { crashChild, freshGraph, kglite, startChild } from './helpers.mjs';

async function count(graph, label = 'Item') {
  return (await graph.executeRead(`MATCH (n:${label}) RETURN count(n) AS c`)).rows[0].c;
}

async function rejects(promise, code) {
  await assert.rejects(promise, (e) => {
    assert.equal(e.code, code, `expected ${code}, got ${e.code}: ${e.message}`);
    return true;
  });
}

async function withGraph(t, options) {
  const fresh = await freshGraph(options);
  t.after(async () => {
    await fresh.graph.close();
    fresh.cleanup();
  });
  return fresh.graph;
}

test('a transaction sees its own writes; nothing is visible until commit', async (t) => {
  const g = await withGraph(t);
  const tx = await g.begin();
  await tx.run('CREATE (:Item {i: 1})');
  assert.equal((await tx.run('MATCH (n:Item) RETURN count(n) AS c')).rows[0].c, 1);
  assert.equal(await count(g), 0);
  await tx.commit();
  assert.equal(tx.finished, true);
  assert.equal(await count(g), 1);
});

test('rollback discards the writes and is idempotent, also after commit', async (t) => {
  const g = await withGraph(t);
  const tx = await g.begin();
  await tx.run('CREATE (:Item {i: 1})');
  await tx.rollback();
  await tx.rollback();
  assert.equal(await count(g), 0);
  await rejects(tx.commit(), 'TransactionClosed');
  await rejects(tx.run('RETURN 1'), 'TransactionClosed');

  const done = await g.begin();
  await done.run('CREATE (:Item {i: 2})');
  await done.commit();
  await done.rollback();
  assert.equal(await count(g), 1, 'rollback after commit must not undo it');
  await rejects(done.commit(), 'TransactionClosed');
});

test('a lost optimistic race rejects TransactionConflict and publishes nothing', async (t) => {
  const g = await withGraph(t);
  const a = await g.begin();
  const b = await g.begin();
  await a.run('CREATE (:Item {who: "a"})');
  await b.run('CREATE (:Item {who: "b"})');
  await a.commit();
  await rejects(b.commit(), 'TransactionConflict');
  const rows = (await g.executeRead('MATCH (n:Item) RETURN n.who AS who')).rows;
  assert.deepEqual(rows, [{ who: 'a' }]);
});

test('transaction(fn) commits on resolve and returns the callback value', async (t) => {
  const g = await withGraph(t);
  const value = await g.transaction(async (tx) => {
    await tx.run('CREATE (:Item {i: 1})');
    return 'done';
  });
  assert.equal(value, 'done');
  assert.equal(await count(g), 1);
});

test('transaction(fn) rolls back and rethrows when the callback throws', async (t) => {
  const g = await withGraph(t);
  const boom = new Error('boom');
  await assert.rejects(
    g.transaction(async (tx) => {
      await tx.run('CREATE (:Item {i: 1})');
      throw boom;
    }),
    (e) => e === boom,
  );
  assert.equal(await count(g), 0, 'a throwing callback must leave no write behind');
});

test('transaction(fn) rejects a non-function callback', async (t) => {
  const g = await withGraph(t);
  await rejects(g.transaction(42), 'InvalidArgument');
  await rejects(g.transaction(async () => {}, { retries: -1 }), 'InvalidArgument');
  await rejects(g.transaction(async () => {}, { retrys: 1 }), 'InvalidArgument');
});

/** A callback that loses the optimistic race on its first attempt only. */
function racingCallback(g, attempts) {
  return async (tx) => {
    attempts.n++;
    await tx.run('CREATE (:Item {i: 1})');
    if (attempts.n === 1) await g.executeWrite('CREATE (:Item {i: 99})');
  };
}

test('transaction(fn, {retries}) re-runs the callback after a lost race', async (t) => {
  const g = await withGraph(t);
  const attempts = { n: 0 };
  await g.transaction(racingCallback(g, attempts), { retries: 1 });
  assert.equal(attempts.n, 2, 'the callback must have been retried once');
  assert.equal(await count(g), 2, 'the rival write and exactly one copy of the retried write');
});

test('transaction(fn) without retries surfaces the conflict', async (t) => {
  const g = await withGraph(t);
  const attempts = { n: 0 };
  await rejects(g.transaction(racingCallback(g, attempts)), 'TransactionConflict');
  assert.equal(attempts.n, 1);
  assert.equal(await count(g), 1, 'only the rival write is published');
});

test('retries are bounded and only for conflicts', async (t) => {
  const g = await withGraph(t);
  const attempts = { n: 0 };
  await rejects(
    g.transaction(
      async (tx) => {
        attempts.n++;
        await tx.run('CREATE (:Item {i: 1})');
        await g.executeWrite('CREATE (:Item {i: 99})');
      },
      { retries: 2 },
    ),
    'TransactionConflict',
  );
  assert.equal(attempts.n, 3, 'one try plus two retries, then the conflict is surfaced');

  const other = { n: 0 };
  await rejects(
    g.transaction(
      async (tx) => {
        other.n++;
        await tx.run('THIS IS NOT CYPHER');
      },
      { retries: 5 },
    ),
    'CypherSyntax',
  );
  assert.equal(other.n, 1, 'a non-conflict failure is never retried');
});

test('a read-only transaction reads and refuses writes without aborting', async (t) => {
  const g = await withGraph(t);
  await g.executeWrite('CREATE (:Item {i: 1})');
  const tx = await g.begin({ readOnly: true });
  assert.equal(tx.readOnly, true);
  await rejects(tx.run('CREATE (:Item {i: 2})'), 'ReadOnly');
  assert.equal((await tx.run('MATCH (n:Item) RETURN count(n) AS c')).rows[0].c, 1);
  await tx.commit();
  assert.equal(await count(g), 1);
});

test('a readOnly graph only begins read-only transactions', async (t) => {
  const { graph, dir, cleanup } = await freshGraph({ durability: 'full' });
  await graph.executeWrite('CREATE (:Item {i: 1})');
  await graph.close();
  const ro = await kglite.open(join(dir, 'g.kgl'), { readOnly: true });
  t.after(async () => {
    await ro.close();
    cleanup();
  });
  await rejects(ro.begin({ readOnly: false }), 'ReadOnly');
  const tx = await ro.begin();
  assert.equal(tx.readOnly, true);
  assert.equal((await tx.run('MATCH (n:Item) RETURN count(n) AS c')).rows[0].c, 1);
  await tx.rollback();
});

test('a failed write statement aborts the transaction', async (t) => {
  const g = await withGraph(t);
  const tx = await g.begin();
  await tx.run('CREATE (:Item {i: 1})');
  await rejects(tx.run('CREATE (:Item {i: 2}) WITH 1/0 AS x RETURN x'), 'CypherExecution');
  await rejects(tx.run('RETURN 1'), 'TransactionClosed');
  await rejects(tx.commit(), 'TransactionClosed');
  assert.equal(await count(g), 0);
});

const PERSON = JSON.stringify({
  classes: { Person: { required_properties: ['email'], enforcement: 'error' } },
});

test('a violation in statement 3 rolls back statements 1 and 2', async (t) => {
  const g = await withGraph(t);
  await g.executeWrite('CALL db.ontology.declare({ontology: $o})', { o: PERSON });
  await rejects(
    g.transaction(async (tx) => {
      await tx.run('CREATE (:Person {id: 1, email: "a@x"})');
      await tx.run('CREATE (:Person {id: 2, email: "b@x"})');
      await tx.run('CREATE (:Person {id: 3})');
    }),
    'OntologyViolation',
  );
  assert.equal(await count(g, 'Person'), 0, 'statements 1 and 2 must not survive');

  const tx = await g.begin();
  await tx.run('CREATE (:Person {id: 1, email: "a@x"})');
  await rejects(tx.run('CREATE (:Person {id: 3})'), 'OntologyViolation');
  await rejects(tx.commit(), 'TransactionClosed');
  assert.equal(await count(g, 'Person'), 0);
});

test('close() rolls back open transactions', async (t) => {
  const { graph, dir, cleanup } = await freshGraph({ durability: 'full' });
  t.after(cleanup);
  const open = await graph.begin();
  await open.run('CREATE (:Item {i: 1})');
  await graph.close();
  assert.equal(open.finished, true);
  await rejects(open.run('RETURN 1'), 'Closed');
  await rejects(open.commit(), 'Closed');
  await open.rollback();
  await rejects(graph.begin(), 'Closed');
  const again = await kglite.open(join(dir, 'g.kgl'), { durability: 'full' });
  assert.equal(await count(again), 0, 'the open transaction left nothing behind');
  await again.close();
});

test('a transaction dropped without commit or rollback is released', async (t) => {
  const g = await withGraph(t);
  setFlagsFromString('--expose-gc');
  const gc = runInNewContext('gc');
  await (async () => {
    const tx = await g.begin();
    await tx.run('CREATE (:Item {i: 1})');
    assert.ok(kglite.__liveTransactions() >= 1, 'the counter must see a live transaction');
  })();
  // Every transaction any earlier test left behind is garbage too, so the whole
  // process drains to zero once the collector has run.
  let live = kglite.__liveTransactions();
  for (let i = 0; i < 200 && live > 0; i++) {
    gc();
    await sleep(10);
    live = kglite.__liveTransactions();
  }
  assert.equal(live, 0, 'a dropped transaction was never released');
  await g.executeWrite('CREATE (:Item {i: 2})');
  assert.equal(await count(g), 1, 'the dropped transaction published nothing');
});

test('Symbol.asyncDispose rolls a transaction back', async (t) => {
  const g = await withGraph(t);
  // `await using` is syntax Node 20/22 cannot parse; this is what it calls.
  const tx = await g.begin();
  await tx.run('CREATE (:Item {i: 1})');
  await tx[Symbol.asyncDispose]();
  assert.equal(tx.finished, true);
  assert.equal(await count(g), 0);
  await rejects(tx.commit(), 'TransactionClosed');
  await g[Symbol.asyncDispose]();
  assert.equal(g.closed, true);
});

test('a committed transaction survives SIGKILL at durability full; an open one does not', async (t) => {
  const dir = mkdtempSync(join(tmpdir(), 'kglite-node-tx-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const path = join(dir, 'g.kgl');
  const child = startChild('tx', path, 'full');
  try {
    await child.next((l) => l === 'ready');
  } finally {
    await crashChild(child);
  }
  const g = await kglite.open(path, { durability: 'full' });
  const ids = (await g.executeRead('MATCH (n:Item) RETURN n.i AS i ORDER BY i')).rows.map((r) => r.i);
  await g.close();
  assert.deepEqual(ids, [0, 1, 2]);
});
