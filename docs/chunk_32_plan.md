# Chunk 32: `#stm` Optimistic Multi-Entity Transactions

## Goal

Add the `#stm` transaction mode on top of C31's shared-`TxId`, shadow-write, and
participant-trampoline infrastructure. `#stm` differs from `#atomic` in exactly
two ways:

1. **No mailbox lock during the transaction body.** Participants are *not*
   locked; unrelated handlers interleave freely. That interleaving is precisely
   what the conflict check polices.
2. **Version conflict check at commit.** Each participant remembers the version
   it had when the transaction first touched it; at commit, a changed version
   means some concurrent handler mutated it underneath us → abort → retry.

`Transaction stm: aBlock` runs `aBlock` optimistically; on a version conflict it
retries (default 3×), and on exhaustion raises a `TxConflict` condition.

## What is reused from C31 (unchanged)

- `TxContext` (`Root`/`Child`), `TxCtxId`, ancestry (`has_ancestor_or_self`).
- `_primBeginSharedTx` / `_primCommitSharedTx:` / `_primRollbackSharedTx:` /
  `_primEnrolledParticipants:`.
- Shadow-write isolation (`tx_shadow_set` / `tx_shadow_get`) and read-your-writes.
- The participant trampoline (`pvtExitTx:` → `_primExitTxMode:`) and the
  durable-buffer merge-up.
- Tagged envelopes and tag propagation (`envelope.tx_tag`,
  `ev.current_tx_ctx`). Auto-enrollment on first tagged message.

## What is new (small)

1. **Per-entity version counter.** `EntityData.version: AtomicU64`, bumped on any
   change to *live* slots.
2. **`TxFrame` becomes a typed enum** distinguishing locking (`Atomic`) from
   non-locking (`Stm`) frames; the `Stm` variant additionally carries the
   snapshot version captured at enrollment.
3. **Ancestor-aware merge** in `tx_commit_frame` — the merge-vs-apply-to-live
   decision keys on genuine ancestry, not "is there a frame below". This is the
   strict improvement that makes atomic/STM interleaving on one entity sound.
4. **`_primCheckVersions:`** — validates a context's participant snapshots.
5. **Smol:** `Transaction stm:` retry loop + `TxConflict` condition class.

---

## Design resolutions (this session)

### R1: `TxFrame` → enum, not a bool flag

Today `tx_frames: Vec<TxFrame>` is *both* a lock stack and a shadow stack — a
non-empty stack means the mailbox is locked (`tx_holder()` → `admissible_index`).
`#stm` needs shadow-without-lock. Rather than a `locking: bool` discriminator,
migrate to a typed enum so "does this frame lock?" is a match, not a flag read:

```rust
pub enum TxFrame {
    Atomic(AtomicFrame),  // locking + shadow (C31 behavior)
    Stm(StmFrame),        // shadow only, version-validated
}

pub struct AtomicFrame {          // == today's TxFrame
    pub ctx_id: TxCtxId,
    pub shadow: HashMap<Arc<str>, Value>,
    pub tx: OwnedStoreTransaction,
}

pub struct StmFrame {
    pub ctx_id: TxCtxId,
    pub shadow: HashMap<Arc<str>, Value>,
    pub tx: OwnedStoreTransaction,
    pub snapshot_version: u64,     // entity's version at enrollment (first touch)
}
```

Shared-field friction is absorbed by accessors on `TxFrame`
(`ctx_id()`, `shadow(&)`, `shadow_mut(&mut)`, `tx_mut(&mut)`, `is_locking()`).

- `tx_holder()` / `admissible_index()` consider **only `Atomic` frames**, so STM
  frames leave admission open (no body lock).
- Read/shadow scan (`tx_shadow_get`) scans **all** frames top-to-bottom
  regardless of variant (read-your-writes is identical for both).

### R2: Atomic/STM frames may interleave; merge becomes ancestor-aware

Because STM frames don't lock, an unrelated `Atomic` frame can be pushed on top
of an `Stm` frame (admission is open while only STM holds). Stack e.g.
`[Stm(S), Atomic(A)]`, where `S` is **not** `A`'s ancestor. Today
`tx_commit_frame` (entity.rs:359) merges the committing top into *whatever* frame
is below, assuming a single ancestry chain (the invariant at entity.rs:250) — an
invariant that held only because every frame locked and serialized siblings.

Fix — the **strict improvement**: `tx_commit_frame` decides merge-vs-apply by
**ancestry**, not existence:

- If the frame below is a genuine ancestor of the committing frame's context
  (same tree; check via the passed `parent_ctx_id` / `has_ancestor_or_self`) →
  merge up (as today).
- If the frame below is *not* an ancestor (an unrelated interleaved frame) →
  **apply to live slots** (root-style), which bumps the entity's version and
  thereby *correctly* conflicts the interleaved STM tx at its own commit check.

This turns the implicit "single-ancestry-chain" invariant into an enforced one
and is correct for atomic-only stacks too (there the frame below is always an
ancestor, so behavior is unchanged).

### R3: Version counter — where and when it bumps

- **Where stored:** `EntityData.version: AtomicU64`.
- **Semantics:** the version reflects generations of *live* slot state.
  Speculative (shadow) writes never bump it; only apply-to-live does.
- **Single-writer:** every write to entity `E`'s version happens in `E`'s own
  coroutine (normal handlers; `tx_commit_frame` apply runs during `E`'s
  `pvtExitTx:` handler). Cross-thread access is read-only (a committing STM tx
  loads a participant's version). So **no `fetch_add`/CAS** — just
  `store(load(Relaxed) + 1, Release)`.
- **Gate:** a static `STM_ACTIVE: AtomicUsize`, incremented at `stm:` begin and
  decremented at commit/abort (rare). The bump is gated:

  ```rust
  if STM_ACTIVE.load(Relaxed) != 0 {
      self.version.store(self.version.load(Relaxed) + 1, Release);
  }
  ```

  Outside any STM transaction (the common case) this is one relaxed load of a
  static and a not-taken branch. Conservatively correct: any STM tx's
  snapshot→commit window keeps the counter positive, so any concurrent live write
  during that window bumps; writes wholly outside all STM windows skip and by
  definition no snapshot spans them.
- **Why `EntityData`, not `Object`:** the version is an *entity-concurrency*
  concept, not an *Object-identity* one. Per the Value/Object split, Values are
  immutable by definition, so a mutation-version on them is not merely unused but
  semantically incoherent (no concurrent writer ever exists to detect). Class
  entities are covered *structurally* because `InstanceKind::ClassEntity` carries
  its own `Box<EntityData>` — so the counter reaches them without living on
  `Object`. (Pre-existing gap, orthogonal to placement: class-side mutable state
  lives in `Class.class_var_storage`, not `Object.slots`; class-var writes
  bypass `set_slot`/shadow entirely, so class-entity *mutable state* does not
  participate in transactions today even under C31 atomic. Extending shadow +
  version-bump to `class_var_storage` is a separate future item — see
  OQ-C32-5.)
- **Bump sites (two, both entity-specific, since `set_slot` is on the shared
  `Object`):**
  1. `assign_var`'s `InstanceKind::Entity(d)` branch, non-tx path (eval.rs:395).
  2. `tx_commit_frame`'s apply-to-live loop (entity.rs:384) — bump once after the
     loop.
- **Fallback (deferred unless profiling demands):** a coroutine-local plain
  `wrote_live` bool converted to a single version store at the message boundary,
  to avoid even the gated load per write.

### R4: Read/write set = participant set; snapshot per `StmFrame`

An STM tx reads or writes an entity by sending it a tagged message; the entity
auto-enrolls (pushes an `StmFrame`). At push time record the entity's *current*
`version` into `StmFrame.snapshot_version`. The transaction's read/write set is
therefore exactly its participant list — no separate per-ivar snapshot; one `u64`
per participant. At commit each participant compares its snapshot to its live
version.

---

## Open questions (resolve before/within implementation)

### OQ-C32-3 (RESOLVED): multi-entity commit atomicity — 2PC with held locks + try-lock/fail-fast

STM = optimistic **body** (no locks; shadow + snapshot) + brief pessimistic
**commit** via two-phase commit:

- **Phase 1 (prepare):** the coordinator drives each participant, in the
  participant's own coroutine, to **lock its mailbox** then **validate** (compare
  live `version` to `StmFrame.snapshot_version`). Vote commit/abort. The lock is
  **held** on a commit vote.
- **Fail-fast:** any abort vote → tell everyone to release (unlock, discard
  shadow); the transaction retries.
- **Phase 2 (commit):** all voted commit → each **applies shadow → live (bumping
  version), then unlocks**.

Correctness hinges on the phase-1 lock being **held continuously through phase 2**
(no validate→unlock→relock gap), so no concurrent handler mutates a validated
participant before apply. Each participant's validate-hold-apply runs in its own
single-threaded coroutine, so the multi-entity commit is atomic.

**Deadlock:** impossible during the body (STM takes no locks, nothing parks).
Possible only at commit when two coordinators lock overlapping participant sets.
Resolution: **phase-1 lock is non-blocking (try-lock)** — if a participant is
already locked by anyone, immediately vote abort. No waiting ⇒ deadlock is
structurally impossible; it folds into the retry loop. Cost: possible livelock,
bounded by retry limit + randomized backoff. Fallback if livelock ever bites:
blocking acquisition in canonical `EntityId` order (STM-vs-atomic still relies on
the C31 wait-for detector).

Reuses C31 almost wholesale: "lock + apply shadow + unlock" already exists in the
`_primExitTxMode:` trampoline; C32 adds only the *validate* vote and the two-phase
sequencing.

### OQ-C32-1 (RESOLVED): shadow-without-lock

Resolved by R1/R2: STM frames live on the same `tx_frames` stack as a distinct
enum variant; admission consults only `Atomic` frames; interleaving is made sound
by ancestor-aware merge.

### OQ-C32-2 (RESOLVED): version granularity + timing

Resolved by R3/R4: entity-level version (one `u64`), snapshot at first touch
(enrollment), gated bump on live-slot writes. Document the coarseness (two STM
txns touching different ivars of the same entity produce a false conflict) in
`tx_design.md`.

### OQ-C32-5: class-entity transactional participation (pre-existing gap)

Class-side mutable state (`Class.class_var_storage`) is not shadowed or
version-tracked — class-var writes bypass `set_slot`/`tx_shadow_set`. So class
entities cannot participate in atomic *or* STM transactions with respect to their
class-var state today (a C31 limitation surfaced during C32 planning). Fixing it
means routing class-var writes through the shadow stack and bumping the class
entity's `EntityData.version`. Deferred; note in `tx_design.md`.

### OQ-C32-4: retry loop location and limits

Per tx_design OQ-TX-4: default retry limit 3, configurable; exhaustion raises
`TxConflict`; no auto-escalation to saga. Retry loop lives in Smol
(`Transaction stm:`). Confirm where retry count/config is threaded.

---

## Steps (draft — pending OQ-C32-3)

### S0: `TxFrame` enum migration (pure refactor, no behavior change)
Migrate `TxFrame` → `enum { Atomic(AtomicFrame), Stm(StmFrame) }` with accessors;
update all `tx_frames` call sites (`tx_holder`, `tx_shadow_*`, `tx_commit_frame`,
`tx_abort_frame`, `admissible_index`, deadlock graph). Only `Atomic` used so far;
all C31 tests still green. Land + verify before adding STM behavior.

### S1: Ancestor-aware merge (strict improvement)
Rework `tx_commit_frame` to decide merge-vs-apply-to-live by ancestry
(`parent_ctx_id` / `has_ancestor_or_self`) rather than frame existence. C31
atomic-only stacks unaffected; add a targeted test for the interleaved case.

### S2: Version counter + gate
Add `EntityData.version`, static `STM_ACTIVE`, gated single-writer bump at the two
entity live-write sites. Unit-test that shadow writes don't bump and live writes
do (only while `STM_ACTIVE`).

### S3: STM enrollment + snapshot
On first tagged message from an STM context, push an `StmFrame` (no lock) and
capture `snapshot_version`. Distinguish STM vs atomic context (a flag/kind on
`TxContext`, or on the envelope tag). Admission untouched (only `Atomic` locks).

### S4: Two-phase commit protocol (OQ-C32-3 resolved)
Implement 2PC with held locks and try-lock/fail-fast prepare:
- **Prepare primitive** (per participant): try-lock the mailbox; if already
  locked by another ctx → vote abort; else validate live `version` vs
  `StmFrame.snapshot_version` → vote commit/abort, holding the lock on commit.
- **Commit primitive** (per participant): apply shadow → live (bumps version),
  unlock.
- **Release primitive** (per participant): unlock + discard shadow (abort path).
The coordinator sequences prepare-all → (all commit ? commit-all : release-all).
Conflict/abort vote → signal the coordinator to roll back + retry.

### S5: Kernel classes
`Transaction stm:` retry loop (default 3, configurable) + `TxConflict` condition
subclass. Mirror `atomic:`'s HandlerContext/TxResult structure.

### S6: Tests
Single-entity STM commit/abort; concurrent-writer conflict → retry → success;
retry exhaustion → `TxConflict`; atomic/STM interleaving on one entity; nested
STM; STM under atomic and vice-versa.

### S7: Docs
Update `tx_design.md` (`#stm` section, version-granularity coarseness, commit
protocol, resolve OQ-TX-3/4 references) and CLAUDE.md if machinery notes warrant.

---

## Resume state (2026-07-03, cont.) — GenTx refactor (A) DONE; mixed-mode + async pending

Post-review rework in progress. Sequence agreed with user:
- **(A) DONE** — replaced `TxContext`/`RootTxInner`/`ChildTxInner` with typed
  `GenTx { Stm(Arc<StmTx>), Atomic(Arc<AtomicTx>) }` + shared `TxCommon`.
  `StmTx::parent: Option<Arc<StmTx>>` makes "#stm under #atomic"
  **unrepresentable** (contagious-atomic enforced by types, not a runtime
  invariant); `GenTx::begin` is the coercing smart constructor. Collapsed
  root/child into `parent: Option`, mode → variant (`discipline()`). Enrollment
  uses `discipline()`; gate armed/disarmed per-level. Renames done:
  `_primBeginSharedTx`→`_primBeginAtomicTx` (kernel+Rust+tests),
  `tx_push_frame`→`tx_push_atomic_frame`. 719 tests pass; clippy clean.
- **(B) IN PROGRESS.** Done so far:
  - **Ctx-aware shadow routing (the load-bearing fix).** Read/write now key on
    `current_tx_ctx` (the tx this dispatch runs under), not `in_tx()`/top-frame.
    `tx_shadow_set`/`tx_shadow_get` → `tx_shadow_set_for(ctx_id,…)` /
    `tx_shadow_get_for(ctx_id,…)`; `assign_var` writes the current tx's frame (or
    live+bump if external/unenrolled); `lookup_var` walks `current_tx_ctx`'s
    ancestry (first shadow hit), never unrelated interleaved frames. Fixes the
    real STM interleaving bug (external writes were landing in the STM shadow →
    no version bump → conflict undetected). Two new integration tests:
    `stm_conflict_from_external_write_is_detected` (0-retry → `TxConflict`) and
    `stm_conflict_retries_and_succeeds` (conflict → retry → 107). Both were
    designed to FAIL pre-fix (verified the first does).
  - Fixed a test-isolation flake: `version_bumps_*` asserted the global
    `STM_ACTIVE` gated-OFF case, which races parallel STM tests; rewritten to
    assert the delta while holding the gate. 721 pass across 3 consecutive runs.
  - **Frame-targeting for nested/interleaved DONE.** All three paths now target
    the frame by ctx, not stack top: durable (`with_frame_tx_for` +
    `with_durable_tx` keys on `current_tx_ctx`), commit
    (`tx_commit_frame(ctx_id, parent)` locates by ctx, reinserts at position on
    pass-up), abort (`tx_abort_frame(ctx_id)`). `_primExitTxMode:` passes the ctx
    from its tag and guards on `has_frame_for`.
  - **Coercion unit test DONE:** `contagious_atomic_coercion_in_gentx_begin`
    directly verifies stm-under-atomic → atomic (and transitively), stm-under-stm
    → stm, atomic-under-stm → atomic.
  - 722 pass, clippy clean.
  - REMAINING in (B): integration tests for nested mixed-mode *behavior*
    (atomic-in-stm / stm-in-atomic committing correctly through the coordinator);
    `[Stm(S),Atomic(A)]` interleave-merge end-to-end. **Flaky deadlock test:**
    passes 8/8 full runs + 5/5 isolated after the C32 work; could not reproduce a
    failure to diagnose. Real latent smell noted: the test sets a process-global
    `deadlockTimeoutMs: 50` that never resets, perturbing parallel tests — a
    candidate root cause, but not confirmed. Left as-is (don't guess-fix);
    recommend a dedicated stress-repro session if it recurs.
- **(C) DONE** — `pvtStmAttempt:` reworked off `await` onto `atomic:`'s
  `then:`/HandlerContext/`OkToCommit` structure, with retry re-entry inside the
  handler. STM is now structurally identical to atomic. 719 tests pass; clippy
  clean.
  - **Finding (pre-existing, NOT C32):** a tx block that *returns a genuinely
    pending future* (deferring the commit) hangs for **both** `atomic:` and
    `stm:` — the coordinator never yields its TxResult. Verified by an
    atomic-equivalent experiment that hung identically. So the non-blocking
    guarantee test (`stm_returns_result_before_block_work_completes`) is
    `#[ignore]`d pending an upstream fix to deferred-future tx blocks; it is not
    an STM regression. Blocks that complete their work internally (return a
    resolved value) work for both, and all such STM tests pass.

**FLAKY TEST FOUND (pre-existing, not caused by C32):**
`atomic_deadlock_is_detected_and_one_victim_aborts` fails ~40–60% of runs on the
**committed baseline** too (measured: baseline 4/10 pass, C32 branch 6/10). It's a
timing-sensitive deadlock test (`deadlockTimeoutMs: 50` + gated futures). Needs a
separate determinism fix; not a C32 regression.

## Resume state (2026-07-03) — S0–S5 DONE (uncommitted), superseded by the above

All open questions resolved. **S0–S5 implemented; 719 interpreter lib tests pass;
fmt + clippy clean; nothing committed.** User will review after S5 before S6/S7.

Done:
- **S0** — `TxFrame` → `enum { Atomic(AtomicFrame), Stm(StmFrame) }` + accessors
  (`ctx_id`, `is_locking`, `shadow_get/set`, `tx_mut`, `absorb_child`,
  `into_parts`, `snapshot_version`, `into_locking`).
- **S1** — `tx_commit_frame` ancestor-aware (searches downward for the genuine
  parent; root commit always applies to live). Atomic-only behavior unchanged.
- **S2** — `EntityData.version: AtomicU64`, static `STM_ACTIVE` gate,
  single-writer bump at the two live-write sites (`assign_var` Entity branch +
  `tx_commit_frame` apply-to-live). Unit test `version_bumps_only_while_stm_active`.
- **S3** — `Stm` frame variant + `TxMode` (on `RootTxInner`, `mode()` accessor) +
  non-locking `tx_push_stm_frame` (snapshots version) + `tx_holder` skips
  non-locking frames + mode-aware enrollment (`has_frame_for` dedup for STM).
- **S4** — `_primBeginStmTx` (arms gate), `_primStmPrepare` (try-lock + validate +
  promote via `stm_try_prepare`/`into_locking`), gate disarm in commit/rollback
  root arms. Phase-2 apply/release reuses `_primExitTxMode:`.
- **S5** — kernel `Transaction stm:` + `pvtRunStm:`/`pvtStmAttempt:` (retry loop,
  default `stmRetryLimit` = 3) + `pvtStmCommit` (2PC via `inject:into:`) +
  `pvtStmRollback` + `TxConflict` (Error subclass) + `Object >> pvtStmPrepare`.
  3 happy-path Smol tests (no-participant, transfer, abort).

**Known gaps / follow-ups before calling C32 complete (S6/S7):**
- **Conflict + retry path is UNTESTED.** Happy path and error-abort are covered;
  a deterministic concurrent-conflict test (force a version bump between snapshot
  and commit → prove retry → eventual success, and retry-exhaustion → `TxConflict`)
  is the key S6 work. The retry machinery is exercised structurally but not under
  a real conflict.
- **STM uses `await` in the coordinator** (`pvtStmAttempt:`), unlike atomic's
  `then:`/HandlerContext. Works for the tested cases; verify no coordinator-self
  deadlock under participant work that chains back (S6).
- **Single-level only.** Nested `stm:` / mixing `stm:` under `atomic:` and the
  full interleaving merge cases from S1/R2 are not yet tested (S6).
- **`_primExitTxMode:` pops the top frame** — fine for single-level STM (one
  frame); revisit for nested/interleaved where the ctx's frame may not be on top.
- Docs (S7): update `tx_design.md` `#stm` section, version-granularity coarseness,
  2PC commit protocol; resolve OQ-TX-3/4 references.

Original plan authored 2026-07-03; C31 fully committed (through S5c).
