# A small, auditable help knowledge base

This tutorial builds a synthetic three-page help corpus. It needs no model
account, vendor data, or KGLite installation. The fixture is intentionally
bounded: its checks demonstrate a preservation method you can adapt; they do
not claim that the script is a general HTML converter.

Run it from the repository root, choosing paths in a temporary workspace:

```bash
work_dir="$(mktemp -d)"
python -S examples/knowledge_base/knowledge_base.py build --output "$work_dir/vault"
python -S examples/knowledge_base/knowledge_base.py check --vault "$work_dir/vault"
python -S examples/knowledge_base/knowledge_base.py query \
  --vault "$work_dir/vault" procedure --page-size 2
```

`-S` disables site packages and proves this path uses the Python standard
library. `build` refuses a nonempty output directory. The example therefore
cannot mix regenerated files with an existing tree by accident.

## What the checks establish

The build report records a SHA-256 inventory of every mirrored original,
every original-to-note mapping, and every hyperlink found in the originals.
`check` compares those three records independently. The hashes establish byte
identity for this mirror; they are separate from KGLite's cache fingerprint.
It also rejects placeholder text and checks facts a generic file count misses:

- the HTML `rowspan` and `colspan` table becomes an explicit rectangular
  logical grid, while the original HTML remains under `Sources/`;
- two distinct `ValueError` conditions remain two blocks;
- a safety warning remains attached to the reset procedure;
- all four expected cross-page references remain accounted for.

The generated Markdown table repeats merged header values because pipe tables
cannot encode merged cells. This fixture-specific normalization makes the
logical cells testable. For a complex table, preserving an explicit JSON grid
or serving the original may be more honest than forcing it into Markdown.

The wrapped `warning.svg`, downloadable checklist, and hidden catalogue marker
are inventoried and retained under `Sources/`. SVG is an exact-source
attachment in this example; KGLite's bundled image fetch tool serves raster
PNG/JPEG/GIF/WebP files, so the tutorial does not claim that it renders this
SVG through that tool.

The query result reports `total`, `returned`, `truncated`, and ordered `pages`.
With a page size of two, the warning is beyond the first page. Traverse every
page and assert `returned == total`; a preview alone cannot prove the complete
procedure was recovered. The source handle in the same payload points back to
`procedure.html`.

The negative tests deliberately remove a reference, merge an exception
condition, omit the late warning, and inject a private canary into a share.
Each mutation must make its own check fail:

```bash
uv run --no-sync pytest -q tests/test_knowledge_base_example.py
```

## Keep generated facts, methods, and memory separate

The inputs have four owners:

- `originals/` contains synthetic vendor pages;
- `overlays/` contains a reviewed annotation with a stable ID, exact source
  page and source hash, status, precedence, and local ownership statement;
- `workflows/` contains an operator method and says that it is not a vendor
  fact;
- `private/` contains a synthetic memory with scope, observation date, status,
  confidence, sensitivity, activation terms, and recheck instructions.

`build` derives a fresh vault from those inputs. It never asks you to edit a
generated note, so regeneration retains the separately owned workflow,
annotation, and memory. `check` fails if the annotation's recorded source hash
is stale. Resolve that by reviewing the changed original and updating the
overlay input, then build a new output directory.

The memory's narrow activation terms are part of the application convention,
not native KGLite fields. A direct API-use task may retrieve it. A documentation
index lookup and an unrelated answer must not disclose it. If the operator
corrects the observation, update the source memory and rebuild so the current
statement replaces the old one. To delete it, remove its source input, rebuild
or invalidate derived state, restart or refresh the served graph, and verify
absence through retrieval and a fresh open. Conversation logs and backups are
outside that deletion. Query filtering is not access control.

Exercise correction and deletion against a copied input set, leaving the
repository fixture unchanged:

```bash
cp -R examples/knowledge_base "$work_dir/inputs"
python -c 'from pathlib import Path; p=Path("'$work_dir'/inputs/private/memory.md"); p.write_text(p.read_text().replace("PRIVATE-CANARY-7F3A", "CORRECTED-ACCESS"))'
python -S examples/knowledge_base/knowledge_base.py build \
  --source "$work_dir/inputs" --output "$work_dir/corrected"
grep -R "CORRECTED-ACCESS" "$work_dir/corrected/Memories"
rm "$work_dir/inputs/private/memory.md"
python -S examples/knowledge_base/knowledge_base.py build \
  --source "$work_dir/inputs" --output "$work_dir/deleted"
test ! -e "$work_dir/deleted/Memories"
! grep -R "CORRECTED-ACCESS" "$work_dir/deleted"
```

The second build is fresh derived state. The test suite also opens that fresh
output independently; it does not infer deletion from a filter on the old
vault.

## Build a public share from an allowlist

Create a new archive rather than subtracting guessed private filenames from an
old one:

```bash
python -S examples/knowledge_base/knowledge_base.py share \
  --vault "$work_dir/vault" --output "$work_dir/public.zip"
python -S examples/knowledge_base/knowledge_base.py verify-share \
  --archive "$work_dir/public.zip"
```

The allowlist includes articles, mirrored public sources, reviewed annotations,
workflows, and the report. It excludes `Memories/` and derived `.kglite/`
caches. Verification inspects every archive member, rejects unsafe paths and
duplicates, and searches every member for the synthetic private canary. This is
a concrete regression check for this corpus, not a privacy scanner for arbitrary
data. A real release policy must enumerate attachments, hidden files, copied
skill/recipe text, annotations, and every other place private content can land;
extract it in a clean temporary directory and repeat its source and retrieval
checks there.

## Optional KGLite navigation

With a current KGLite extension installed, run the distinct script
subcommand:

```bash
python examples/knowledge_base/knowledge_base.py kglite-query \
  --vault "$work_dir/vault" procedure --page-size 2
```

This opens the source-backed vault with the `obsidian` dialect, traverses all
three `Article` nodes in deterministic pages, checks the selected topic exists,
and reports the native graph-carried recipe schema. Recipe defaults are applied
by the MCP recipe route rather than by `KnowledgeGraph.cypher`; invoke
`run_recipe_query` as shown below to test omission, override, and refusal.

```json
{"recipe":"help","query":"article_index","variables":{}}
{"recipe":"help","query":"article_index","variables":{"limit":1}}
{"recipe":"help","query":"article_index","variables":{"limit":0}}
```

Through the real bundled MCP server, these return two rows from the default,
one row from the override, and an `invalid_variables` error naming `$.limit`.
The executable contract is `tests/test_knowledge_base_mcp.py`.

The returned `ResultView` carries exact retained-row information in `diagnostics`: when a
`row_limit` cuts results short, `total_rows` is the full count and the warning
must be surfaced. `head()` is only a preview and has no query diagnostics, so
keep the original result when deciding whether to fetch or export the complete
set. Use `FORMAT CSV` or explicit deterministic paging for a complete large
result; do not concatenate overlapping previews.

For an MCP deployment, a recipe parameter is optional only when its top-level
schema property declares `default`. Omission binds that default before the
stored query runs; an explicit override wins; explicit `null` remains null and
must satisfy the declared type. Every property without a default must appear in
`required`, while a defaulted property must not. A wrong-type/out-of-range
default, a nested default, an unknown argument, or a missing required argument
is an error. Recipe results return columns, positional rows, and `row_count`;
they are all-or-error above 200 rows rather than silently truncated. See
{doc}`mcp-servers` for the exact `list_recipe_queries` and `run_recipe_query`
request, return, and error envelopes.

The portable lifecycle remains useful if you never adopt KGLite: inventory,
mapping, reference accounting, source-backed citations, complete traversal,
regeneration ownership, and fresh allowlisted sharing are properties of the
knowledge-base process rather than of one graph engine.
