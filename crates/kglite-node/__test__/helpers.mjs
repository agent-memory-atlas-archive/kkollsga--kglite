import { createRequire } from 'node:module';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const require = createRequire(import.meta.url);
export const kglite = require('../index.js');

/** Open a fresh in-memory graph under a temp dir; `durability: 'off'` keeps it file-free. */
export async function freshGraph(options = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'kglite-node-'));
  const graph = await kglite.open(join(dir, 'g.kgl'), { durability: 'off', ...options });
  return { graph, dir, cleanup: () => rmSync(dir, { recursive: true, force: true }) };
}
