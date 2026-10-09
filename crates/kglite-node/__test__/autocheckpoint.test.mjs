import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { kglite } from './helpers.mjs';

function scratch(t) {
  const dir = mkdtempSync(join(tmpdir(), 'kglite-node-autockpt-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

const MIB = 1024 * 1024;
const PAD = 'x'.repeat(64 * 1024);
const walSize = (path) => (existsSync(`${path}-wal`) ? statSync(`${path}-wal`).size : 0);

async function writeMany(g, count, from = 0) {
  for (let i = from; i < from + count; i += 1) {
    await g.executeWrite('CREATE (:T {id: $i, pad: $pad})', { i, pad: PAD });
  }
}

async function until(predicate, ms = 10000) {
  const deadline = Date.now() + ms;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, 'condition not reached in time');
    await sleep(20);
  }
}

test('writing past autoCheckpointWalMib trims the log in the background and a restart loses nothing', async (t) => {
  const path = join(scratch(t), 'g.kgl');
  const g = await kglite.open(path, { durability: 'normal', autoCheckpointWalMib: 1 });
  await writeMany(g, 60);
  await until(() => existsSync(path) && walSize(path) < 2 * MIB);
  assert.ok(walSize(path) < 2 * MIB, 'the log stays bounded');
  // The checkpoint ran on its own: nothing called checkpoint() or close().
  await writeMany(g, 5, 60);
  await g.close();
  const again = await kglite.open(path, { durability: 'normal' });
  const rows = (await again.executeRead('MATCH (n:T) RETURN count(n) AS c')).rows;
  assert.equal(rows[0].c, 65);
  await again.close();
});

test('autoCheckpointWalMib: 0 leaves the log growing', async (t) => {
  const path = join(scratch(t), 'g.kgl');
  const g = await kglite.open(path, { durability: 'normal', autoCheckpointWalMib: 0 });
  await writeMany(g, 40);
  await sleep(300);
  assert.ok(walSize(path) > 2 * MIB, 'no automatic checkpoint when disabled');
  assert.equal(existsSync(path), false);
  await g.close();
});

test('close right after a burst waits out the background checkpoint and frees the path', async (t) => {
  const path = join(scratch(t), 'g.kgl');
  const g = await kglite.open(path, { durability: 'normal', autoCheckpointWalMib: 1 });
  await writeMany(g, 40);
  await g.close();
  const again = await kglite.open(path, { durability: 'normal' });
  const rows = (await again.executeRead('MATCH (n:T) RETURN count(n) AS c')).rows;
  assert.equal(rows[0].c, 40);
  await again.close();
});

test('autoCheckpointWalMib is validated and refused on a read-only open', async (t) => {
  const path = join(scratch(t), 'g.kgl');
  const g = await kglite.open(path, { durability: 'off' });
  await g.executeWrite('CREATE (:T {id: 1})');
  await g.close();
  await assert.rejects(kglite.open(path, { autoCheckpointWalMib: -1 }), (e) => e.code === 'InvalidArgument');
  await assert.rejects(kglite.open(path, { autoCheckpointWalMib: 'big' }), (e) => e.code === 'InvalidArgument');
  await assert.rejects(
    kglite.open(path, { readOnly: true, autoCheckpointWalMib: 4 }),
    (e) => e.code === 'InvalidArgument',
  );
});
