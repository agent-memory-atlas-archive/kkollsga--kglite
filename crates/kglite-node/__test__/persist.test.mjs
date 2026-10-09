import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { kglite } from './helpers.mjs';

function scratch(t) {
  const dir = mkdtempSync(join(tmpdir(), 'kglite-node-persist-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

// AC2: open -> write -> close -> reopen -> read, at every durability level.
// `off` writes no log, so this only passes because close() checkpoints.
for (const durability of ['full', 'normal', 'off']) {
  test(`data survives close and reopen at durability '${durability}'`, async (t) => {
    const path = join(scratch(t), 'g.kgl');
    const g = await kglite.open(path, { durability });
    assert.equal(g.durability, durability);
    await g.executeWrite('CREATE (:City {name: $n, pop: $p})-[:NEAR {km: 12}]->(:City {name: "Oslo", pop: 700000})', {
      n: 'Drammen',
      p: 100000,
    });
    await g.close();
    assert.equal(g.closed, true);

    const again = await kglite.open(path, { durability });
    const rows = (
      await again.executeRead('MATCH (a:City)-[r:NEAR]->(b:City) RETURN a.name AS a, a.pop AS p, r.km AS km, b.name AS b')
    ).rows;
    assert.deepEqual(rows, [{ a: 'Drammen', p: 100000, km: 12, b: 'Oslo' }]);
    await again.close();
  });
}

test('close() is idempotent and later calls reject Closed', async (t) => {
  const g = await kglite.open(join(scratch(t), 'g.kgl'), { durability: 'normal' });
  await g.executeWrite('CREATE (:N)');
  await g.close();
  await g.close();
  for (const call of [
    () => g.executeRead('RETURN 1'),
    () => g.executeWrite('CREATE (:N)'),
    () => g.checkpoint(),
    () => g.sync(),
  ]) {
    await assert.rejects(call(), (e) => e.code === 'Closed' && e.name === 'KgliteError');
  }
  assert.equal(g.path.endsWith('g.kgl'), true);
});

test('checkpoint() writes once and is a no-op until something changes', async (t) => {
  const path = join(scratch(t), 'g.kgl');
  const g = await kglite.open(path, { durability: 'off' });
  await g.executeWrite('CREATE (:N)');
  await g.checkpoint();
  const first = statSync(path).mtimeMs;
  await sleep(30);
  await g.checkpoint();
  assert.equal(statSync(path).mtimeMs, first, 'an unchanged graph is not rewritten');
  await g.executeWrite('CREATE (:N)');
  await sleep(30);
  await g.checkpoint();
  assert.ok(statSync(path).mtimeMs > first, 'a changed graph is rewritten');
  await g.close();
});

test('an untouched existing graph is not rewritten by close()', async (t) => {
  const path = join(scratch(t), 'g.kgl');
  const g = await kglite.open(path, { durability: 'off' });
  await g.executeWrite('CREATE (:N)');
  await g.close();
  const written = statSync(path).mtimeMs;
  await sleep(30);
  const again = await kglite.open(path, { durability: 'off' });
  await again.close();
  assert.equal(statSync(path).mtimeMs, written);
});

test('sync() flushes under normal/full and rejects NotDurable under off', async (t) => {
  const dir = scratch(t);
  for (const durability of ['normal', 'full']) {
    const g = await kglite.open(join(dir, `${durability}.kgl`), { durability });
    await g.executeWrite('CREATE (:N)');
    await g.sync();
    await g.close();
  }
  const off = await kglite.open(join(dir, 'off.kgl'), { durability: 'off' });
  await assert.rejects(off.sync(), (e) => e.code === 'NotDurable');
  await off.close();
});

test('the default durability is full and openWarnings is an array', async (t) => {
  const g = await kglite.open(join(scratch(t), 'g.kgl'));
  assert.equal(g.durability, 'full');
  assert.equal(g.readOnly, false);
  assert.deepEqual(g.openWarnings, []);
  await g.close();
});

test('disk storage: an inherited durability degrades to off with a warning; an explicit one is refused', async (t) => {
  const dir = scratch(t);
  const g = await kglite.open(join(dir, 'disk'), { storage: 'disk' });
  assert.equal(g.durability, 'off');
  assert.ok(g.openWarnings.some((w) => /disk/.test(w) && /'full'/.test(w)), g.openWarnings.join('|'));
  await g.executeWrite('CREATE (:D {k: 1})');
  await g.close();
  const back = await kglite.open(join(dir, 'disk'));
  assert.equal((await back.executeRead('MATCH (d:D) RETURN d.k AS k')).rows[0].k, 1);
  await back.close();
  await assert.rejects(kglite.open(join(dir, 'disk2'), { storage: 'disk', durability: 'full' }), (e) =>
    /disk/.test(e.message),
  );
});

test('mapped storage round-trips through close', async (t) => {
  const path = join(scratch(t), 'g.kgl');
  const g = await kglite.open(path, { storage: 'mapped', durability: 'normal' });
  await g.executeWrite('CREATE (:M {k: 7})');
  await g.close();
  const back = await kglite.open(path);
  assert.equal((await back.executeRead('MATCH (m:M) RETURN m.k AS k')).rows[0].k, 7);
  await back.close();
});

test('readOnly loads the last checkpoint, rejects writes and never creates the path', async (t) => {
  const dir = scratch(t);
  const path = join(dir, 'g.kgl');
  await assert.rejects(kglite.open(path, { readOnly: true }), (e) => e.code === 'FileNotFound');
  const w = await kglite.open(path, { durability: 'off' });
  await w.executeWrite('CREATE (:N {k: 1})');
  await w.close();
  const r = await kglite.open(path, { readOnly: true });
  assert.equal(r.readOnly, true);
  assert.equal(r.durability, 'off');
  assert.equal((await r.executeRead('MATCH (n:N) RETURN count(n) AS c')).rows[0].c, 1);
  for (const call of [() => r.executeWrite('CREATE (:N)'), () => r.checkpoint(), () => r.sync()]) {
    await assert.rejects(call(), (e) => e.code === 'ReadOnly');
  }
  await r.close();
  await r.close();
  // A read-only handle takes no lease: a writer opens while it is live.
  const r2 = await kglite.open(path, { readOnly: true });
  const w2 = await kglite.open(path, { durability: 'off' });
  await w2.close();
  await r2.close();
});

test('readOnly cannot be combined with writer options', async (t) => {
  const path = join(scratch(t), 'g.kgl');
  for (const extra of [{ durability: 'full' }, { storage: 'memory' }, { lockTimeoutMs: 10 }]) {
    await assert.rejects(kglite.open(path, { readOnly: true, ...extra }), (e) => e.code === 'InvalidArgument');
  }
});
