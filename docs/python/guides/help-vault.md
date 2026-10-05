# A small, auditable help knowledge base

This tutorial builds a synthetic three-page help corpus. It needs no model account, vendor data, or KGLite installation.

The fixture is intentionally bounded. Its checks demonstrate a preservation method you can adapt. They do not claim that the script is a general HTML converter.

Run it from the repository root, choosing paths in a temporary workspace:

```bash
work_dir="$(mktemp -d)"
python -S examples/knowledge_base/knowledge_base.py build --output "$work_dir/vault"
python -S examples/knowledge_base/knowledge_base.py check --vault "$work_dir/vault"
python -S examples/knowledge_base/knowledge_base.py query \
  --vault "$work_dir/vault" procedure --page-size 2
```

`-S` disables site packages and proves this path uses the Python standard library. `build` refuses a nonempty output directory, so the example cannot mix regenerated files with an existing tree by accident.

## What the checks establish

The build report records three things:

- a SHA-256 inventory of every mirrored original;
- every original-to-note mapping;
- every hyperlink found in the originals.

`check` compares those three records independently. The hashes establish byte identity for this mirror. They are separate from KGLite's cache fingerprint.

`check` also rejects placeholder text and checks facts a generic file count misses:

- the HTML `rowspan` and `colspan` table becomes an explicit rectangular
  logical grid, while the original HTML remains under `Sources/`;
- two distinct `ValueError` conditions remain two blocks;
- a safety warning remains attached to the reset procedure;
- all four expected cross-page references remain accounted for.

The generated Markdown table repeats merged header values, because pipe tables cannot encode merged cells. This fixture-specific normalization makes the logical cells testable. For a complex table, preserving an explicit JSON grid or serving the original may be more honest than forcing it into Markdown.

The wrapped `warning.svg`, downloadable checklist, and hidden catalogue marker are inventoried and retained under `Sources/`. SVG is an exact-source attachment in this example. KGLite's bundled image fetch tool serves raster PNG/JPEG/GIF/WebP files. The tutorial therefore does not claim that it renders this SVG through that tool.

The query result reports `total`, `returned`, `truncated`, and ordered `pages`. With a page size of two, the warning is beyond the first page.

- Traverse every page and assert `returned == total`. A preview alone cannot prove the complete procedure was recovered.
- The source handle in the same payload points back to `procedure.html`.

The negative tests deliberately do four things: remove a reference, merge an exception condition, omit the late warning, and inject a private canary into a share. Each mutation must make its own check fail:

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

`build` derives a fresh vault from those inputs. It never asks you to edit a generated note, so regeneration retains the separately owned workflow, annotation, and memory.

`check` fails if the annotation's recorded source hash is stale. To resolve that, review the changed original, update the overlay input, then build a new output directory.

The memory's narrow activation terms are part of the application convention, not native KGLite fields.

- A direct API-use task may retrieve the memory.
- A documentation index lookup and an unrelated answer must not disclose it.
- To correct the observation, update the source memory and rebuild, so the current statement replaces the old one.
- To delete it, remove its source input, rebuild or invalidate derived state, restart or refresh the served graph, and verify absence through retrieval and a fresh open.
- Conversation logs and backups are outside that deletion.
- Query filtering is not access control.

Exercise correction and deletion against a copied input set, leaving the repository fixture unchanged:

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

The second build is fresh derived state. The test suite also opens that fresh output independently. It does not infer deletion from a filter on the old vault.

## Build a public share from an allowlist

Create a new archive. Do not subtract guessed private filenames from an old one:

```bash
python -S examples/knowledge_base/knowledge_base.py share \
  --vault "$work_dir/vault" --output "$work_dir/public.zip"
python -S examples/knowledge_base/knowledge_base.py verify-share \
  --archive "$work_dir/public.zip"
```

The allowlist includes articles, mirrored public sources, reviewed annotations, workflows, and the report. It excludes `Memories/` and derived `.kglite/` caches.

Verification inspects every archive member, rejects unsafe paths and duplicates, and searches every member for the synthetic private canary. This is a concrete regression check for this corpus, not a privacy scanner for arbitrary data.

A real release policy must enumerate attachments, hidden files, copied skill/recipe text, annotations, and every other place private content can land. Extract the archive in a clean temporary directory and repeat its source and retrieval checks there.

## Optional KGLite navigation

If a current KGLite extension is installed, run the distinct script subcommand:

```bash
python examples/knowledge_base/knowledge_base.py kglite-query \
  --vault "$work_dir/vault" procedure --page-size 2
```

The built vault also carries a complete read-only MCP manifest. Verify the configuration with a live handshake. Then use the same command without `--selftest` as the stdio launch command in your MCP client:

```bash
kglite-mcp-server --vault "$work_dir/vault" --vault-cache none \
  --mcp-config "$work_dir/vault/mcp.yaml" --selftest

kglite-mcp-server --vault "$work_dir/vault" --vault-cache none \
  --mcp-config "$work_dir/vault/mcp.yaml"
```

Vault mode binds source tools to the vault root. Public originals live under its allowlisted `Sources/` mirror. After a structured result identifies `Client.connect`, expand the exact source with:

```json
{"file_path":"Sources/api.html","grep":"Client.connect|Raises","grep_context":1}
```

The generated vault derives these rows from that source page:

- one `ApiSymbol`;
- three `ApiParameter` rows;
- one `ApiReturn`;
- three `ApiException` rows.

The symbol keeps its owner, complete signature, literal nested default, return, source anchor, and source SHA-256. Exception rows use distinct `condition_id` values, so the two `ValueError` conditions cannot overwrite one another. These are fixture conventions exercised by `tests/test_knowledge_base_example.py`, not universal KGLite fields.

The `kglite-query` subcommand does four things:

- It opens the source-backed vault with the `obsidian` dialect.
- It traverses all three `Article` nodes in deterministic pages.
- It checks the selected topic exists.
- It reports the native graph-carried recipe schema.

The MCP recipe route applies recipe defaults, not `KnowledgeGraph.cypher`. Invoke `run_recipe_query` as shown below to test omission, override, and refusal.

```json
{"recipe":"help","query":"article_index","variables":{}}
{"recipe":"help","query":"article_index","variables":{"limit":1}}
{"recipe":"help","query":"article_index","variables":{"limit":0}}
```

Through the real bundled MCP server, these return two rows from the default, one row from the override, and an `invalid_variables` error naming `$.limit`. The executable contract is `tests/test_knowledge_base_mcp.py`.

### Retained rows in a `ResultView`

The returned `ResultView` carries exact retained-row information in `diagnostics`. When a `row_limit` cuts results short, `total_rows` is the full count and you must surface the warning.

`head()` is only a preview and has no query diagnostics. Keep the original result when deciding whether to fetch or export the complete set. Use `FORMAT CSV` or explicit deterministic paging for a complete large result. Do not concatenate overlapping previews.

### Recipe parameters in an MCP deployment

A recipe parameter is optional only when its top-level schema property declares `default`.

- Omission binds that default before the stored query runs.
- An explicit override wins.
- An explicit `null` remains null and must satisfy the declared type.
- Every property without a default must appear in `required`. A defaulted property must not.
- A wrong-type/out-of-range default, a nested default, an unknown argument, or a missing required argument is an error.

Recipe results return columns, positional rows, and `row_count`. They are all-or-error above 200 rows rather than silently truncated. See {doc}`mcp-servers` for the exact `list_recipe_queries` and `run_recipe_query` request, return, and error envelopes.

## Portable lifecycle

The lifecycle remains useful if you never adopt KGLite. These are properties of the knowledge-base process, not of one graph engine:

- inventory;
- mapping;
- reference accounting;
- source-backed citations;
- complete traversal;
- regeneration ownership;
- fresh allowlisted sharing.
