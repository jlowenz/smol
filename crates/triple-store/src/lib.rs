//! Bitemporal triple store for the smol programming environment.
//!
//! See `docs/triple_store_PRD.md` in the repository root for the full design.
//!
//! # Architecture
//!
//! Three traits form the public interface:
//! - [`storage::Storage<TFact>`] — committed-fact persistence primitives
//!   (`write`, `supersede`, `close`, `with_facts`).
//! - [`transaction::Transaction<TFact>`] — cardinality-aware buffered writes
//!   (`assert_single/set/indexed`, `retract_*`, `commit`, `rollback`).
//! - [`triple_store::TripleStore<TFact>`] — top-level entry point;
//!   owns a storage backend and creates transactions.
//!
//! The in-memory implementation ([`in_memory_store`]) is the only backend in
//! Phase 0. `InMemoryTripleStore` implements `TripleStore<StoredFact>`.

pub mod fact;
pub mod id;
pub mod in_memory_store;
pub mod intern;
pub mod pattern;
pub mod predicates;
pub mod storage;
pub mod term;
pub mod time;
pub mod transaction;
pub mod triple_store;
