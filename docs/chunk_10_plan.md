# Chunk 10: ExecContext, AccessError, and the Store Wrapper

## Status

Chunk 9 shipped `PredicateRegistry` with `Visibility` and `Cardinality`. Chunk
10 introduces the **access-control enforcement layer** that reads that metadata
(PRD §3.5, §4). The data model is in place; this chunk adds the policy.

---

## What Already Exists

- `PredicateRegistry` / `Visibility` / `Cardinality` / `PredicateMeta` — chunk 9.
- `InMemoryTripleStore` with `declare_predicate`, `predicate_meta`,
  `predicate_name` — chunk 9.
- Full transaction/commit pipeline (`new_transaction` → `assert_*`/`retract_*`
  → `commit`) — chunks 1–8.
- `Fact.subject == Fact.asserter` is the existing internal/external
  distinction — already modelled in `fact.rs`.

---

## Scope for Chunk 10

**In scope:**

- New module `exec_context.rs`:
  - `ExecContext` enum: `Entity(EntityId)` | `System`.
  - `AccessError` enum: `Denied { predicate: Predicate, reason: &'static str }`.
  - Three methods on `ExecContext` encoding the PRD §3.5 rules.
- New module `store.rs`:
  - `StoreError` enum wrapping `AccessError` and `TxError`, with `From` impls.
  - `Store` newtype wrapper over `InMemoryTripleStore`.
  - `Store` mutation methods — `assert_single/set/indexed`, `retract_single/set/indexed`
    — each runs the appropriate access check then delegates a one-op transaction.
  - `Store` query methods — `query_open`, `query_at`, `history` — each checks
    visibility and returns an empty result (silently) for denied predicates.
  - Predicate-registry delegation: `declare_predicate`, `predicate`,
    `predicate_meta`, `predicate_name`.
  - Subscription delegation: `subscribe`, `unsubscribe`.

**Deferred:**

- Multi-operation transaction API (begin / TxId-indexed assert / commit).
  The `Store` uses one-op transactions internally for now.
- `Store::clone_handle()` / `Arc<InMemoryTripleStore>` wrapping (chunk 11+).
- Subscription filtering by subscriber identity (chunk 11+).
- `entities_with` query (chunk 11+).

---

## Design

### `ExecContext` and `AccessError` — new file `exec_context.rs`

`ExecContext` is a lightweight value type that identifies *who* is performing
an operation. It has two variants:

```rust
pub enum ExecContext {
    Entity(EntityId),   // a specific entity acting under its own identity
    System,             // privileged bypass for store-level tests and bootstrap
}
```

`AccessError` is the rejection type returned when an operation is not permitted:

```rust
pub enum AccessError {
    Denied { predicate: Predicate, reason: &'static str },
}
```

`reason` is `&'static str` — no allocation — consistent with how
`StorageError::Internal` and `TxError::Internal` work in the rest of the codebase.

The three access rules from PRD §3.5 are implemented as methods on `ExecContext`:

```
check_internal_assert(&self, subject: EntityId) -> Result<(), AccessError>
check_external_assert(&self, meta: PredicateMeta)  -> Result<(), AccessError>
can_read(&self, subject: EntityId, meta: PredicateMeta) -> bool
```

See **Exercise A** below for the precise rule logic.

### `StoreError` — lives in `store.rs`

`StoreError` is the error type returned by all `Store` mutation methods. It
combines access failures with transaction failures:

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    Access(AccessError),
    Transaction(TxError),
}
```

Add `From<AccessError> for StoreError` and `From<TxError> for StoreError` so
the `?` operator can be used to propagate either type inside `Store` methods.
This is the standard Rust multi-error-source pattern.

### `Store` wrapper — lives in `store.rs`

`Store` is a newtype wrapper over `InMemoryTripleStore`:

```rust
pub struct Store(InMemoryTripleStore);
```

It is **not** `Clone` in this chunk (shared-handle semantics deferred). It
exposes its own API; `InMemoryTripleStore`'s public API is not re-exported
directly — callers go through `Store`.

**Mutation methods.** Each assert/retract method:
1. Determines whether the operation is internal or external (compare
   `fact.subject` with `fact.asserter` for asserts; retract is always by the
   owning context, so compare `ctx` with `subject`).
2. Calls the appropriate check method on `ctx`.
3. Creates a one-op transaction, runs the operation, commits.
4. Propagates errors as `StoreError`.

**Query methods.** Each query method:
1. Calls `self.0.predicate_meta(predicate)` to retrieve visibility.
2. Calls `ctx.can_read(subject, meta)`. If `false`, returns an empty `Vec`
   immediately (PRD §3.5: "as if it does not exist").
3. Otherwise delegates to the corresponding `InMemoryTripleStore` query method.

The silent-empty-result behaviour for denied reads is intentional: it prevents
information leakage by not revealing whether a predicate exists.

---

## PRD §3.5 Rules Reference

The three rules, stated precisely for implementation:

### Internal assert

> An `assert` of an **Internal** fact for entity E is only accepted from
> entity E's context. `System` is always permitted.

```
Entity(caller) → permit iff caller == subject
System         → always permit
```

### External assert

> An `assert` of an **External** fact requires the predicate's visibility to
> be `Public`. `Private` and `Shared` predicates reject external assertions.

```
visibility == Public  → permit (any context, including System)
otherwise             → deny, regardless of ctx
```

Note: `System` context is still denied for external asserts on non-Public
predicates. `System` grants bypass for *ownership* checks (internal asserts),
not visibility checks. This is a deliberate boundary.

### Read

> `Private`: only E's own context can read.
> `Shared`: any entity's context can read.
> `Public`: any entity can read.

```
Private  → permit iff ctx == Entity(subject) || ctx == System
Shared   → always permit
Public   → always permit
```

---

## Exercise Tasks

These two tasks are scoped to be implementable from the description above and
the existing codebase, without needing additional guidance from Claude.

### Exercise A — `ExecContext` and `AccessError`

**File:** `crates/triple-store/src/exec_context.rs` (new file)

**What to implement:**

1. The `ExecContext` enum and `AccessError` enum as shown above. Follow the
   derive pattern of the rest of the codebase: `Copy + Clone + Debug + PartialEq + Eq`
   where appropriate. `ExecContext` should be `Copy`; `AccessError` should be
   `Debug + PartialEq + Eq` (not `Copy` — the `reason: &'static str` field is
   `Copy` but the overall error type conventionally isn't).

2. An `impl ExecContext` block with three methods:
   ```
   pub fn check_internal_assert(&self, subject: EntityId) -> Result<(), AccessError>
   pub fn check_external_assert(&self, meta: PredicateMeta) -> Result<(), AccessError>
   pub fn can_read(&self, subject: EntityId, meta: PredicateMeta) -> bool
   ```
   Translate the three rules from the **PRD §3.5 Rules Reference** section
   above using `match` on `self`. For `Denied`, pick a short `reason` string
   describing the violation (e.g. `"entity may not assert internal facts for
   another entity"`).

3. A `#[cfg(test)]` module with at least these cases:
   - `System` permits internal assert for any subject.
   - `Entity(1)` permits internal assert for subject 1.
   - `Entity(1)` denies internal assert for subject 2.
   - `Entity(x)` denies external assert on `Private` predicate.
   - `Entity(x)` denies external assert on `Shared` predicate.
   - `System` denies external assert on `Private` predicate (see note above).
   - Any context permits external assert on `Public` predicate.
   - `Private` predicate: only owner and `System` can read.
   - `Shared` predicate: any context can read.

**Rust hints:**

- `match self { ExecContext::Entity(id) => ..., ExecContext::System => ... }`
  is the idiomatic way to branch on enum variants with data.
- For `check_internal_assert`, `Visibility` is not involved — only
  `ExecContext` variant vs. `subject`.
- For `can_read`, return `true` early for `Shared` and `Public` since both are
  unconditionally permitted; handle `Private` as the restrictive case.
- `Predicate(0)` is fine as a placeholder predicate id in tests (it doesn't
  need to be registered for these unit tests).

---

### Exercise B — `StoreError` with `From` impls

**File:** `crates/triple-store/src/store.rs` (new file, just the error type for now)

**What to implement:**

1. The `StoreError` enum:
   ```rust
   pub enum StoreError {
       Access(AccessError),
       Transaction(TxError),
   }
   ```

2. Two `From` impls:
   ```rust
   impl From<AccessError> for StoreError { ... }
   impl From<TxError> for StoreError { ... }
   ```

3. Verify that `?` can propagate both types. A quick way to check: write a
   dead-code function (not a test, just a local fn) that calls a fallible
   function returning `Result<_, AccessError>` and uses `?` inside a function
   returning `Result<_, StoreError>`. It will fail to compile if the `From`
   impl is missing.

**Rust hints:**

- `From<A> for B` is the trait for "A can be infallibly converted to B".
  `impl From<AccessError> for StoreError { fn from(e: AccessError) -> Self { Self::Access(e) } }`.
- The `?` operator on a `Result<T, E>` inside a function returning
  `Result<T, F>` automatically calls `F::from(e)` on the error. This is why
  `From` impls are the idiomatic way to compose error types in Rust.
- `#[derive(Debug, PartialEq, Eq)]` on `StoreError` requires that both wrapped
  types also implement those traits. Check that `AccessError` and `TxError`
  already do — they should from the existing codebase.

---

## Remaining Deliverables (not exercises)

These should be implemented (or reviewed after the exercises are done):

- `Store` struct (newtype over `InMemoryTripleStore`) in `store.rs`.
- All six mutation methods on `Store` with access checks.
- Three query methods on `Store` with visibility filtering.
- Predicate-registry and subscription delegation on `Store`.
- `lib.rs` updated to declare both new modules.
- Tests covering: internal assert permitted/denied, external assert
  permitted/denied, query visibility filtering, System bypass for internal
  asserts, retract-not-found propagated as `StoreError::Transaction`.

---

## Expected Deliverables

- `crates/triple-store/src/exec_context.rs` — `ExecContext`, `AccessError`,
  three check methods, unit tests.
- `crates/triple-store/src/store.rs` — `StoreError`, `Store`, delegation and
  access-check wiring, integration tests.
- `lib.rs` updated to declare both new modules.
- All 103 existing tests continue to pass with no modifications.

---

## What This Sets Up (Chunk 11 Preview)

With `Store` in place:

- Multi-operation transactions can be exposed on `Store` (begin / multi-assert / commit).
- `Store::clone_handle()` can be added once `Arc<InMemoryTripleStore>` is
  introduced, enabling multiple entity contexts to share one backend.
- Subscriber identity can be captured at `subscribe` time (from `ExecContext`)
  and used to filter dispatch by visibility.
