import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { kglite, startChild } from './helpers.mjs';

function scratch(t) {
  const dir = mkdtempSync(join(tmpdir(), 'kglite-node-lease-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return join(dir, 'g.kgl');
}

/** A child holding the writer lease on `path`, with 5 checkpointed + 3 WAL-only rows. */
async function holder(t, path, durability = 'normal') {
  const child = startChild('hold', path, durability);
  t.after(() => child.proc.kill('SIGKILL'));
  await child.next((l) => l === 'ready');
  return child;
}

test('a second process write-open rejects WriterLeaseHeld naming the holder', async (t) => {
  const path = scratch(t);
  const child = await holder(t, path);
  await assert.rejects(kglite.open(path), (e) => {
    assert.equal(e.code, 'WriterLeaseHeld');
    assert.equal(e.name, 'KgliteError');
    assert.equal(e.holder.pid, child.proc.pid);
    assert.equal(e.holder.self, false);
    assert.equal(typeof e.holder.since, 'string');
    return true;
  });
});

test('a same-process double open rejects WriterLeaseHeld with holder.self', async (t) => {
  const path = scratch(t);
  const first = await kglite.open(path, { durability: 'off' });
  await assert.rejects(kglite.open(path, { durability: 'off' }), (e) => {
    assert.equal(e.code, 'WriterLeaseHeld');
    assert.equal(e.holder.pid, process.pid);
    assert.equal(e.holder.self, true);
    return true;
  });
  await first.close();
  const second = await kglite.open(path, { durability: 'off' });
  await second.close();
});

test('lockTimeoutMs waits for the holder: expires when it never leaves, succeeds when it does', async (t) => {
  const path = scratch(t);
  const first = await kglite.open(path, { durability: 'off' });
  const started = Date.now();
  await assert.rejects(kglite.open(path, { lockTimeoutMs: 300 }), (e) => e.code === 'WriterLeaseHeld');
  assert.ok(Date.now() - started >= 250, 'waited for the timeout instead of failing fast');
  const waiting = kglite.open(path, { durability: 'off', lockTimeoutMs: 5000 });
  setTimeout(() => first.close(), 200);
  const second = await waiting;
  await second.close();
});

test('readOnly while a writer is live reads the last checkpoint, never the log, and corrupts nothing', async (t) => {
  const path = scratch(t);
  const child = await holder(t, path);
  const r = await kglite.open(path, { readOnly: true });
  // O1: 5 rows were checkpointed; the 3 written after it live only in the write-ahead log.
  assert.equal((await r.executeRead('MATCH (n:Item) RETURN count(n) AS c')).rows[0].c, 5);
  await r.close();
  // The writer is unharmed and its log is intact: a clean close folds all 8 rows in.
  child.send('close');
  await child.next((l) => l === 'closed');
  child.send('exit');
  await child.exited;
  const after = await kglite.open(path, { readOnly: true });
  assert.equal((await after.executeRead('MATCH (n:Item) RETURN count(n) AS c')).rows[0].c, 8);
  await after.close();
});

test('close() then a second writer succeeds, across processes', async (t) => {
  const path = scratch(t);
  const child = await holder(t, path);
  await assert.rejects(kglite.open(path), (e) => e.code === 'WriterLeaseHeld');
  child.send('close');
  await child.next((l) => l === 'closed');
  // The child process is still alive; only the graph was closed.
  const g = await kglite.open(path);
  assert.equal((await g.executeRead('MATCH (n:Item) RETURN count(n) AS c')).rows[0].c, 8);
  await g.close();
  child.send('exit');
  await child.exited;
});

test('the lease is freed by kill -9, and the next writer recovers the log', { skip: process.platform === 'win32' }, async (t) => {
  const path = scratch(t);
  const child = await holder(t, path);
  child.proc.kill('SIGKILL');
  await child.exited;
  const g = await kglite.open(path);
  assert.equal((await g.executeRead('MATCH (n:Item) RETURN count(n) AS c')).rows[0].c, 8);
  await g.close();
});
