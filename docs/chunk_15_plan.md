# Chunk 15: Benchmarks and Coverage

## Status

Chunks 1–14 delivered a full durable triple store with WAL persistence, predicate
registry recovery, and entity ID stability. 144 tests pass. Chunk 15 is a
maintenance sprint before the interpreter bootstrap (now chunk 16): it installs
two project-wide quality tools — a Criterion benchmark suite for the triple store
backends and `cargo-llvm-cov` test coverage reporting.

Neither item adds production code or changes public API.

---

## Item 1 — Criterion Benchmark Suite

### Goal

Establish a reproducible, regression-detectable baseline for the two
`TripleStore` backends. The suite must be runnable in a single `cargo bench`
invocation and produce Criterion's HTML report locally.

### Dependency changes

**`Cargo.toml` (workspace)** — add to `[workspace.dependencies]`:

```toml
criterion = { version = "0.5", features = ["html_reports"] }
```

**`crates/triple-store/Cargo.toml`** — add to `[dev-dependencies]` and declare
the benchmark binary:

```toml
criterion = { workspace = true }

[[bench]]
name = "triple_store"
harness = false
```

### Benchmark file

**`crates/triple-store/benches/triple_store.rs`**

All benchmarks use a `TickClock` (already defined in test helpers) injected via
`create_with_clock` so wall-clock variance does not inflate durable-store timings.
For in-memory benchmarks, `InMemoryTripleStore::new()` is used directly.

The file is organised into Criterion benchmark groups:

#### Group `in_memory/assert`

Parametrized over batch size N ∈ {1, 100, 1_000}:
- `assert_single_N`: commit N `assert_single` ops in one transaction. Each fact
  has a distinct subject so no supersession occurs. Measures pure insert
  throughput.
- `assert_single_supersede_N`: commit N `assert_single` ops against the *same*
  subject/predicate/asserter to force N−1 `Updated` changes. Measures supersede
  throughput.

#### Group `in_memory/query`

Setup: pre-populate the store with 10_000 distinct-subject facts.

- `query_open_hit`: `query_open` for a subject known to have one open fact.
- `query_open_miss`: `query_open` for a subject with no facts.
- `history_10k`: `history` scan over all 10_000 facts on a single subject by
  iterating `subject` IDs — measures history read path.

#### Group `durable/assert`

Uses `tempfile::tempdir()` for each benchmark iteration group; the directory
is created in Criterion's setup closure.

- `durable_assert_single`: single-fact transaction committed to WAL. Measures
  encode + write + fsync latency.
- `durable_assert_batch_100`: 100-fact transaction committed as one WAL record.

#### Group `durable/replay`

Pre-populate a WAL file with N ∈ {100, 1_000, 10_000} facts outside the timed
region, then measure `DurableFileTripleStore::open` (full log replay).

- `replay_N`: open a pre-populated WAL with N committed facts.

#### Group `store/overhead`

Measures the `Store` façade overhead on top of the in-memory backend.

- `store_declare_10`: declare 10 distinct predicates (exercises the ArcSwap
  publish path and WAL no-op default).
- `store_assert_single`: one `assert_single` call through `Store` with
  `ExecContext::System` (access check + transaction round-trip).

### Running benchmarks

```bash
# Full suite, HTML report at target/criterion/
cargo bench -p triple-store

# Single group (faster iteration)
cargo bench -p triple-store -- in_memory/assert

# Baseline save / comparison (Criterion built-in)
cargo bench -p triple-store -- --save-baseline before
# … make changes …
cargo bench -p triple-store -- --baseline before
```

Criterion saves baselines under `target/criterion/`. That directory is
`.gitignore`-able (large HTML output) but baseline JSON files under
`target/criterion/<name>/base/` can optionally be committed to track regressions
in CI.

### `.gitignore` addition

```
target/criterion/
```

---

## Item 2 — Test Coverage with `cargo-llvm-cov`

### Goal

Produce a human-readable HTML coverage report and an LCOV file suitable for
future CI integration, covering all workspace crates.

### Tool installation

`cargo-llvm-cov` is a cargo subcommand installed into the developer's toolchain;
it is **not** a `Cargo.toml` dependency.

```bash
cargo install cargo-llvm-cov --locked
```

This also requires the `llvm-tools-preview` rustup component, which
`cargo-llvm-cov` installs automatically on first run if missing.

### Usage

```bash
# HTML report — open target/llvm-cov/html/index.html
cargo llvm-cov --workspace --html

# LCOV for CI / coverage badge tools
cargo llvm-cov --workspace --lcov --output-path lcov.info

# Summary table to stdout (no file output)
cargo llvm-cov --workspace

# Include doctests
cargo llvm-cov --workspace --doctests --html
```

The coverage run exercises all `#[test]`, `#[cfg(test)]` modules, and the
integration tests under `crates/triple-store/tests/`.

Benchmarks are excluded automatically because they use `harness = false` and
do not run under `cargo test`.

### Exclusion configuration

Create **`crates/triple-store/.llvm-cov-exclude`** (or use inline attributes)
to exclude generated or trivially-thin code:

```toml
# .cargo/llvm-cov.toml (workspace-level, if desired)
[report]
exclude-from-report = [
    "*/tests/*",          # integration test helpers are not production code
]
```

Alternatively, annotate with `#[cfg(not(coverage))]` for specific blocks. For
now, no exclusions are needed — coverage of the test files themselves is harmless
to report.

### `.gitignore` additions

```
lcov.info
target/llvm-cov/
```

### Current baseline (chunk 15, first run)

Measured with `cargo-llvm-cov 0.8.6`, toolchain `1.94.1`, 142 tests passing.

| Module | Line cover | Notes |
|---|---|---|
| `predicate_registry` | 100.00% | |
| `exec_context` | 100.00% | |
| `triple_store` | 100.00% | |
| `durable_file_store` | 92.99% | WAL serialization error branch, `replay_change` log-inconsistency fallback uncovered |
| `in_memory_store` | 96.42% | |
| `transaction` | 98.39% | |
| `time` | 95.77% | |
| `fact` | 96.84% | |
| `predicates` | 87.50% | |
| `intern` | 89.53% | |
| `store` | 76.94% | Unexercised: `assert_set`, `assert_indexed`, `retract_set`, `retract_indexed` via `Store`; query/subscription paths not reached through `Store` wrapper |
| `term` | 84.00% | |
| `change` | 54.55% | Only `ChangeKind` variants exercised through existing paths; direct `Subscription` drop/close paths missing |
| **TOTAL** | **92.14%** | |

Known acceptable gaps: WAL error branches (require fault injection),
`replay_change` log-inconsistency path, and `Store`-facade wrappers for
`assert_set` / `assert_indexed` / `retract_*` variants (all covered at the
backend level; Store wrapper tests not yet added for every cardinality).

---

## Implementation Order

1. Add `criterion` to workspace and crate `Cargo.toml`.
2. Write `benches/triple_store.rs` — start with `in_memory/assert` group, then
   add remaining groups. Verify `cargo bench -p triple-store -- --test` compiles
   and runs without panicking before running timed benchmarks.
3. Add `target/criterion/` and `lcov.info` to `.gitignore`.
4. Install `cargo-llvm-cov`, run `cargo llvm-cov --workspace --html`, record the
   baseline numbers in a comment in this plan or in `docs/`.
5. Commit both items together (one commit: "Add criterion benchmarks and
   llvm-cov coverage reporting").

---

## Open Questions

### Benchmark placement for future crates

When the interpreter crate is added (chunk 16+), its benchmarks should live in
`crates/interpreter/benches/`. The `criterion` workspace dependency is already
shared; each crate declares its own `[[bench]]` section.

### CI integration

Coverage upload (Codecov, Coveralls) and benchmark regression gates (Bencher.dev,
GitHub Actions artifact comparison) are deferred. This chunk only establishes
local tooling. CI wiring is a follow-on once the project has a CI pipeline.

### Durable benchmark I/O variance (future performance work)

`DurableFileTripleStore` benchmarks create real files in `tempfile::tempdir()`.
On macOS with APFS, `sync_data()` carries non-deterministic per-call overhead
due to the filesystem's own journaling and CoW mechanics. High variance in
durable benchmarks is therefore expected and is fundamentally tied to the
current per-transaction `sync_data` durability contract — not simply a
buffering problem.

Three options exist for future consideration, in rough order of WAL complexity:

1. **`create_with_no_sync` benchmark mode.** Skip `sync_data` inside a test-only
   construction path so benchmarks measure encode + write latency in isolation.
   Does not address production variance; useful only for attributing benchmark
   noise.

2. **Group commit.** Batch multiple concurrent transactions into a single WAL
   flush window and issue one `sync_data` for the group. Per-transaction
   durability is preserved (the record hits disk before the transaction is
   acknowledged), but the WAL framing and commit protocol become materially more
   complex. This is the pattern used by PostgreSQL and SQLite WAL mode and is
   the most direct lever for reducing both latency and variance under concurrent
   load.

3. **Per-transaction private logs with buffered merge.** Each transaction
   sync-writes its record to its own ephemeral file (or dedicated segment),
   ensuring no two in-flight transactions contend on the same `sync_data` call.
   A background process merges settled records into the canonical WAL. This
   eliminates cross-transaction fsync interference at the cost of a more
   complex recovery path: replay must locate and order all partial logs, and
   the merge buffer introduces an intermediate durability state between
   "transaction synced" and "transaction in main WAL."

None of these should be pursued in chunk 15. Record the observed variance
numbers after the first benchmark run so future work has a concrete baseline
to improve against.
