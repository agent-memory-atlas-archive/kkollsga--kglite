// Child process driven by crash.test.mjs and lease.test.mjs.
//
//   node child.mjs stream <path> <durability>    acknowledged writes, one `ack <i>` line each
//   node child.mjs hold   <path> <durability>    5 rows, checkpoint, 3 more rows, `ready`;
//                                                `close` on stdin closes the graph first, any
//                                                other input ends the process without closing
import { createRequire } from 'node:module';
import { createInterface } from 'node:readline';

const require = createRequire(import.meta.url);
const kglite = require('../index.js');
const [mode, path, durability] = process.argv.slice(2);

const graph = await kglite.open(path, { durability });
if (mode === 'stream') {
  for (let i = 0; ; i++) {
    await graph.executeWrite('CREATE (:Item {i: $i})', { i });
    process.stdout.write(`ack ${i}\n`);
  }
} else {
  for (let i = 0; i < 5; i++) await graph.executeWrite('CREATE (:Item {i: $i})', { i });
  await graph.checkpoint();
  for (let i = 5; i < 8; i++) await graph.executeWrite('CREATE (:Item {i: $i})', { i });
  process.stdout.write('ready\n');
  for await (const line of createInterface({ input: process.stdin })) {
    if (line === 'close') {
      await graph.close();
      process.stdout.write('closed\n');
    } else if (line === 'exit') {
      process.exit(0);
    }
  }
}
