// OpenOptions.validTimeDefault: which instant an unprefixed statement reads.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { kglite } from './helpers.mjs';

const SETUP = [
  "CREATE (:Well {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}), (:Well {id: 2, vf: date('2005-01-01')})",
  "CALL db.temporal.declare({node: 'Well', from: 'vf', to: 'vt', convention: 'closed'}) YIELD declared RETURN declared",
];
const IDS = 'MATCH (w:Well) RETURN w.id AS id ORDER BY id';

async function wells(options, { reopenReadOnly = false } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'kglite-vt-'));
  const path = join(dir, 'g.kgl');
  try {
    const seed = await kglite.open(path, { durability: 'off' });
    for (const q of SETUP) await seed.executeWrite(q);
    if (!reopenReadOnly) {
      await seed.close();
      const g = await kglite.open(path, { durability: 'off', ...options });
      try { return (await g.executeRead(IDS)).rows.map((r) => r.id); } finally { await g.close(); }
    }
    await seed.close();
    const g = await kglite.open(path, { readOnly: true, ...options });
    try { return (await g.executeRead(IDS)).rows.map((r) => r.id); } finally { await g.close(); }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('today (the default) hides the well that closed in 2010', async () => {
  assert.deepEqual(await wells({}), [2]);
  assert.deepEqual(await wells({ validTimeDefault: 'today' }), [2]);
});

test("'all' and a fixed day change what an unprefixed read sees", async () => {
  assert.deepEqual(await wells({ validTimeDefault: 'all' }), [1, 2]);
  assert.deepEqual(await wells({ validTimeDefault: '2008-01-01' }), [1, 2]);
  assert.deepEqual(await wells({ validTimeDefault: '2003-01-01' }), [1]);
});

test('a readOnly open honours it too', async () => {
  assert.deepEqual(await wells({ validTimeDefault: 'all' }, { reopenReadOnly: true }), [1, 2]);
  assert.deepEqual(await wells({}, { reopenReadOnly: true }), [2]);
});

test('an invalid value rejects InvalidArgument', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'kglite-vt-'));
  try {
    for (const bad of ['yesterday', '2008-13-01', 5]) {
      await assert.rejects(
        kglite.open(join(dir, 'g.kgl'), { durability: 'off', validTimeDefault: bad }),
        (e) => e.code === 'InvalidArgument',
      );
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
