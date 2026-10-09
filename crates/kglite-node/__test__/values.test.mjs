import test from 'node:test';
import assert from 'node:assert/strict';
import { kglite, freshGraph } from './helpers.mjs';

const { LocalDate, LocalDateTime, Duration, Point, KgFloat } = kglite;

const { graph, cleanup } = await freshGraph();
const { graph: bigGraph, cleanup: cleanupBig } = await freshGraph({ integers: 'bigint' });
test.after(() => {
  cleanup();
  cleanupBig();
});

/** Round-trip one parameter through the engine and back. */
async function roundTrip(value, g = graph) {
  const r = await g.executeRead('RETURN $v AS v', { v: value });
  return r.rows[0].v;
}

const I64_MAX = 2n ** 63n - 1n;
const I64_MIN = -(2n ** 63n);

test('integers: safe mode is number when exact, bigint beyond 2^53-1', async () => {
  for (const n of [0, 1, -1, 42, Number.MAX_SAFE_INTEGER, Number.MIN_SAFE_INTEGER]) {
    const back = await roundTrip(n);
    assert.equal(typeof back, 'number', `${n}`);
    assert.equal(back, n);
  }
  // 2^53 and 2^53+1 are not exactly a JS number; the BigInt path keeps them exact.
  for (const b of [2n ** 53n, 2n ** 53n + 1n, -(2n ** 53n) - 1n, I64_MAX, I64_MIN]) {
    const back = await roundTrip(b);
    assert.equal(typeof back, 'bigint', `${b}`);
    assert.equal(back, b);
  }
});

test('integers: a BigInt inside the safe range comes back as number', async () => {
  assert.equal(await roundTrip(5n), 5);
});

test("integers: 'bigint' mode returns every integer as bigint", async () => {
  assert.equal(await roundTrip(7, bigGraph), 7n);
  assert.equal(await roundTrip(I64_MIN, bigGraph), I64_MIN);
  assert.equal(await roundTrip(0.5, bigGraph), 0.5);
});

test('integers: a stored 2^53+1 survives a write/read cycle', async () => {
  await graph.executeWrite('CREATE (:Big {v: $v})', { v: 2n ** 53n + 1n });
  const r = await graph.executeRead('MATCH (n:Big) RETURN n.v AS v');
  assert.equal(r.rows[0].v, 2n ** 53n + 1n);
});

test('integers: BigInt outside i64 rejects with InvalidArgument naming the parameter', async () => {
  for (const b of [I64_MAX + 1n, I64_MIN - 1n, 2n ** 70n]) {
    await assert.rejects(
      graph.executeRead('RETURN $big', { big: b }),
      (e) => e.code === 'InvalidArgument' && /\$big/.test(e.message) && /BigInt/.test(e.message),
    );
  }
  await assert.rejects(
    graph.executeRead('RETURN $m', { m: { deep: [1, 2n ** 64n] } }),
    (e) => e.code === 'InvalidArgument' && /\$m\.deep\[1\]/.test(e.message),
  );
});

test('floats: NaN, infinities and negative zero are preserved', async () => {
  assert.ok(Number.isNaN(await roundTrip(NaN)));
  assert.equal(await roundTrip(Infinity), Infinity);
  assert.equal(await roundTrip(-Infinity), -Infinity);
  assert.ok(Object.is(await roundTrip(-0), -0));
  assert.equal(await roundTrip(0.1), 0.1);
  assert.equal(await roundTrip(1e300), 1e300);
  assert.equal(await roundTrip(2 ** 53), 2 ** 53); // a float: not a safe integer
});

test('floats: a whole-number JS number is an integer, KgFloat forces a float', async () => {
  // Integer division truncates; float division does not.
  const r = await graph.executeRead('RETURN $a / 2 AS a, $b / 2 AS b', { a: 3, b: new KgFloat(3) });
  assert.equal(r.rows[0].a, 1);
  assert.equal(r.rows[0].b, 1.5);
});

test('strings: empty, unicode, astral, embedded NUL', async () => {
  for (const s of ['', 'plain', 'héllo wörld', '日本語', '😀 emoji \u{1F9EA}', 'a\u0000b', 'line\nbreak\ttab']) {
    assert.equal(await roundTrip(s), s);
  }
});

test('booleans, null and undefined', async () => {
  assert.equal(await roundTrip(true), true);
  assert.equal(await roundTrip(false), false);
  assert.equal(await roundTrip(null), null);
  // An `undefined` entry in the params object is omitted like any map key, so the
  // statement reports the parameter as missing instead of binding a silent null.
  await assert.rejects(roundTrip(undefined), (e) => /Missing parameter: \$v/.test(e.message));
});

test('lists and maps nest and keep order; undefined map entries are omitted', async () => {
  const value = { a: [1, 'x', null, { b: [true, 2.5, [[]]] }], c: {}, d: [] };
  assert.deepEqual(await roundTrip(value), value);
  assert.deepEqual(await roundTrip({ keep: 1, drop: undefined }), { keep: 1 });
  assert.deepEqual(await roundTrip([undefined, 1]), [null, 1]);
});

test('maps: a __proto__ key is an own property and does not touch the prototype', async () => {
  const param = JSON.parse('{"__proto__": {"polluted": 1}, "ok": 2}');
  const back = await roundTrip(param);
  assert.ok(Object.hasOwn(back, '__proto__'));
  assert.equal(Object.getPrototypeOf(back), Object.prototype);
  assert.equal({}.polluted, undefined);
  assert.equal(back.ok, 2);
  // And a column named __proto__ is a real own property of its row.
  const r = await graph.executeRead('RETURN 5 AS `__proto__`');
  assert.ok(Object.hasOwn(r.rows[0], '__proto__'));
  assert.equal(Object.getPrototypeOf(r.rows[0]), Object.prototype);
});

test('dates: LocalDate round-trips and prints ISO', async () => {
  const back = await roundTrip(new LocalDate(2024, 2, 29));
  assert.ok(back instanceof LocalDate);
  assert.deepEqual([back.year, back.month, back.day], [2024, 2, 29]);
  assert.equal(String(back), '2024-02-29');
  assert.equal(JSON.stringify(back), '"2024-02-29"');
  assert.throws(() => new LocalDate(2023, 2, 29), (e) => e.code === 'InvalidArgument');
});

test('datetimes: nanoseconds survive, toDate() reads the wall clock as UTC', async () => {
  const t = new LocalDateTime(2024, 12, 31, 23, 59, 58, 123456789);
  const back = await roundTrip(t);
  assert.ok(back instanceof LocalDateTime);
  assert.equal(back.nanosecond, 123456789);
  assert.equal(String(back), '2024-12-31T23:59:58.123456789');
  assert.equal(back.toDate().toISOString(), '2024-12-31T23:59:58.123Z');
  assert.equal(String(await roundTrip(new LocalDateTime(2024, 1, 2))), '2024-01-02T00:00:00');
});

test('datetimes: a JS Date parameter is a UTC datetime with millisecond precision', async () => {
  const back = await roundTrip(new Date('2024-05-06T07:08:09.123Z'));
  assert.ok(back instanceof LocalDateTime);
  assert.equal(String(back), '2024-05-06T07:08:09.123');
  await assert.rejects(roundTrip(new Date(NaN)), (e) => e.code === 'InvalidArgument');
});

test('durations and points round-trip', async () => {
  const d = await roundTrip(new Duration(14, 3, 4));
  assert.ok(d instanceof Duration);
  assert.deepEqual([d.months, d.days, d.seconds], [14, 3, 4]);
  assert.equal(String(d), 'P1Y2M3DT4S');
  assert.deepEqual(JSON.parse(JSON.stringify(d)), { months: 14, days: 3, seconds: 4 });
  const p = await roundTrip(new Point(59.91, 10.75));
  assert.ok(p instanceof Point);
  assert.deepEqual([p.latitude, p.longitude], [59.91, 10.75]);
  assert.deepEqual(JSON.parse(JSON.stringify(p)), { latitude: 59.91, longitude: 10.75 });
});

test('temporal values returned by Cypher functions are the same classes', async () => {
  const r = await graph.executeRead("RETURN date('2020-01-02') AS d, duration({months: 1, days: 5}) AS u");
  assert.ok(r.rows[0].d instanceof LocalDate);
  assert.equal(String(r.rows[0].d), '2020-01-02');
  assert.ok(r.rows[0].u instanceof Duration);
  assert.equal(r.rows[0].u.months, 1);
  assert.equal(r.rows[0].u.days, 5);
});

test('nodes, relationships and paths are plain objects with documented shapes', async () => {
  await graph.executeWrite(
    "CREATE (a:Person {name: 'Ada', born: 1815})-[:KNOWS {since: 1833}]->(b:Person {name: 'Babbage'})",
  );
  const r = await graph.executeRead(
    'MATCH p = (a:Person {name: "Ada"})-[r:KNOWS]->(b:Person) RETURN a, r, p',
  );
  const { a, r: rel, p } = r.rows[0];
  assert.equal(Object.getPrototypeOf(a), Object.prototype);
  assert.deepEqual(Object.keys(a).sort(), ['id', 'labels', 'properties']);
  assert.equal(typeof a.id, 'number');
  assert.deepEqual(a.labels, ['Person']);
  assert.equal(a.properties.name, 'Ada');
  assert.equal(a.properties.born, 1815);
  assert.deepEqual(Object.keys(rel).sort(), ['endId', 'id', 'properties', 'startId', 'type']);
  assert.equal(rel.type, 'KNOWS');
  assert.equal(rel.properties.since, 1833);
  assert.equal(rel.startId, a.id);
  assert.deepEqual(Object.keys(p).sort(), ['nodes', 'relationships']);
  assert.equal(p.nodes.length, 2);
  assert.equal(p.relationships.length, 1);
  assert.equal(p.nodes[0].id, a.id);
  assert.equal(p.relationships[0].type, 'KNOWS');
  assert.equal(JSON.parse(JSON.stringify(r.rows[0])).a.properties.name, 'Ada');
});

test('parameters that are not representable reject with InvalidArgument', async () => {
  const rejects = (value) =>
    assert.rejects(graph.executeRead('RETURN $v', { v: value }), (e) => e.code === 'InvalidArgument');
  await rejects(new Map());
  await rejects(new Set());
  await rejects(Buffer.from('x'));
  await rejects(new Uint8Array(2));
  await rejects(() => 1);
  await rejects(Symbol('s'));
  await rejects(new (class Foo {})());
  const cyclic = {};
  cyclic.self = cyclic;
  await rejects(cyclic);
  let deep = 1;
  for (let i = 0; i < 2000; i++) deep = [deep];
  await rejects(deep);
});

test('params must be a plain object; options are validated, not ignored', async () => {
  await assert.rejects(graph.executeRead('RETURN 1', [1]), (e) => e.code === 'InvalidArgument');
  await assert.rejects(graph.executeRead('RETURN 1', {}, { timeoutMS: 5 }), (e) => e.code === 'InvalidArgument' && /timeoutMS/.test(e.message));
  await assert.rejects(graph.executeRead('RETURN 1', {}, { rowLimit: -1 }), (e) => e.code === 'InvalidArgument');
  await assert.rejects(graph.executeRead(42), (e) => e.code === 'InvalidArgument');
  await assert.rejects(kglite.open('x', { durabilty: 'off' }), (e) => e.code === 'InvalidArgument');
  await assert.rejects(kglite.open('x', { durability: 'sometimes' }), (e) => e.code === 'InvalidArgument');
  await assert.rejects(kglite.open('x', { readOnly: 'yes' }), (e) => e.code === 'InvalidArgument');
});

test('result shape: columns, stats, warnings, truncated', async () => {
  const w = await graph.executeWrite('CREATE (:S {v: 1}), (:S {v: 2}), (:S {v: 3}) RETURN 1 AS one');
  assert.deepEqual(w.columns, ['one']);
  assert.equal(w.stats.nodesCreated, 3);
  assert.deepEqual(w.warnings, []);
  assert.equal(w.truncated, undefined);
  const r = await graph.executeRead('MATCH (n:S) RETURN n.v AS v ORDER BY v', null, { rowLimit: 2 });
  assert.deepEqual(r.rows.map((x) => x.v), [1, 2]);
  assert.deepEqual(r.truncated, { rowLimit: 2, totalRows: 3 });
  assert.ok(r.warnings.length >= 1, 'a truncation also appears as a warning');
  const unknown = await graph.executeRead('MATCH (n:NoSuchLabel) RETURN n');
  assert.ok(unknown.warnings.length >= 1, 'unknown-label advice reaches warnings');
});
