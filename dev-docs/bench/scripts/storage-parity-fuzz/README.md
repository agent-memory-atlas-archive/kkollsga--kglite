# storage-parity-fuzz — randomized differential harness (memory / mapped / disk)

Promoted 2026-09-08 from the deep-scan 2026-09-07 storage-parity lane scratch
(session scratchpad, now expired). Durable copy of the only existing version.

- `harness.py` — randomized differential harness: a pure-Python `Model` vs
  `{memory, mapped, disk}`, index on/off, planner passes on/off. Env:
  `SP_SCRATCH` (work dir), `SP_MODES` (comma list).
- `h2.py` — replayable variant with recorded scripts + delta-debug minimization.
- `minimize.py` / `minimize2.py` — delta-debug drivers over `h2.run_script`.
- `instrument.py` — replays one recorded seed's script step by step.

Findings from this lane are in `dev-docs/plans/deep-scan-2026-09-07/storage-parity.md`
(and the report at `dev-docs/plans/deep-scan-2026-09-07.md`).

**Open follow-up:** promote to an opt-in pytest marker so the corpus runs in CI
instead of only by hand. See `dev-docs/todos.md`.
