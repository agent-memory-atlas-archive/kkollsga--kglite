// graph.setEmbedder: a JavaScript function as the engine's text embedder. The engine
// calls it on a pool thread; the function runs on the JS thread.
import test from 'node:test';
import assert from 'node:assert/strict';
import { freshGraph } from './helpers.mjs';

const DIM = 16;

/** Deterministic bag-of-words vector: each word hashes into one of DIM buckets. */
function vectorOf(text) {
  const v = new Array(DIM).fill(0);
  for (const word of text.toLowerCase().split(/\W+/).filter(Boolean)) {
    let h = 0;
    for (const ch of word) h = (h * 31 + ch.codePointAt(0)) >>> 0;
    v[h % DIM] += 1;
  }
  const norm = Math.hypot(...v) || 1;
  return v.map((x) => x / norm);
}

const syncEmbed = (texts) => texts.map(vectorOf);
const asyncEmbed = async (texts) => {
  await new Promise((r) => setTimeout(r, 5));
  return texts.map(vectorOf);
};

const DOCS = ['alpha beta gamma', 'delta epsilon zeta', 'eta theta iota'];

async function seeded(t, options) {
  const g = await freshGraph(options);
  t.after(async () => {
    await g.graph.close();
    g.cleanup();
  });
  for (const [i, text] of DOCS.entries()) {
    await g.graph.executeWrite('CREATE (:Doc {id: $id, text: $text})', { id: i, text });
  }
  return g.graph;
}

const EMBED_ALL = `MATCH (d:Doc) WITH collect(d) AS docs
  CALL db.node_embeddings.embed({type: 'Doc', text_column: 'text', nodes: docs})
  YIELD embedded, dimension, model RETURN embedded, dimension, model`;

const RANK = `MATCH (d:Doc) RETURN d.id AS id, text_score(d, 'text', $q) AS s ORDER BY s DESC`;

test('a synchronous embedder embeds nodes and answers a text_score query', async (t) => {
  const graph = await seeded(t);
  graph.setEmbedder('hash16', syncEmbed, { dimension: DIM });
  const out = await graph.executeWrite(EMBED_ALL);
  assert.deepEqual(out.rows[0], { embedded: 3, dimension: DIM, model: 'hash16' });
  const ranked = await graph.executeRead(RANK, { q: 'delta epsilon zeta' });
  assert.equal(ranked.rows[0].id, 1);
  assert.ok(ranked.rows[0].s > 0.99);
  assert.ok(ranked.rows[1].s < 0.5);
});

test('an async embedder works, and modelId overrides the registration name', async (t) => {
  const graph = await seeded(t);
  graph.setEmbedder('local', asyncEmbed, { dimension: DIM, modelId: 'hash16-v2' });
  const out = await graph.executeWrite(EMBED_ALL);
  assert.equal(out.rows[0].model, 'hash16-v2');
  const ranked = await graph.executeRead(RANK, { q: 'eta theta iota' });
  assert.equal(ranked.rows[0].id, 2);
});

test('typed arrays are accepted and an omitted dimension is learned', async (t) => {
  const graph = await seeded(t);
  graph.setEmbedder('f32', (texts) => texts.map((x) => Float32Array.from(vectorOf(x))));
  const out = await graph.executeWrite(EMBED_ALL);
  assert.equal(out.rows[0].dimension, DIM);
  const ranked = await graph.executeRead(RANK, { q: 'alpha beta gamma' });
  assert.equal(ranked.rows[0].id, 0);
});

test('a text_score string query without an embedder is a typed error', async (t) => {
  const graph = await seeded(t);
  graph.setEmbedder('x', syncEmbed, { dimension: DIM });
  await graph.executeWrite(EMBED_ALL);
  assert.equal(graph.clearEmbedder('other'), false);
  assert.equal(graph.clearEmbedder('x'), true);
  assert.equal(graph.clearEmbedder(), false);
  await assert.rejects(graph.executeRead(RANK, { q: 'alpha' }), (e) => {
    assert.equal(e.name, 'KgliteError');
    assert.match(e.message, /embedding model/);
    return true;
  });
});

test('a throwing, rejecting or malformed embedder gives a typed error and the process lives', async (t) => {
  const graph = await seeded(t);
  const cases = [
    [() => { throw new Error('boom-sync'); }, /boom-sync/],
    [async () => { throw new Error('boom-async'); }, /boom-async/],
    [() => Promise.reject('plain string'), /plain string/],
    [() => 'not an array', /array of vectors/],
    [() => [[1, 'x', 3]], /not a number/],
    [() => [[1, NaN, 3]], /not finite/],
    [() => [], /returned 0 vectors for 1 texts/],
  ];
  for (const [fn, pattern] of cases) {
    graph.setEmbedder('bad', fn, { dimension: 3 });
    await assert.rejects(graph.executeRead(RANK, { q: 'alpha' }), (e) => {
      assert.equal(e.name, 'KgliteError');
      assert.equal(typeof e.code, 'string');
      assert.match(e.message, pattern);
      return true;
    });
  }
  // The same graph still answers once a working embedder is back.
  graph.setEmbedder('ok', syncEmbed, { dimension: DIM });
  await graph.executeWrite(EMBED_ALL);
  assert.equal((await graph.executeRead(RANK, { q: 'alpha beta gamma' })).rows[0].id, 0);
});

test('a vector of the wrong dimension is a typed error', async (t) => {
  const graph = await seeded(t);
  graph.setEmbedder('wrong', syncEmbed, { dimension: DIM + 1 });
  await assert.rejects(graph.executeWrite(EMBED_ALL), (e) => {
    assert.match(e.message, new RegExp(`dimension ${DIM} \\(declared ${DIM + 1}\\)`));
    return true;
  });
});

test('an embedder that never answers fails after timeoutMs instead of hanging', async (t) => {
  const graph = await seeded(t);
  graph.setEmbedder('stuck', () => new Promise(() => {}), { dimension: DIM, timeoutMs: 300 });
  const started = performance.now();
  await assert.rejects(graph.executeRead(RANK, { q: 'alpha' }), /did not answer within 300 ms/);
  assert.ok(performance.now() - started < 5000);
});

test('two queries embedding at once both get their own answer', async (t) => {
  const graph = await seeded(t);
  graph.setEmbedder('hash', asyncEmbed, { dimension: DIM });
  await graph.executeWrite(EMBED_ALL);
  const queries = DOCS.flatMap((text, id) => [text, text].map(async (q) => {
    const r = await graph.executeRead(RANK, { q });
    return [id, r.rows[0].id];
  }));
  for (const [want, got] of await Promise.all(queries)) assert.equal(got, want);
});

test('the event loop keeps ticking while a slow async embedder runs', async (t) => {
  const graph = await seeded(t);
  graph.setEmbedder('slow', async (texts) => {
    await new Promise((r) => setTimeout(r, 600));
    return texts.map(vectorOf);
  }, { dimension: DIM });
  let ticks = 0;
  const timer = setInterval(() => { ticks += 1; }, 20);
  try {
    await graph.executeWrite(EMBED_ALL);
  } finally {
    clearInterval(timer);
  }
  assert.ok(ticks >= 15, `only ${ticks} timer ticks during a 600 ms embedder`);
});

test('transactions use the registered embedder', async (t) => {
  const graph = await seeded(t);
  graph.setEmbedder('tx', syncEmbed, { dimension: DIM });
  await graph.executeWrite(EMBED_ALL);
  const rows = await graph.transaction(async (tx) => (await tx.run(RANK, { q: 'delta epsilon zeta' })).rows);
  assert.equal(rows[0].id, 1);
});

test('argument errors throw coded errors synchronously', async (t) => {
  const graph = await seeded(t);
  const bad = (fn, pattern) => assert.throws(fn, (e) => e.code === 'InvalidArgument' && pattern.test(e.message));
  bad(() => graph.setEmbedder('', syncEmbed), /name must not be empty/);
  bad(() => graph.setEmbedder('n', 'nope'), /embed must be a function/);
  bad(() => graph.setEmbedder('n', syncEmbed, { dimension: 0 }), /positive integer/);
  bad(() => graph.setEmbedder('n', syncEmbed, { dimensions: 3 }), /unknown embedder option/);
  await graph.close();
  assert.throws(() => graph.setEmbedder('n', syncEmbed), (e) => e.code === 'Closed');
});

// Built with the test-hooks feature (`make test-node`).
test('calling the embedder on the JS thread is refused, not a deadlock', async (t) => {
  const graph = await seeded(t);
  if (typeof graph.__embedOnJsThread !== 'function') return t.skip('needs the test-hooks build');
  graph.setEmbedder('js', syncEmbed, { dimension: DIM, timeoutMs: 400 });
  const started = performance.now();
  const message = graph.__embedOnJsThread();
  assert.match(message, /cannot be called from the JavaScript thread/);
  assert.ok(performance.now() - started < 200, 'the refusal must be immediate');
});
