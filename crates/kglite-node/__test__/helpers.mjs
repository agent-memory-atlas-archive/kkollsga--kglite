import { createRequire } from 'node:module';
import { spawn } from 'node:child_process';
import assert from 'node:assert/strict';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
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

/** Hard-kill `child` (no cleanup: SIGKILL on POSIX, TerminateProcess on Windows) and check it did not exit normally. */
export async function crashChild(child) {
  child.proc.kill('SIGKILL');
  const { code, signal } = await child.exited;
  assert.ok(signal !== null || code !== 0, `child exited normally (code ${code}) instead of being killed`);
}

const childScript = fileURLToPath(new URL('./child.mjs', import.meta.url));

/** Start `child.mjs`; `lines` resolves each stdout line to waiters via `next(prefix)`. */
export function startChild(mode, path, durability) {
  const proc = spawn(process.execPath, [childScript, mode, path, durability], {
    stdio: ['pipe', 'pipe', 'inherit'],
  });
  const lines = [];
  const waiters = [];
  const exited = new Promise((resolve) => proc.on('exit', (code, signal) => resolve({ code, signal })));
  createInterface({ input: proc.stdout }).on('line', (line) => {
    lines.push(line);
    for (const w of [...waiters]) w();
  });
  const next = (predicate, timeoutMs = 20000) =>
    new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`child silent for ${timeoutMs}ms`)), timeoutMs);
      const check = () => {
        const hit = lines.find(predicate);
        if (hit === undefined) return;
        clearTimeout(timer);
        waiters.splice(waiters.indexOf(check), 1);
        resolve(hit);
      };
      waiters.push(check);
      check();
    });
  return { proc, lines, next, exited, send: (s) => proc.stdin.write(`${s}\n`) };
}
