# Building a trustworthy knowledge base

A useful knowledge base is more than a folder that can be searched. It keeps
the source available, preserves what a reader could see, exposes the facts that
matter to the work, and produces answers that are complete for the question
without leaking unrelated material. Those are separate properties and should
be tested separately.

This guide is a portable method. You can use plain Markdown and file search, a
relational database, a search index, a graph, or a combination. The examples
use neutral files and checks. The
[worked knowledge-base example](https://github.com/kkollsga/kglite/tree/main/examples/knowledge_base)
(`examples/knowledge_base/`) applies the method to a small synthetic support
corpus. If you want a KGLite implementation, continue with the
[help-vault tutorial](https://kglite.readthedocs.io/en/latest/python/guides/help-vault.html)
(`docs/python/guides/help-vault.md`) and the
[VAULT.md format reference](https://kglite.readthedocs.io/en/latest/reference/vault-format.html)
(`VAULT.md`).

## Start with the work, not the backend

Write down representative tasks before choosing storage. A good task set
includes direct lookups, procedures, questions that combine documents,
follow-ups that depend on earlier context, corrections, and questions that
must not activate private memory. Record what evidence a correct answer needs.

Use Markdown and direct search when the main work is finding a likely document,
reading a coherent section, and citing it. This path is easy to inspect and has
little interface overhead. Add a structured backend when tasks repeatedly need
typed joins, reverse links, ownership, dependency traversal, complete indexes,
or filtering across many documents. Many systems benefit from both: files
remain canonical, direct search is a strong baseline and fallback, and a
rebuildable index or graph handles relational tasks.

Do not decide from corpus size alone. A large, well-titled manual can work well
with search; a small corpus with dense ownership and prerequisite relationships
may justify structure. Compare approaches on the same tasks, sources, guidance,
model, and privacy rules.

## Keep three layers separate

Use separate locations and ownership rules for:

- **Sources:** immutable originals and attachments. Preserve bytes even when
  they are awkward to parse.
- **Generated knowledge:** normalized notes and extracted facts. Rebuild this
  layer from sources.
- **Overlays:** reviewed annotations, operator workflows, and owner memories.
  Preserve these across regeneration and allow them to be removed independently.

A simple layout is enough:

```text
knowledge-base/
  Sources/                 # exact source mirror; not searched twice
  Notes/                   # generated, readable documents
  Facts/                   # optional structured records
  Annotations/             # reviewed claims about exact source evidence
  Workflows/               # operator method, separate from source claims
  Memories/                # owner-local observations
  manifests/               # inventories, mappings, exclusions, evaluations
```

The names are conventions, not fields promised by any database. Keep the raw
mirror reachable through an exact-source tool or file path, but exclude it from
the normal corpus if indexing it would duplicate every document.

## Define stable identity, provenance, and semantics

Give every durable item an ID that does not depend on its display title or
folder. A source-derived ID can combine a source-system name with the source's
own immutable key. If no key exists, maintain an explicit mapping rather than
quietly changing IDs when titles move.

For every derived fact or relationship, retain:

- the source item ID and exact location, such as a heading, table cell, or
  fragment;
- the source revision or content hash used to derive it;
- the extraction method and, where useful, its version;
- whether the value was quoted, normalized, calculated, inferred, or reviewed;
- review status and the time it was last checked.

Model only semantics the source actually asserts. A table of contents,
breadcrumb, UI route, related-topic list, API index, symbol owner, and execution
prerequisite are different relationships. The word “dependency” in a heading
does not prove execution order, and a prose mention of “related” is not a
curated related-topic link. Use a neutral link for ordinary references. Assign
a stronger type only when an exact source structure supports it, and retain
that provenance.

Preserve alternative UI routes instead of manufacturing one route from nearby
concepts. Preserve repeated exception conditions as separate facts even when
they share an exception type. Keep quoted and nested defaults as source text;
normalization must not change their meaning.

## Convert without losing evidence

Create an immutable inventory before conversion. At minimum, record each
member's relative path, byte size, and SHA-256 digest, including hidden files
and attachments. Record a source-to-output mapping even when a source produces
no searchable note.

Then reconcile four acceptance levels independently:

| Level | What it proves | A useful check |
| --- | --- | --- |
| Original bytes available | Every source member can be recovered exactly | Rehash the source mirror and compare every path, size, and digest |
| Rendered content preserved | A reader's prose, order, warnings, tables, images, downloads, and nested blocks survived | Compare normalized prose and logical table cells; account for every local reference |
| Facts queryable | Chosen fields and relationships return exact, source-backed values | Run expected-value queries, including negative and multi-document cases |
| Answers complete and scoped | Retrieval supplies decisive context without unrelated disclosure | Grade fresh end-to-end sessions against source-backed rubrics |

Counts and successful parsing are useful format checks, but they cannot prove
these four outcomes. A converter can emit the expected number of pages while
dropping a wrapped image, a nested warning, a download, or the meaning of a
merged table.

For tables with row spans, column spans, or layered headers, construct a
logical cell grid and test it. Plain Markdown cannot represent every merged
layout, so keep the original and document the normalization. Detect known
placeholder text and list unparsed or non-searchable material in the report.
Never count silently discarded material as covered.

Use small adversarial fixtures before converting the full corpus: a linked
image wrapped in another element, a nested block, a downloadable file, layered
table headers, repeated exception types, and a decisive warning late in a long
procedure. Each fixture should fail its check when the relevant content is
removed or merged incorrectly.

## Make facts queryable without breaking the document

Keep procedures coherent enough to read as a unit: prerequisites, ordered
steps, required controls, the final action, warnings, and verification belong
together. Structure can add navigation, but a result preview must not separate
a warning from the action it qualifies.

For API material, extract signatures, owners, parameter names and types,
literal defaults, returns, and each exception condition with an exact source
handle. For GUI material, derive controls, ranges, defaults, and constraints
only from explicit source statements. Test exact expected values; a nonzero row
count does not establish correctness.

Every collection response should make completeness visible. A practical
application response reports `total`, `returned`, whether it is `truncated`, a
deterministic continuation token or offset, and an exact source-expansion
handle. These are response-design conventions, not assumed native fields in a
backend. Test an index larger than one response and verify that complete paging
has neither gaps nor duplicates. Test nested arrays, long strings, examples,
and previews too; a row limit says nothing about truncation inside a row.

## Retrieve for complete, concise answers

Route from the task to a likely source, fetch the decisive context once, and
stop when the task is covered. Concision means removing repetition. It does not
mean imposing a fixed word or line cap that drops a prerequisite or warning.

Keep exact source expansion available after every summary or structured
result. The answering layer should be able to recover the complete procedure,
table, example, or attachment and cite the source location. Preserve task
constraints across turns, such as “inspect only,” “do not run,” or “do not
save.” API follow-ups must retain the entity, owner, identifier, data type, and
resource lifetime needed for valid code.

Treat direct file search as a maintained baseline even after adding a backend.
If the structured route needs more discovery and context while answer quality
is unchanged, that is useful evidence about the deployed interface. It is not
proof that structure is useless; relational tasks may tell a different story.

## Add enrichment as regeneration-safe overlays

Generated notes should be disposable. Fix systematic source conversion errors
in the converter, then regenerate. Put legitimate custom work in separately
owned overlays joined by stable source ID and exact evidence location or hash.

An annotation should state its evidence hash, review status, reviewer, and
precedence. During regeneration, mark it stale if its evidence disappears or
changes; never apply it silently to a nearby heading. Workflow cards should
identify themselves as operator methodology so an answer does not present a
local practice as a vendor claim.

A safe regeneration sequence is:

1. inventory and hash the new sources;
2. regenerate notes and facts into a fresh directory;
3. reconcile bytes, rendered content, and expected facts;
4. reattach overlays by stable ID and exact evidence;
5. quarantine stale or ambiguous overlays for review;
6. atomically replace the previous generated layer;
7. rebuild indexes and rerun answer evaluations.

## Treat memory as a removable, scoped overlay

Memory records observations about an owner or environment; they are not vendor
facts. Keep them in a separate input set. A useful application-level record
contains a stable ID, scope, observation date, status, provenance, confidence,
sensitivity, activation terms, and recheck instructions. Those names are a
portable convention rather than native keys.

Activation should be narrow. A code-use question may need a current observation
about access to a specific API. A request for that API's documentation index
usually does not. An unrelated answer must not mention the observation. Test
all three cases.

Current user statements override older observations for the active task. Mark
the old record corrected or superseded and capture the new provenance; do not
silently rewrite history. A relevant unchanged memory need not be repeated on
every turn once it is active in the session.

Deletion is a lifecycle, not a file operation:

1. remove or tombstone the canonical memory record;
2. invalidate and rebuild every derived index, graph, embedding, cache, and
   materialized answer that can contain it;
3. refresh or restart served state as the chosen system requires;
4. verify absence by ID, distinctive canary text, generic search, and a fresh
   process or clean open;
5. apply the retention policy separately to logs, backups, and conversations.

Retrieval filtering is not access control. Enforce authorization before
retrieval, and keep sensitive memory out of artifacts that unauthorized users
can download or query.

## Share from an allowlist

Build a public package from declared public inputs rather than deleting known
private files from a copy. Exclude memories, private annotations, credentials,
logs, caches, embeddings, generated databases, and any skills or recipes into
which private content was copied.

Maintain an exclusion manifest and a synthetic private canary. After packaging:

1. list and hash every archive member, including hidden files;
2. extract into a clean temporary directory;
3. search all source and configuration files for the canary;
4. rebuild every derived artifact from the extracted public inputs;
5. search and query the rebuilt state for the canary and private IDs;
6. verify originals, attachments, links, and exact-source access;
7. test cold construction, relocation, and a subsequent warm open separately.

A cache hit on the creator's machine does not prove that a relocated package
will reuse the cache. Treat caches as derived and optional unless the chosen
backend documents and tests portable reuse.

## Evaluate the deployed system

Freeze a task set and source-backed rubric before comparing retrieval paths.
Include direct lookups, long procedures, pagination, relational questions,
multi-document synthesis, follow-ups, current-user corrections, relevant
memory, irrelevant-memory negatives, and privacy canaries. Hold some questions
out of development and add more than one client or model before making broad
claims.

Run fresh sessions and continued sessions. Blind answer grading where practical
and retain pass, partial, and fail outcomes. Measure response completeness in
addition to correctness. Record cached and uncached input, output, model
requests, discovery calls, tool calls, and wall time. Timing should state
whether corpus preparation and cold construction are included.

Usage counters are often cumulative within a session. Record the final
cumulative value once, or subtract adjacent snapshots to obtain per-turn
deltas. Never sum cumulative snapshots; that counts earlier turns repeatedly.
Retain failed runs and document harness repairs instead of quietly replacing
them. Compare quality first, then resource use among outcomes that meet the
same quality and privacy bar.

### Measure an MCP catalogue without calling it token usage

If a system exposes MCP tools, capture its complete `tools/list` JSON and run
the offline diagnostic:

```bash
python examples/knowledge_base/interface_budget.py tools-list.json --pretty
```

The report counts tools and, for each tool, the UTF-8 bytes in its description,
compact input schema, compact output schema, and complete tool definition. It
also hashes exact descriptive blocks repeated across tools and reports where
they occur. The input may be a bare tools array, a `{"tools": [...]}` result,
or a JSON-RPC `{"result": {"tools": [...]}}` envelope. Pipe JSON with `-`.

These are interface-shape diagnostics. They do not say whether a client loads
the whole catalogue eagerly, discovers selected tools, repeats descriptions on
later requests, or projects text and structured results together. Dynamically
fetched skill bodies are absent unless the captured JSON contains them. Measure
those behaviors at the client boundary before claiming token savings.

For an exact comparison, preserve the raw catalogue and a client trace for
each arm. Record which tool fields and result representations enter each model
request, discovery calls, model requests, cached and uncached input, output,
answer grade, completeness, privacy failures, and wall time. For each native
session, record the final cumulative usage once; if only snapshots exist,
subtract adjacent snapshots and sum the deltas. Use identical frozen tasks,
sources, guidance, model settings, and fresh versus continued-session design.
Catalogue shrinkage alone is not an acceptance result.

The final release gate should be able to fail when you deliberately remove a
reference, merge two exception conditions, omit a late warning, duplicate a
page of results, keep a deleted memory in an index, or leak the private canary.
That failure is the evidence that the checks protect the properties they name.

## A small worked path

The synthetic example in
[`examples/knowledge_base/`](https://github.com/kkollsga/kglite/tree/main/examples/knowledge_base)
contains source pages, overlays, a manifest, and checks for the
lifecycle in this guide. It uses the Python standard library and explicit
caller-owned output paths. From the repository root, run:

```bash
work_dir="$(mktemp -d)"
python examples/knowledge_base/knowledge_base.py build --output "$work_dir/vault"
python examples/knowledge_base/knowledge_base.py check --vault "$work_dir/vault"
python examples/knowledge_base/knowledge_base.py query --vault "$work_dir/vault" procedure --page-size 2
python examples/knowledge_base/knowledge_base.py share --vault "$work_dir/vault" --output "$work_dir/public.zip"
python examples/knowledge_base/knowledge_base.py verify-share --archive "$work_dir/public.zip"
```

The direct Markdown query is the baseline and needs no vendor data or model
account.

For an optional structured implementation, the
[KGLite help-vault tutorial](https://kglite.readthedocs.io/en/latest/python/guides/help-vault.html)
(`docs/python/guides/help-vault.md`) builds the same corpus, queries
source-backed facts, documents correction and deletion requirements, and
creates a clean share package. With KGLite installed, the example's optional
structured query is:

```bash
python examples/knowledge_base/knowledge_base.py kglite-query \
  --vault "$work_dir/vault" procedure --page-size 2
```

KGLite-specific frontmatter and lifecycle rules belong in the
[VAULT.md format reference](https://kglite.readthedocs.io/en/latest/reference/vault-format.html),
not in this portable method.
