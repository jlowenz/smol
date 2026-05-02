//! Bitemporal triple store for the smol programming environment.
//!
//! See `docs/triple_store_PRD.md` in the repository root for the full design.
//! Phase 0 (this crate revision) provides the core data model only:
//! identifiers, terms, intervals, facts, patterns, and string interning.
//! The `FactStore` trait and `Store` API are added in subsequent phases.

pub mod fact;
pub mod fact_store;
pub mod id;
pub mod in_memory_store;
pub mod intern;
pub mod pattern;
pub mod predicates;
pub mod term;
pub mod time;
