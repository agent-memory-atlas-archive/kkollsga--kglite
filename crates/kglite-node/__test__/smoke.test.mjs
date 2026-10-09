import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';

const require = createRequire(import.meta.url);
const addon = require('../index.js');

const cargo = readFileSync(new URL('../../../Cargo.toml', import.meta.url), 'utf8');
const workspaceVersion = /\[workspace\.package\][^[]*?version\s*=\s*"([^"]+)"/s.exec(cargo)[1];

test('version() matches [workspace.package] version', () => {
  assert.equal(addon.version(), workspaceVersion);
});

test('a panic inside an export becomes a JS Error with code INTERNAL', () => {
  assert.equal(typeof addon.__panic, 'function', 'build with --features test-hooks (make test-node)');
  assert.throws(
    () => addon.__panic(),
    (e) => e instanceof Error && e.code === 'INTERNAL' && /deliberate test panic/.test(e.message),
  );
  // The process survived and the addon still works.
  assert.equal(addon.version(), workspaceVersion);
});
