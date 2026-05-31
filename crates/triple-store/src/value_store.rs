//! Atemporal, content-addressed registry for value object structures.
//!
//! Values are eternal and definitional — they have no subject, no valid-time
//! interval, and no asserter. This module stores the structural records that
//! `Term::ValueRef(hash_id)` references point to, orthogonal to the temporal
//! triple store.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::id::EntityId;
use crate::term::Term;

/// Structural record for a content-addressed value object.
///
/// Both named and indexed fields are stored as `Term` values. Nested value
/// objects appear as `Term::ValueRef(hash_id)`, establishing recursive
/// structure by reference rather than nesting.
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredValue {
    pub class_name: Arc<str>,
    /// Named ivars, ordered by class definition.
    pub named_fields: Vec<(Arc<str>, Term)>,
    /// Position-indexed slots; `indexed_fields[i]` is at position `i`.
    pub indexed_fields: Vec<Term>,
}

/// Content-addressed, atemporal registry for value object structures.
///
/// Embedded in `Store` as an `Arc<ValueStore>` and shared across all
/// `clone_handle()` instances. Not exposed in the public `Store` API;
/// accessed only via `OwnedStoreTransaction::buffer_value` (write path)
/// and `Store::lookup_value` (read path).
pub(crate) struct ValueStore {
    permanent: RwLock<HashMap<EntityId, Arc<StoredValue>>>,
}

impl ValueStore {
    pub(crate) fn new() -> Self {
        ValueStore {
            permanent: RwLock::new(HashMap::new()),
        }
    }

    /// Insert `value` at `id` if not already present. Idempotent.
    ///
    /// Requires: `id.0 & 0x8000_0000_0000_0000 != 0` (content-hash id space).
    /// Ensures: `lookup(id)` returns `Some(_)` after this call.
    pub(crate) fn commit_value(&self, id: EntityId, value: StoredValue) {
        debug_assert!(
            id.0 & 0x8000_0000_0000_0000 != 0,
            "commit_value: id is not in content-hash space"
        );
        let mut map = self.permanent.write();
        map.entry(id).or_insert_with(|| Arc::new(value));
    }

    /// Look up a previously committed value by its content-hash id.
    ///
    /// Requires: `id.0 & 0x8000_0000_0000_0000 != 0`.
    /// Ensures: returns `Some(v)` iff `id` was previously committed via
    ///   `commit_value`; otherwise `None`.
    pub(crate) fn lookup(&self, id: EntityId) -> Option<Arc<StoredValue>> {
        debug_assert!(
            id.0 & 0x8000_0000_0000_0000 != 0,
            "lookup: id is not in content-hash space"
        );
        self.permanent.read().get(&id).cloned()
    }

    /// All currently stored value IDs. Entry point for a future glacier pass.
    pub(crate) fn all_ids(&self) -> Vec<EntityId> {
        self.permanent.read().keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::EntityId;

    const HIGH_BIT: u64 = 0x8000_0000_0000_0000;

    fn hash_id(n: u64) -> EntityId {
        EntityId(n | HIGH_BIT)
    }

    fn simple_value(class: &str) -> StoredValue {
        StoredValue {
            class_name: Arc::from(class),
            named_fields: vec![],
            indexed_fields: vec![],
        }
    }

    #[test]
    fn commit_and_lookup_roundtrip() {
        let vs = ValueStore::new();
        let id = hash_id(1);
        vs.commit_value(id, simple_value("Point"));
        let got = vs.lookup(id).expect("must find committed value");
        assert_eq!(got.class_name.as_ref(), "Point");
    }

    #[test]
    fn lookup_absent_returns_none() {
        let vs = ValueStore::new();
        let id = hash_id(99);
        assert!(vs.lookup(id).is_none());
    }

    #[test]
    fn commit_is_idempotent() {
        let vs = ValueStore::new();
        let id = hash_id(2);
        vs.commit_value(id, simple_value("First"));
        vs.commit_value(id, simple_value("Second")); // must not overwrite
        let got = vs.lookup(id).unwrap();
        assert_eq!(got.class_name.as_ref(), "First");
    }

    #[test]
    fn all_ids_returns_all_committed() {
        let vs = ValueStore::new();
        vs.commit_value(hash_id(10), simple_value("A"));
        vs.commit_value(hash_id(11), simple_value("B"));
        let mut ids = vs.all_ids();
        ids.sort_by_key(|e| e.0);
        assert_eq!(ids, vec![hash_id(10), hash_id(11)]);
    }
}
