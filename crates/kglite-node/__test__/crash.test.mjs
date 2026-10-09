import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { crashChild, kglite, startChild } from './helpers.mjs';

/** Run a streaming writer, SIGKILL it after `kill_after` acknowledged writes, and return what it acknowledged. */
async function crashed(path, durability, killAfter) {
  const child = startChild('stream', path, durability);
  try {
    await child.next((l) => l === `ack ${killAfter}`);
  } finally {
    await crashChild(child);
  }
  // Every complete `ack` line the pipe delivered before the process died.
  return child.lines.filter((l) => l.startsWith('ack ')).map((l) => Number(l.slice(4)));
}

async function present(path) {
  const g = await kglite.open(path, { durability: 'full' });
  const ids = (await g.executeRead('MATCH (n:Item) RETURN n.i AS i')).rows.map((r) => r.i);
  await g.close();
  return new Set(ids);
}

// AC7: a write whose promise resolved is on disk even if the process dies next.
for (const durability of ['normal', 'full']) {
  for (const killAfter of [3, 40, 120]) {
    test(`every acknowledged write survives SIGKILL at '${durability}' (killed after ${killAfter})`, async (t) => {
      const dir = mkdtempSync(join(tmpdir(), 'kglite-node-crash-'));
      t.after(() => rmSync(dir, { recursive: true, force: true }));
      const path = join(dir, 'g.kgl');
      const acked = await crashed(path, durability, killAfter);
      assert.ok(acked.length > killAfter, 'the writer was killed mid-stream');
      const ids = await present(path);
      const lost = acked.filter((i) => !ids.has(i));
      assert.deepEqual(lost, [], `acknowledged writes lost after SIGKILL: ${lost.join(',')}`);
    });
  }
}

// Non-vacuity: the same crash at 'off' with no close() must lose acknowledged
// writes, otherwise the assertion above could not tell durable from not.
test("the same crash at durability 'off' without close() loses acknowledged writes", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), 'kglite-node-crash-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const path = join(dir, 'g.kgl');
  const acked = await crashed(path, 'off', 40);
  assert.ok(acked.length > 40);
  const ids = await present(path);
  assert.ok(acked.some((i) => !ids.has(i)), 'expected loss at durability off; the crash test would be vacuous');
});
