// Issue #221 acceptance criteria through the Node binding: online single-file backup.
import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { kglite } from './helpers.mjs';

const MIN_WINDOW_MS = 300;

const opened = new WeakMap();

/** A temp dir removed after the test, once every graph opened through `open` is closed. */
function scratch(t) {
  const dir = mkdtempSync(join(tmpdir(), 'kglite-node-backup-'));
  const graphs = [];
  opened.set(t, graphs);
  t.after(async () => {
    for (const g of graphs) await g.close().catch(() => {});
    rmSync(dir, { recursive: true, force: true });
  });
  return dir;
}

async function open(t, path, options) {
  const graph = await kglite.open(path, options);
  opened.get(t).push(graph);
  return graph;
}

/** `n` Person nodes with enough payload that serializing them takes real time. */
async function seed(graph, n) {
  const chunk = 20000;
  for (let lo = 1; lo <= n; lo += chunk) {
    await graph.executeWrite(
      "UNWIND range($lo, $hi) AS i CREATE (:Person {id: i, city: 'city-' + toString(i % 977), bio: 'a long-ish text field number ' + toString(i) + ' and then some more text to fill the row'})",
      { lo, hi: Math.min(n, lo + chunk - 1) },
    );
  }
}

/** Commit `CREATE (:Seq {id: i})` every ~10 ms, recording each acknowledgement time. */
function startWriter(graph) {
  const w = { acks: [], stop: false, error: null };
  w.done = (async () => {
    try {
      for (let i = 1; !w.stop; i++) {
        await graph.executeWrite('CREATE (:Seq {id: $i})', { i });
        w.acks.push({ at: performance.now(), i });
        await new Promise((r) => setTimeout(r, 10));
      }
    } catch (e) {
      w.error = e;
    }
  })();
  w.finish = async () => {
    w.stop = true;
    await w.done;
    assert.equal(w.error, null);
  };
  return w;
}

const maxGap = (acks, start, end) => {
  const inside = acks.filter((a) => a.at >= start && a.at <= end).map((a) => a.at);
  const points = [start, ...inside, end];
  let gap = 0;
  for (let i = 1; i < points.length; i++) gap = Math.max(gap, points[i] - points[i - 1]);
  return { gap, count: inside.length };
};

/** Open `file` read-only in a fresh Node process and print its Seq summary as JSON. */
function readInFreshProcess(file) {
  const script = `
    const kglite = require(${JSON.stringify(new URL('../index.js', import.meta.url).pathname)});
    (async () => {
      const g = await kglite.open(process.argv[1], { readOnly: true });
      const r = await g.executeRead('MATCH (s:Seq) RETURN count(s) AS c, min(s.id) AS lo, max(s.id) AS hi');
      const p = await g.executeRead('MATCH (p:Person) RETURN count(p) AS c');
      console.log(JSON.stringify({ seq: r.rows[0], people: p.rows[0].c }));
      await g.close();
    })();`;
  return JSON.parse(execFileSync(process.execPath, ['-e', script, file], { encoding: 'utf8' }));
}

test('AC1/AC2/AC3: writers keep acking during a backup, which is an exact gap-free prefix', async (t) => {
  let people = 150_000;
  for (let attempt = 0; ; attempt++) {
    const dir = scratch(t);
    const graph = await open(t, join(dir, 'g.kgl'), { durability: 'normal' });
    await seed(graph, people);
    const base = await graph.backup(join(dir, 'base.kgl'));
    const writer = startWriter(graph);
    await new Promise((r) => setTimeout(r, 300));
    const dest = join(dir, 'snap.kgl');
    const t0 = performance.now();
    const report = await graph.backup(dest);
    const t1 = performance.now();
    await new Promise((r) => setTimeout(r, 200));
    await writer.finish();
    await graph.close();

    console.log(`backup ${(t1 - t0).toFixed(0)} ms (${people} nodes), lock hold ${report.lockHoldMs.toFixed(1)} ms`);
    if (t1 - t0 <= MIN_WINDOW_MS && attempt < 3) {
      // Too quick to prove anything on this machine: grow the graph and go again.
      people *= 2;
      continue;
    }
    assert.ok(t1 - t0 > MIN_WINDOW_MS, `a ${people}-node backup took ${t1 - t0} ms: too fast to prove anything`);

    // AC1: commits flowed through the window; the stall is bounded well below the backup.
    const { gap, count } = maxGap(writer.acks, t0, t1);
    assert.ok(count >= 10, `writer starved: ${count} acks in ${(t1 - t0).toFixed(0)} ms`);
    assert.ok(gap < 0.5 * (t1 - t0), `writer stalled ${gap.toFixed(0)} ms during a ${(t1 - t0).toFixed(0)} ms backup`);
    assert.ok(report.lockHoldMs < gap + 250 && report.lockHoldMs < 0.5 * report.elapsedMs);

    // The report.
    assert.equal(report.path, dest);
    assert.equal(report.bytes, statSync(dest).size);
    assert.ok(report.elapsedMs >= report.lockHoldMs);
    assert.equal(typeof report.lsn, 'number');
    assert.ok(report.lsn > base.lsn);

    // AC3: one file, no sidecar beside it.
    assert.deepEqual(readdirSync(dir).filter((f) => f.startsWith('snap')), ['snap.kgl']);

    // AC2: nodes 1..N contiguous, N consistent with the lsn (one log frame per commit).
    const seen = readInFreshProcess(dest);
    assert.equal(seen.people, people);
    assert.ok(seen.seq.c >= 10);
    assert.equal(seen.seq.lo, 1);
    assert.equal(seen.seq.hi, seen.seq.c);
    assert.equal(report.lsn - base.lsn, seen.seq.c);
    assert.equal(report.nodes, people + seen.seq.c);
    return;
  }
});

test('AC4: the destination only ever holds a complete file while a backup runs', async (t) => {
  const dir = scratch(t);
  const graph = await open(t, join(dir, 'g.kgl'), { durability: 'off' });
  await seed(graph, 60_000);
  const dest = join(dir, 'out', 'b.kgl');
  await graph.executeWrite('CREATE (:Marker {v: 1})'); // distinguishes the two backups
  mkdirSync(join(dir, 'out'));
  const first = await graph.backup(dest);
  await seed(graph, 60_000);
  let sawTemp = false;
  const sizes = new Set();
  let finished = false;
  const second = graph.backup(dest).finally(() => {
    finished = true;
  });
  while (!finished) {
    const names = readdirSync(join(dir, 'out'));
    if (names.some((n) => n !== 'b.kgl')) sawTemp = true;
    if (!finished && existsSync(dest)) sizes.add(statSync(dest).size);
    await new Promise((r) => setImmediate(r));
  }
  const report = await second;
  assert.ok(sawTemp, 'no in-flight temporary file was ever observed (graph too small?)');
  // The destination is the previous complete backup until the rename, then the new complete one.
  assert.ok(
    [...sizes].every((s) => s === first.bytes || s === report.bytes),
    `partial destination observed: ${[...sizes]} (expected ${first.bytes} or ${report.bytes})`,
  );
  assert.equal(statSync(dest).size, report.bytes);
  assert.deepEqual(readdirSync(join(dir, 'out')), ['b.kgl']);
  assert.ok(report.bytes > first.bytes);
});

test('AC5: a backup onto the live graph or an alias of it is refused and changes nothing', async (t) => {
  const dir = scratch(t);
  const path = join(dir, 'g.kgl');
  const graph = await open(t, path, { durability: 'normal' });
  await graph.executeWrite('CREATE (:Person {id: 1})');
  await graph.checkpoint();
  const before = statSync(path).size;
  for (const alias of [path, join(dir, '.', 'g.kgl'), join(dir, '..', dir.split('/').pop(), 'g.kgl')]) {
    await assert.rejects(graph.backup(alias), (e) => {
      assert.equal(e.name, 'KgliteError');
      assert.match(e.message, /live|alias|same/i);
      return true;
    });
  }
  assert.equal(statSync(path).size, before);
  // The graph is intact and still backs up elsewhere.
  const ok = await graph.backup(join(dir, 'copy.kgl'));
  assert.equal(ok.nodes, 1);
});

test('AC5 before any checkpoint exists: the graph path is still refused', async (t) => {
  const dir = scratch(t);
  const path = join(dir, 'g.kgl');
  const graph = await open(t, path, { durability: 'off' });
  await graph.executeWrite('CREATE (:Person {id: 1})');
  await assert.rejects(graph.backup(path), (e) => e.name === 'KgliteError');
  assert.equal(existsSync(path), false);
});

test('a non-durable graph reports lsn null; integers: bigint makes it a bigint', async (t) => {
  const dir = scratch(t);
  const off = await open(t, join(dir, 'a.kgl'), { durability: 'off' });
  await off.executeWrite('CREATE (:Person {id: 1})');
  assert.equal((await off.backup(join(dir, 'a-copy.kgl'))).lsn, null);

  const durable = await open(t, join(dir, 'b.kgl'), { durability: 'normal', integers: 'bigint' });
  await durable.executeWrite('CREATE (:Person {id: 1})');
  const report = await durable.backup(join(dir, 'b-copy.kgl'));
  assert.equal(typeof report.lsn, 'bigint');
});

test('argument and mode errors reject: empty or non-string dest, disk storage, closed graph', async (t) => {
  const dir = scratch(t);
  const graph = await open(t, join(dir, 'g.kgl'), { durability: 'off' });
  await assert.rejects(graph.backup(''), (e) => e.code === 'InvalidArgument');
  await assert.rejects(graph.backup(5), (e) => e.code === 'InvalidArgument');
  await graph.close();
  await assert.rejects(graph.backup(join(dir, 'x.kgl')), (e) => e.code === 'Closed');

  const disk = await open(t, join(dir, 'disk'), { storage: 'disk' });
  await disk.executeWrite('CREATE (:Person {id: 1})');
  await assert.rejects(disk.backup(join(dir, 'd.kgl')), (e) => /disk/i.test(e.message));
  assert.equal(existsSync(join(dir, 'd.kgl')), false);
});

test('a readOnly graph can be backed up', async (t) => {
  const dir = scratch(t);
  const path = join(dir, 'g.kgl');
  const w = await kglite.open(path, { durability: 'off' });
  await w.executeWrite('CREATE (:Person {id: 1})');
  await w.close();
  const r = await open(t, path, { readOnly: true });
  assert.equal((await r.backup(join(dir, 'copy.kgl'))).nodes, 1);
  await assert.rejects(r.backup(path), (e) => e.name === 'KgliteError');
});
