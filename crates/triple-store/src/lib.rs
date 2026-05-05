//! Bitemporal triple store for the smol programming environment.
//!
//! See `docs/triple_store_PRD.md` in the repository root for the full design.
//!
//! # Architecture
//!
//! Four traits form the public interface:
//! - [`storage::Storage<TFact>`] — committed-fact persistence primitives
//!   (`write`, `supersede`, `close`, `with_facts`).
//! - [`transaction::Transaction<TFact>`] — cardinality-aware buffered writes
//!   (`assert_single/set/indexed`, `retract_*`, `commit`, `rollback`).
//! - [`queryable::Queryable<TFact>`] — clock-free read interface
//!   (`query_open`, `query_at`, `history`); implementable by storage and stores.
//! - [`triple_store::TripleStore<TFact>`] — top-level entry point;
//!   owns a storage backend, creates transactions, and adds `query_as_of` for
//!   `AsOf`-based bitemporal queries resolved against the store's clock.
//!
//! The in-memory implementation ([`in_memory_store`]) is the only backend in
//! Phase 0. `InMemoryTripleStore` implements `TripleStore<StoredFact>`.

pub mod change;
pub mod fact;
pub mod id;
pub mod in_memory_store;
pub mod intern;
pub mod pattern;
pub mod predicates;
pub mod queryable;
pub mod storage;
pub mod term;
pub mod time;
pub mod transaction;
pub mod triple_store;
