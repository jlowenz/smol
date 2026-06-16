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
//! Two backends implement `TripleStore<StoredFact>`:
//! - [`in_memory_store::InMemoryTripleStore`] — ephemeral, sharded in-memory store.
//! - [`DurableFileTripleStore`] — append-only WAL-backed store that survives
//!   process restarts.

pub mod change;
pub mod durable_file_store;
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
pub mod store;
pub mod term;
pub mod test_support;
pub mod time;
pub mod transaction;
pub mod triple_store;
pub mod value_store;

pub use durable_file_store::DurableFileTripleStore;
pub use value_store::StoredValue;
