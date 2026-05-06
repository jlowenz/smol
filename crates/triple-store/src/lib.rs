//! Bitemporal triple store for the smol programming environment.
//!
//! See `docs/triple_store_PRD.md` in the repository root for the full design.
//!
//! # Architecture
//!
//! The public interface is built from these components:
//! - [`storage::Storage<TFact>`] — committed-fact persistence primitives
//!   (`write`, `supersede`, `close`, `with_facts`).
//! - [`transaction::Transaction<TFact>`] — concrete struct; cardinality-aware
//!   buffered writes (`assert_single/set/indexed`, `retract_*`, `commit`,
//!   `rollback`). Commits via `Applier<TFact>`, keeping the struct independent
//!   of any concrete store type.
//! - [`queryable::Queryable<TFact>`] — clock-free read interface
//!   (`query_open`, `query_at`, `history`); implemented by storage backends.
//! - [`triple_store::TripleStore<TFact>`] — dyn-compatible top-level entry
//!   point; creates transactions, answers queries, manages subscriptions, and
//!   provides `query_as_of` for `AsOf`-based bitemporal queries resolved
//!   against the store's clock. `Arc<dyn TripleStore<StoredFact>>` is valid.
//!
//! The in-memory implementation ([`in_memory_store`]) is the only backend in
//! Phase 0. `InMemoryTripleStore` implements `TripleStore<StoredFact>`.

pub mod change;
pub mod exec_context;
pub mod fact;
pub mod id;
pub mod in_memory_store;
pub mod intern;
pub mod pattern;
pub mod predicate_registry;
pub mod predicates;
pub mod queryable;
pub mod storage;
pub mod term;
pub mod time;
pub mod transaction;
pub mod triple_store;
