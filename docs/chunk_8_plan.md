# Chunk 8: Change Stream Subscriptions — Final

## Status

**Shipped** in commit `409e9a7`. PRD §3.4 covered for the in-memory backend.

---

## What Shipped

### New module: `crates/triple-store/src/change.rs`

- `ChangeEvent { subject, predicate, kind, tx_id }` — the lightweight projection
  delivered to subscribers. Storage internals (timestamps, index_key, asserter,
  the original commit-tx vs. retract-tx distinction) stay inside the storage
  layer; subscribers that need them can re-query using `tx_id` as the cut.
- `ChangeKind` — `Asserted { new_object }`, `Updated { old_object, new_object }`,
  `Retracted { old_object }`. Field names track the PRD verbatim.
- `Subscription` — opaque public handle. Owns the receiver. Methods:
  - `id() -> SubscriptionId`
  - `recv() -> impl Future<Output = Result<ChangeEvent, ChannelClosed>>`
  - `recv_blocking() -> Result<ChangeEvent, ChannelClosed>`
  - `try_recv() -> Result<Option<ChangeEvent>, ChannelClosed>` (`Ok(None)` =
    empty-but-open, `Err(ChannelClosed)` = store dropped its sender)
- `ChannelClosed` — single error type for the entire subscription surface.
- `Subscriber` — crate-private registry entry holding `id + pattern + Sender`.

### Inherent methods on `InMemoryTripleStore`

- `subscribe(pattern: Pattern) -> Subscription` — store creates the channel
  internally; no `async_channel` type appears in the public signature.
- `unsubscribe(id: SubscriptionId)` — explicit removal; idempotent for unknown ids.
- Private `dispatch(tx_id, &changes)` runs at the tail of `apply()`. Two-phase
  locking: read-lock the registry to send, collect closed subscribers into a
  local `Vec`, then write-lock only if pruning is needed.

### Test coverage (10 new tests, all in `in_memory_store.rs`)

Asserted/Updated/Retracted delivery, subject-pattern filter, unsubscribe stops
delivery (and closes channel), unknown-id no-op, drop-pruning, multiple
subscribers with independent patterns, empty commit doesn't dispatch, multiple
commits carry distinct tx_ids.

---

## Resolved Design Questions

| # | Question | Resolution |
|---|----------|-----------|
| 1 | Subscribe/unsubscribe on trait or inherent? | **Inherent only.** Trait surface deferred until the Store API layer is designed. |
| 2 | `ChangeEvent` lightweight type vs. `Arc<CommitChange>`? | **Lightweight `ChangeEvent`.** Avoids leaking `StoredFact` internals (`index_key`, timestamps) to subscribers; preserves freedom to evolve `CommitChange` independently; keeps the future Store API access-control layer simple. |
| 3 | `Updated` matching: opened, closed, or both? | **Opened (new state) for v0.** Closed-side and dual-side matching deferred until a concrete need appears. |
| 4 | Lazy cleanup of dropped subscriptions? | **Lazy, with caveats documented.** Pruning happens during the next dispatch that would have delivered to the dead entry. v0 subscriber counts make this acceptable. |
| 5 | `SubscriptionId` generation? | **`AtomicU64` counter** mirroring `next_tx`. `Relaxed` ordering is sufficient (uniqueness only). |

### Late-resolved design point: handle shape

Initial draft had `subscribe(pattern, sender) -> SubscriptionId` per the PRD's
literal struct. Revised before commit because:

- It read awkwardly ("I subscribe by handing over a *sender*?"), and
- It exposed `async_channel::Sender` in the public signature, locking us to
  that channel implementation.

The shipped form `subscribe(pattern) -> Subscription` keeps the channel choice
fully internal to `change.rs`. Swapping `async-channel` for another mpmc
implementation (e.g., for the future custom-VM scheduler) is now a single-file
edit confined to `change.rs` and the `dispatch` path.

---

## Deferred (carried into later chunks)

- `subscribe`/`unsubscribe` on the `TripleStore` trait — needs the Store API
  layer for context.
- Backpressure / bounded channels / drop policy (PRD §5.3).
- Wildcard-subject subscriptions — same blocker as wildcard queries from
  chunk 7 (needs cross-shard `for_all_facts` iterator).
- Auto-unsubscribe on `Subscription::drop` — would require a back-reference to
  the store (Arc or lifetime). Lazy cleanup covers correctness; the explicit
  `unsubscribe` is the documented immediate-cleanup path.

---

## Files Touched

- `crates/triple-store/Cargo.toml` — added `async-channel` workspace dep
- `crates/triple-store/src/change.rs` — **new**
- `crates/triple-store/src/lib.rs` — register `change` module
- `crates/triple-store/src/in_memory_store.rs` — registry, subscribe/unsubscribe,
  dispatch, pattern-matching projection, 10 tests
- `Cargo.lock` — async-channel transitive deps

Test count: 79 passing (was 69). No new clippy warnings.
