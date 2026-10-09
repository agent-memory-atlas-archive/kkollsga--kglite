// Compile-time contract for index.d.ts: checked by `tsc --noEmit`, never run.
import {
  open,
  Duration,
  Graph,
  KgFloat,
  LocalDate,
  LocalDateTime,
  Point,
  Transaction,
  type KgliteError,
  type KgNode,
  type KgValue,
  type BackupReport,
  type OntologyDeclared,
  type QueryResult,
} from '../index.js';

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
function assertType<_T extends true>(): void {}

export async function flow(): Promise<void> {
  const graph: Graph = await open('/tmp/x.kgl', { durability: 'normal', integers: 'safe' });

  // Integers are number | bigint, never plain number.
  const result: QueryResult = await graph.executeRead('RETURN 1 AS n', { a: 1, b: 2n, c: new KgFloat(1), d: new Date() });
  const n: KgValue = result.rows[0].n;
  if (typeof n === 'number' || typeof n === 'bigint') {
    const widened: number | bigint = n;
    void widened;
  }
  // @ts-expect-error a KgValue is not assignable to number without narrowing
  const bad: number = n;
  void bad;

  // Transaction flow.
  const tx: Transaction = await graph.begin({ readOnly: false });
  const inner: QueryResult = await tx.run('CREATE (:T)');
  await tx.commit();
  await tx.rollback();
  const finished: boolean = tx.finished;
  void [inner, finished];

  const value: number = await graph.transaction(async (t) => {
    await t.run('CREATE (:T)');
    return 42;
  }, { retries: 3 });
  assertType<Equal<typeof value, number>>();

  // Graph getters.
  assertType<Equal<typeof graph.durability, 'full' | 'normal' | 'off'>>();
  const warnings: string[] = graph.openWarnings;
  void warnings;

  // Errors carry a string code and an optional lease holder.
  try {
    await graph.close();
  } catch (e) {
    const err = e as KgliteError;
    const code: string = err.code;
    const pid: number | undefined = err.holder?.pid;
    const self: boolean | undefined = err.holder?.self;
    const rule: string | undefined = err.rule;
    const entity: 'node' | 'relationship' | undefined = err.entity;
    const property: string | null | undefined = err.property;
    const reportCount: number | undefined = err.report?.[0]?.count;
    void [code, pid, self, rule, entity, property, reportCount];
  }

  // Backup and ontology.
  const report: BackupReport = await graph.backup('/tmp/b.kgl');
  const lsn: number | bigint | null = report.lsn;
  const declared: OntologyDeclared = await graph.declareOntology({ classes: {} });
  await graph.declareOntology('{"classes":{}}');
  await graph.clearOntology();
  const warned: string[] = declared.warnings;
  void [lsn, warned];

  // Value classes.
  const date = new LocalDate(2024, 2, 29);
  const stamp = new LocalDateTime(2024, 2, 29, 12, 30);
  const span = new Duration(1, 2, 3);
  const point = new Point(59.9, 10.7);
  const text: string = date.toString() + stamp.toString() + span.toString() + point.toString();
  const jsDate: Date = stamp.toDate();
  const year: number = date.year;
  void [text, jsDate, year];

  // Nodes are plain objects with numeric ids.
  const node = result.rows[0].node as KgNode;
  const labels: string[] = node.labels;
  void labels;

  // @ts-expect-error integers option accepts only 'safe' | 'bigint'
  await open('/tmp/x.kgl', { integers: 'number' });
}
