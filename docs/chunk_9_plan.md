# Chunk 9: Predicate Registry with Visibility Metadata

## Status

Chunk 8 closed out the change-event surface. The next missing P0 piece is the
**predicate registry** with declared visibility (PRD §2.5, §5.1). The store
currently has only a string interner (`PredicateInterner`); it has no concept
of `Visibility`, no way to declare predicates with metadata, and no
foundation for the access-control layer that PRD §3.5 describes.

Chunk 9 introduces the metadata foundation **without** enforcement. The
visibility values are recorded and queryable, but no code path yet rejects an
operation based on them. Enforcement comes in chunk 10 along with
`ExecContext` and the Store API wrapper.

This split keeps each chunk small and reviewable: data first, behavior after.

---

## What Already Exists

- `crate::intern::PredicateInterner` — string ↔ `u32` handle with no metadata.
- `Predicate(pub u32)` — opaque handle; carries no semantic info.
- `InMemoryTripleStore` — owns clock, tx counter, subscription registry; would
  also own the predicate registry naturally.
- PRD §2.5 specifies the target shape:

  ```rust
  enum Visibility { Private, Shared, Public }
  struct PredicateMeta {
      id: Predicate,
      visibility: Visibility,
  }
  ```

  with the rule: undeclared predicates default to `Visibility::Private`;
  explicit declaration is required to grant `Shared` or `Public`.

---

## Scope for Chunk 9

**In scope:**

- New module `predicate_registry.rs` (or extend `intern.rs` — TBD; see Q1).
- `Visibility` enum: `Private`, `Shared`, `Public`. Derive `Copy + Clone +
  Debug + PartialEq + Eq + Hash`.
- `PredicateMeta { id: Predicate, visibility: Visibility }`. Single struct per
  PRD; field set kept minimal so additions later don't shift API.
- `PredicateRegistry` type (or methods on the existing interner) with:
  - `declare(name: &str, visibility: Visibility) -> Predicate` — interns the
    name if needed; sets visibility. Re-declaration with the *same* visibility
    is idempotent; re-declaration with a *different* visibility is an error
    (see Q3).
  - `lookup(name: &str) -> Option<Predicate>` — pure lookup, no side effects;
    does **not** intern. (Mirrors the existing `PredicateInterner::lookup`.)
  - `meta(id: Predicate) -> PredicateMeta` — total: undeclared but
    interned-by-`assert` predicates return `Visibility::Private`. Never panics
    for any `Predicate` the registry has seen.
  - `intern_default(name: &str) -> Predicate` — interns without setting
    visibility (default Private path). This is what the existing
    `assert_*` paths can call internally if they need to.
- Wire the registry into `InMemoryTripleStore`:
  - Add `predicates: parking_lot::RwLock<PredicateRegistry>` field.
  - Public methods: `declare_predicate(name, visibility) -> Predicate`,
    `predicate(name) -> Option<Predicate>`, `predicate_meta(id) ->
    PredicateMeta`.
  - Existing `Predicate` values constructed in tests via `Predicate(0)`
    continue to work — they default to `Private` per the registry contract.
- Tests: declare/intern idempotency, default visibility, redeclaration
  rejection, lookup-without-declare, registry isolation across store
  instances.

**Deferred:**

- `ExecContext` and access-control enforcement (chunk 10).
- Store API wrapper that re-exposes mutations with `ExecContext` threading
  (chunk 10).
- Predicate metadata beyond `visibility` (cardinality hints, ordering hints).
  PRD §2.5 leaves the struct extensible; we intentionally ship the minimum.
- Cross-store predicate sharing (out of scope; PRD §1.1 explicitly chose
  per-store registries).
- Persistence of the registry (P2, when persistent backend lands).

---

## Design Considerations

### Registry as a separate type vs. extension of `PredicateInterner`

Two options:

**A. Extend `PredicateInterner`** with a parallel `Vec<Visibility>` indexed by
the `u32` handle. Minimal new surface; one type owns name → id and id →
visibility.

**B. New `PredicateRegistry` type** that *owns* a `PredicateInterner` and
adds the metadata vector. Cleaner separation: the interner stays a pure
string-handle utility; the registry is the policy-bearing wrapper.

Option B is the better fit. The interner is currently used for symbols too
(`SymbolInterner`); polluting it with predicate-only concerns muddies the
abstraction. A `PredicateRegistry` that *uses* the interner internally
preserves the interner's general-purpose role.

### Re-declaration semantics

PRD doesn't specify. Three reasonable choices:

1. **Reject conflicting re-declaration.** Strictest; surfaces accidental
   visibility drift early. Same-visibility re-declaration is a no-op.
2. **Last-write-wins.** Convenient for hot-reload-style workflows but hides
   conflicts. Bad fit for a system where visibility is a security boundary.
3. **First-write-wins.** Quietly ignores subsequent declarations. Hides bugs.

**Recommendation: option 1.** Errors are loud and the operation is naturally
rare (one declaration per predicate per store lifetime). Add a
`PredicateError::VisibilityConflict { name, existing, requested }` variant.

### Default for undeclared predicates

PRD §2.5 is explicit: undeclared predicates are `Private`. The registry
honors this by making `meta(id)` total — never returns `None` for a known id.
The internal storage choice (sparse map vs. dense vec with a `Private`
default) is private to the implementation.

### Concurrent access pattern

`InMemoryTripleStore` already uses `parking_lot::RwLock` for the subscription
registry. The predicate registry follows the same pattern: `RwLock<...>`,
read-locked for `lookup` / `meta`, write-locked for `declare`. Both registries
are infrequently written compared to the storage shards.

### Will this break the existing tests?

Most existing tests construct `Predicate(0)` directly. With the new registry,
those facts still commit fine — the registry is consulted for visibility
*metadata*, but no code path in chunk 9 *requires* a registry entry to write a
fact. `predicate_meta(Predicate(0))` will return `Visibility::Private` for
unknown ids (the total-function contract). Zero existing tests should change.

---

## Open Design Questions

1. **`PredicateRegistry` in `predicate_registry.rs` or extend `intern.rs`?**
   Recommendation: new module. Keeps the interner pure and the registry's
   policy concerns visible at the file level.

2. **Should `declare_predicate` return an error or panic on re-declaration
   with conflicting visibility?**
   Recommendation: error (typed `PredicateError`). Visibility is policy;
   silent acceptance or panic both feel wrong.

3. **Do we need a `predicate_name(id) -> Option<&str>` accessor on the
   store?** It's already available via the interner; exposing it through the
   store may simplify future debugging/serialization paths but adds API
   surface. Likely yes — small win, no ongoing cost.

4. **Should `Visibility` be ordered (`Private < Shared < Public`)?**
   Useful for "at least this visible" comparisons. `PartialOrd + Ord` is cheap
   to derive on a 3-variant enum. Recommendation: yes.

---

## Expected Deliverables

- New file `crates/triple-store/src/predicate_registry.rs` (or chosen
  location) defining `Visibility`, `PredicateMeta`, `PredicateRegistry`,
  `PredicateError`.
- `InMemoryTripleStore` gains `predicates: RwLock<PredicateRegistry>` and
  `declare_predicate` / `predicate` / `predicate_meta` inherent methods.
- Tests covering: idempotent re-declaration with same visibility,
  conflicting re-declaration error, default-Private for undeclared,
  declared visibility round-trip, registry isolation across two stores.
- Existing 79 tests continue to pass with no modifications.

---

## What This Sets Up (Chunk 10 Preview)

With `Visibility` recorded, chunk 10 can introduce:

- `ExecContext { caller: EntityId }` (or `SystemContext` variant for
  privileged internal ops).
- A Store API wrapper layer that threads `ExecContext` through every
  mutation and query.
- Access-control rules per PRD §3.5: Private predicates rejected for
  external assertion; Shared predicates readable cross-entity but writable
  only by owner; Public predicates open-world.
- `subscribe`/`unsubscribe` re-exposed on the wrapper with subscriber
  identity captured in `ExecContext`, so dispatch can filter on visibility.

Chunk 9 ships the data model that chunk 10's policy logic reads.
