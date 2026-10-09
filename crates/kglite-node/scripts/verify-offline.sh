#!/bin/sh
# AC1 verification: install the packed tarballs on a machine with no compiler
# and no registry access, then run the smoke.
#
# usage: verify-offline.sh <tarball-dir> <version> <expected-platform-package>
# Runs inside a node:22-slim / node:22-alpine container (POSIX sh).
set -eu
tarballs=$1
version=$2
expected=$3

# Non-vacuity first: an image that quietly gained a toolchain would make this
# job prove nothing about "install needs no compiler".
for tool in cc gcc g++ clang make cargo rustc; do
  if command -v "$tool" >/dev/null 2>&1; then
    echo "verify: $tool is present; this image is not toolchain-free" >&2
    exit 1
  fi
done
echo "verify: no compiler/cargo present"

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
cp "$here/smoke.cjs" "$work/"
cd "$work"
main=$(ls "$tarballs"/kglite-node-"$version".tgz)
overrides=""
for t in "$tarballs"/kglite-node-*-"$version".tgz; do
  name=$(basename "$t" "-$version.tgz")
  overrides="$overrides\"$name\": \"file:$t\","
done
cat > package.json <<JSON
{ "name": "verify", "version": "0.0.0", "private": true,
  "dependencies": { "kglite-node": "file:$main" },
  "overrides": { ${overrides%,} } }
JSON
# --offline: every dependency is a local tarball, so any registry fetch is a
# packaging bug and must fail rather than succeed over the network.
npm install --offline --no-audit --no-fund

installed=$(ls -d node_modules/kglite-node-* | sed 's|node_modules/||')
if [ "$installed" != "$expected" ]; then
  echo "verify: installed platform package(s) '$installed', expected '$expected'" >&2
  exit 1
fi
node smoke.cjs "$version"
echo "verify ok: $expected"
