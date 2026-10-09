import test from 'node:test';
import assert from 'node:assert/strict';
import { freshGraph } from './helpers.mjs';

const { graph, cleanup } = await freshGraph();
test.after(cleanup);

/** Largest gap between consecutive 10 ms timer ticks while `work` runs. */
async function watchLoop(work) {
  const gaps = [];
  let last = performance.now();
  const timer = setInterval(() => {
    const now = performance.now();
    gaps.push(now - last);
    last = now;
  }, 10);
  const started = performance.now();
  let result;
  try {
    result = await work();
  } finally {
    clearInterval(timer);
  }
  // The promise settles from inside the callback that built the result, so its
  // continuation runs before the timers phase could observe that callback's
  // duration. Count the time since the last tick as a gap or the final block,
  // the large-result conversion, would be invisible.
  gaps.push(performance.now() - last);
  return { result, elapsed: performance.now() - started, maxGap: Math.max(0, ...gaps), ticks: gaps.length };
}

const heavy = (n) => `UNWIND range(1, ${n}) AS a UNWIND range(1, ${n}) AS b RETURN count(*) AS c`;

/** Smallest n (doubling) whose cross product keeps the engine busy for at least `ms`. */
async function sizeFor(ms) {
  for (let n = 500; ; n *= 2) {
    const t = performance.now();
    await graph.executeRead(heavy(n));
    if (performance.now() - t >= ms) return n;
  }
}

// AC5: the loop keeps turning while the engine computes. Budget: a tick every
// 10 ms; 100 ms of slack absorbs CI scheduling noise yet is far below the
// multi-second block a query on the JS thread causes.
const TICK_GAP_BUDGET_MS = 100;

test('the event loop keeps ticking during a multi-second query', async () => {
  const n = await sizeFor(2000);
  const { result, elapsed, maxGap, ticks } = await watchLoop(() => graph.executeRead(heavy(n)));
  assert.equal(result.rows[0].c, n * n);
  assert.ok(elapsed >= 2000, `query must be long enough to prove anything (took ${elapsed.toFixed(0)} ms)`);
  assert.ok(ticks >= (elapsed / 10) * 0.5, `only ${ticks} ticks in ${elapsed.toFixed(0)} ms`);
  assert.ok(maxGap < TICK_GAP_BUDGET_MS, `event loop stalled ${maxGap.toFixed(0)} ms during a ${elapsed.toFixed(0)} ms query`);
  console.log(`# AC5: ${elapsed.toFixed(0)} ms query, ${ticks} ticks, max gap ${maxGap.toFixed(1)} ms`);
});

test('concurrent queries overlap instead of serialising on the JS thread', async () => {
  const n = await sizeFor(1000);
  const single = performance.now();
  await graph.executeRead(heavy(n));
  const oneMs = performance.now() - single;
  const { elapsed, maxGap } = await watchLoop(() => Promise.all([1, 2, 3].map(() => graph.executeRead(heavy(n)))));
  // Three queries on a pool of >= 2 workers finish well before 3x one query.
  assert.ok(elapsed < oneMs * 2.6, `3 queries took ${elapsed.toFixed(0)} ms vs ${oneMs.toFixed(0)} ms for one`);
  assert.ok(maxGap < TICK_GAP_BUDGET_MS, `event loop stalled ${maxGap.toFixed(0)} ms`);
});

// Building a large result happens on the JS thread, in one block, so the stall
// grows with the row count and with the CPU. A fixed millisecond budget is
// therefore machine-dependent; these assertions are relative to the same
// process instead (measured on a debug build, macOS arm64: 30,000 rows stall
// ~20 ms, 300,000 rows ~165 ms of a ~320 ms call, and a stream of the same
// 300,000 rows stalls ~20 ms):
//  1. the stall is a fraction of the call, so the query itself runs off the JS
//     thread (a query on the loop would make the stall about the whole call);
//  2. the stall scales about linearly with rows from a small calibration run;
//  3. stream() of the same result stalls several times less, so the stall is
//     the one-block conversion, not engine work. Callers with bigger results
//     page with LIMIT, stream, or set `rowLimit`.
const LARGE_RESULT_ROWS = 300_000;
const CALIBRATION_ROWS = 30_000;
const rowsQuery = (n) => `UNWIND range(1, ${n}) AS a RETURN a, a * 2 AS b, toString(a) AS c`;

test('converting a large result stalls the loop for a bounded time', async () => {
  const small = rowsQuery(CALIBRATION_ROWS);
  const q = rowsQuery(LARGE_RESULT_ROWS);
  await graph.executeRead(q); // warm the plan cache and allocator
  let calibration = Infinity;
  for (let i = 0; i < 3; i++) calibration = Math.min(calibration, (await watchLoop(() => graph.executeRead(small))).maxGap);

  const { result, elapsed, maxGap } = await watchLoop(() => graph.executeRead(q));
  assert.equal(result.rows.length, LARGE_RESULT_ROWS);
  assert.deepEqual(result.rows[LARGE_RESULT_ROWS - 1], { a: LARGE_RESULT_ROWS, b: LARGE_RESULT_ROWS * 2, c: String(LARGE_RESULT_ROWS) });
  const streamed = await watchLoop(async () => {
    for await (const row of graph.stream(q)) void row;
  });
  console.log(
    `# large result: ${LARGE_RESULT_ROWS} rows in ${elapsed.toFixed(0)} ms, max loop gap ${maxGap.toFixed(0)} ms ` +
      `(calibration ${calibration.toFixed(0)} ms for ${CALIBRATION_ROWS} rows; streamed gap ${streamed.maxGap.toFixed(0)} ms)`,
  );
  assert.ok(maxGap < elapsed * 0.8, `stall ${maxGap.toFixed(0)} ms of a ${elapsed.toFixed(0)} ms call: the query ran on the JS thread`);
  const expected = calibration * (LARGE_RESULT_ROWS / CALIBRATION_ROWS);
  assert.ok(maxGap < expected * 3 + 50, `stall ${maxGap.toFixed(0)} ms vs ${expected.toFixed(0)} ms extrapolated from ${CALIBRATION_ROWS} rows`);
  assert.ok(streamed.maxGap * 2 < maxGap, `stream stalled ${streamed.maxGap.toFixed(0)} ms vs ${maxGap.toFixed(0)} ms for the eager result`);
});

test('rowLimit bounds the conversion stall', async () => {
  const q = `UNWIND range(1, ${LARGE_RESULT_ROWS}) AS a RETURN a, a * 2 AS b, toString(a) AS c`;
  const { result, maxGap } = await watchLoop(() => graph.executeRead(q, null, { rowLimit: 1000 }));
  assert.equal(result.rows.length, 1000);
  assert.equal(result.truncated.totalRows, LARGE_RESULT_ROWS);
  assert.ok(maxGap < TICK_GAP_BUDGET_MS, `limited result stalled ${maxGap.toFixed(0)} ms`);
});
