# P3 relationship-embedding WAL measurement

Run only while holding the repository's build/measurement token. The runner
uses the release profile because these are performance measurements, runs each
ignored harness twice, and refuses to overwrite prior evidence.

```bash
dev-docs/bench/scripts/run_edge_wal_p3.sh candidate-1
```

Each harness owns its fixed 20 warmups and 200 measured rounds. The codec CSV
reports encode minimum and append mean. The capture CSV includes minimum and
mean; mean is the primary statistic because capture is a once-per-event cost.
Raw `cargo test --nocapture` output, extracted CSV, and a command/source-state
manifest are written beneath `dev-docs/bench/out/p3-edge-wal-<label>-*`. The
manifest includes a bounded content digest over the runner, both ignored
harnesses, and the codec/capture production modules, including untracked files.
Passing an ignored test here is not a release correctness claim; correctness
uses the normal debug tests and program gates.
