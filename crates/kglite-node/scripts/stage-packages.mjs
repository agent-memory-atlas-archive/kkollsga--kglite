// Stage the npm packages from per-platform artifacts and pack them.
//
// usage: node stage-packages.mjs <version> <natives-dir> <out-dir>
//   natives-dir  searched recursively for kglite-node.<platform>.node files
//   out-dir      receives one .tgz per package (main + one per napi target)
//
// Fails unless every napi target in package.json has its .node file, so a
// partial platform set can never be packed. Runs from a staging copy so the
// committed package.json (placeholder version) is never modified.
import { execFileSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, symlinkSync, writeFileSync, copyFileSync, existsSync } from 'node:fs';
import { join, resolve, dirname, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

const [version, nativesArg, outArg] = process.argv.slice(2);
if (!/^\d+\.\d+\.\d+/.test(version ?? '') || !nativesArg || !outArg) {
  console.error('usage: stage-packages.mjs <x.y.z> <natives-dir> <out-dir>');
  process.exit(2);
}
const crate = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const natives = resolve(nativesArg);
const out = resolve(outArg);
const stage = join(out, '.stage');
rmSync(stage, { recursive: true, force: true });
mkdirSync(stage, { recursive: true });
for (const f of ['package.json', 'index.js', 'index.d.ts']) copyFileSync(join(crate, f), join(stage, f));
copyFileSync(resolve(crate, '../../LICENSE'), join(stage, 'LICENSE'));
symlinkSync(join(crate, 'node_modules'), join(stage, 'node_modules'));

const run = (cmd, args, cwd) => execFileSync(cmd, args, { cwd, stdio: 'inherit' });
const napi = (...args) => run('npx', ['--no-install', 'napi', ...args], stage);

const findNodeFiles = (dir) =>
  readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? findNodeFiles(join(dir, e.name)) : e.name.endsWith('.node') ? [join(dir, e.name)] : [],
  );
const available = new Map(findNodeFiles(natives).map((p) => [basename(p), p]));

const pkg = JSON.parse(readFileSync(join(stage, 'package.json'), 'utf8'));
run('npm', ['version', version, '--no-git-tag-version', '--allow-same-version'], stage);
napi('create-npm-dirs');
napi('version');

const npmDir = join(stage, 'npm');
const platforms = readdirSync(npmDir).sort();
if (platforms.length !== pkg.napi.targets.length) {
  throw new Error(`${platforms.length} npm dirs for ${pkg.napi.targets.length} napi targets`);
}
mkdirSync(out, { recursive: true });
const optional = {};
for (const platform of platforms) {
  const dir = join(npmDir, platform);
  const platformPkg = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
  const file = `${pkg.napi.binaryName}.${platform}.node`;
  const source = available.get(file);
  if (!source) throw new Error(`no ${file} under ${natives} (have: ${[...available.keys()].join(', ') || 'none'})`);
  if (platformPkg.main !== file) throw new Error(`${platformPkg.name} main is ${platformPkg.main}, expected ${file}`);
  if (platformPkg.version !== version) throw new Error(`${platformPkg.name} version ${platformPkg.version} != ${version}`);
  copyFileSync(source, join(dir, file));
  copyFileSync(join(stage, 'LICENSE'), join(dir, 'LICENSE'));
  optional[platformPkg.name] = version;
  run('npm', ['pack', '--pack-destination', out], dir);
}

const mainPath = join(stage, 'package.json');
const mainPkg = JSON.parse(readFileSync(mainPath, 'utf8'));
mainPkg.optionalDependencies = optional;
writeFileSync(mainPath, JSON.stringify(mainPkg, null, 2) + '\n');
run('npm', ['pack', '--pack-destination', out], stage);

const tarballs = readdirSync(out).filter((f) => f.endsWith('.tgz'));
if (tarballs.length !== platforms.length + 1) throw new Error(`expected ${platforms.length + 1} tarballs, got ${tarballs.length}`);
rmSync(stage, { recursive: true, force: true });
console.log(`packed ${tarballs.length} tarballs for ${version}:\n  ${tarballs.sort().join('\n  ')}`);
