'use strict';
// Release-artifact smoke: load the addon, check it is the release build.
//
// usage: node smoke.cjs <expected-version> [entry]
//   entry   path to the loader (default: the installed `kglite-node` package)
//
// Fails (non-zero) unless the addon loads, reports the expected version, and
// does NOT export the test-only `__panic` hook. `test-hooks` is a Cargo
// feature that must never reach a published artifact; its absence is asserted
// on the loaded addon here and on the binary's bytes in `assertNoTestHooks`.
const { readFileSync } = require('node:fs');

const [expected, entry] = process.argv.slice(2);
if (!expected) {
  console.error('usage: smoke.cjs <expected-version> [entry]');
  process.exit(2);
}

const addon = require(entry ? require('node:path').resolve(entry) : 'kglite-node');

function fail(msg) {
  console.error(`SMOKE FAILED: ${msg}`);
  process.exit(1);
}

if (typeof addon.version !== 'function') fail('addon exports no version()');
if (addon.version() !== expected) fail(`version() is ${addon.version()}, expected ${expected}`);
if ('__panic' in addon) fail('addon exports __panic: built with the test-hooks feature');

// The export being absent is not enough on its own (a rename would hide the
// hook); the hook's panic message string must also be absent from the binary.
const binary = process.env.KGLITE_NODE_BINARY;
if (binary && readFileSync(binary).includes('deliberate test panic')) {
  fail(`${binary} contains the test-hooks panic message`);
}

// A durable write/read round trip through close and a reopen: the packaged
// binary persists, recovers and releases its lease, not merely loads.
async function roundTrip() {
  const { mkdtempSync, rmSync } = require('node:fs');
  const { tmpdir } = require('node:os');
  const { join } = require('node:path');
  const dir = mkdtempSync(join(tmpdir(), 'kglite-node-smoke-'));
  try {
    const path = join(dir, 'smoke.kgl');
    const g = await addon.open(path, { durability: 'normal' });
    await g.executeWrite('CREATE (:Smoke {n: $n})', { n: 42 });
    await g.close();
    const back = await addon.open(path, { durability: 'normal' });
    const rows = (await back.executeRead('MATCH (s:Smoke) RETURN s.n AS n')).rows;
    await back.close();
    if (rows.length !== 1 || rows[0].n !== 42) fail(`round trip returned ${JSON.stringify(rows)}`);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

roundTrip().then(
  () => console.log(`smoke ok: kglite-node ${addon.version()} on ${process.platform}-${process.arch}`),
  (e) => fail(`write/read round trip rejected: ${e && e.code} ${e && e.message}`),
);
