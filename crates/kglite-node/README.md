# kglite-node

Embedded graph database with Cypher for Node.js. The engine runs inside your process, stores a graph in one `.kgl` file, and shares that file format with the Python wheel and the Bolt server.

## Install

```bash
npm install kglite-node
```

Prebuilt binaries cover macOS (arm64, x64), Linux (x64, arm64; glibc 2.35 or newer, and musl) and Windows x64. You need Node 20 or newer and no Rust toolchain.

## Quick start

```js
const { open } = require('kglite-node');

const graph = await open('./people.kgl');
await graph.executeWrite('CREATE (:Person {name: $name})', { name: 'Ada' });
const { rows } = await graph.executeRead('MATCH (p:Person) RETURN p.name AS name');
console.log(rows); // [ { name: 'Ada' } ]
await graph.close();
```

TypeScript types ship with the package.

## Documentation

- [Node.js guide](https://kglite.readthedocs.io/en/latest/node/index.html): options, durability, value mapping, transactions, errors, multi-process rules.
- [Cypher reference](https://github.com/kkollsga/kglite/blob/main/CYPHER.md)
- [KGLite](https://github.com/kkollsga/kglite): the engine, the Python wheel and the other bindings.

## License

MIT
