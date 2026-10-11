//! Bridge between Smol `Value` and triple-store `Term`.
//!
//! Ivar assignment drives the bridge: converting a `Value` to a `BridgeResult`
//! determines whether the store should assert a new fact, retract an existing
//! fact (nil), or skip (non-storable values).

use std::sync::Arc;

use num_traits::ToPrimitive;

use crate::entity::{EntityId, InstanceKind};
use crate::eval::Evaluator;
use crate::value::Value;
use triple_store::fact::Fact;
use triple_store::predicates::OrderingKey;
use triple_store::term::{Predicate, SymbolId, Term};
use triple_store::value_store::StoredValue;

// ── BridgeResult ──────────────────────────────────────────────────────────────

/// Result of mapping a `Value` to a store operation.
pub enum BridgeResult {
    /// Assert a new fact with the given term as the object.
    Assert(Term),
    /// Nil value: retract the open fact at this predicate (close the interval).
    Retract,
    /// Value is non-storable (Block, Future, Namespace): skip silently.
    Skip,
}

// ── value_to_bridge ───────────────────────────────────────────────────────────

/// Map a Smol `Value` to a `BridgeResult` for store assertion.
///
/// Ensures: `Nil` → `Retract`; non-storable variants → `Skip`;
///   every storable primitive → `Assert(Term)`.
pub fn value_to_bridge(v: &Value) -> BridgeResult {
    match v {
        Value::Int(n) => BridgeResult::Assert(Term::Int(*n)),
        Value::Float(f) => BridgeResult::Assert(Term::Float(*f)),
        Value::Bool(b) => BridgeResult::Assert(Term::Bool(*b)),
        Value::Nil => BridgeResult::Retract,
        Value::Symbol(s) => BridgeResult::Assert(Term::Symbol(s.clone())),
        Value::Str(s) => BridgeResult::Assert(Term::Str(s.clone())),
        Value::LargeInt(n) => {
            BridgeResult::Assert(Term::LargeInt(Arc::from(n.to_string().as_str())))
        }
        Value::Instance(h) => match &h.0.kind {
            InstanceKind::ValueObject(_) => {
                let hash_id = content_hash_id(v);
                BridgeResult::Assert(Term::ValueRef(hash_id))
            }
            InstanceKind::Entity(d) => BridgeResult::Assert(Term::EntityRef(d.id)),
            InstanceKind::ClassEntity(d) => BridgeResult::Assert(Term::EntityRef(d.id)),
        },
        // Deferred: Block requires AST serialisation.
        Value::Block(_) => BridgeResult::Skip,
        // Deferred: Future is transient, no meaningful persistent representation.
        Value::Future(_) => BridgeResult::Skip,
        // Deferred: Namespace requires design for notebook/artifact storage.
        Value::Namespace(_) => BridgeResult::Skip,
        // Handle is a transient runtime value; no persistent representation.
        Value::Handle(_) => BridgeResult::Skip,
    }
}

// ── content_hash_id ───────────────────────────────────────────────────────────

const FNV_OFFSET: u64 = 14695981039346656037;
const FNV_PRIME: u64 = 1099511628211;
const VALUE_OBJECT_HIGH_BIT: u64 = 0x8000_0000_0000_0000;

fn fnv_update(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash ^= *b as u64;
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn hash_value(v: &Value, hash: &mut u64) {
    match v {
        Value::Int(n) => {
            fnv_update(hash, &[0]);
            fnv_update(hash, &n.to_le_bytes());
        }
        Value::Float(f) => {
            fnv_update(hash, &[1]);
            fnv_update(hash, &f.to_bits().to_le_bytes());
        }
        Value::Bool(b) => {
            fnv_update(hash, &[2, *b as u8]);
        }
        Value::Nil => {
            fnv_update(hash, &[3]);
        }
        Value::Symbol(s) => {
            fnv_update(hash, &[4]);
            fnv_update(hash, &(s.len() as u64).to_le_bytes());
            fnv_update(hash, s.as_bytes());
        }
        Value::Str(s) => {
            fnv_update(hash, &[5]);
            fnv_update(hash, &(s.len() as u64).to_le_bytes());
            fnv_update(hash, s.as_bytes());
        }
        Value::LargeInt(n) => {
            fnv_update(hash, &[6]);
            let s = n.to_string();
            fnv_update(hash, s.as_bytes());
        }
        Value::Instance(h) => match &h.0.kind {
            InstanceKind::ValueObject(_) => {
                fnv_update(hash, &[7]);
                let class_name = &h.0.class.name;
                fnv_update(hash, &(class_name.len() as u64).to_le_bytes());
                fnv_update(hash, class_name.as_bytes());
                let obj = h.ivar_storage();
                let slots = obj.slots.lock();
                for ivar_name in h.0.class.all_instance_vars() {
                    fnv_update(hash, &(ivar_name.len() as u64).to_le_bytes());
                    fnv_update(hash, ivar_name.as_bytes());
                    let val = slots.get(ivar_name.as_ref()).cloned().unwrap_or(Value::Nil);
                    hash_value(&val, hash);
                }
            }
            InstanceKind::Entity(d) => {
                fnv_update(hash, &[8]);
                fnv_update(hash, &d.id.0.to_le_bytes());
            }
            InstanceKind::ClassEntity(d) => {
                fnv_update(hash, &[9]);
                fnv_update(hash, &d.id.0.to_le_bytes());
            }
        },
        Value::Block(_) => fnv_update(hash, &[10]),
        Value::Future(_) => fnv_update(hash, &[11]),
        Value::Namespace(_) => fnv_update(hash, &[12]),
        Value::Handle(_) => fnv_update(hash, &[13]),
    }
}

/// Compute a stable content-hash `EntityId` for a value object.
///
/// Requires: `v` is `Value::Instance(h)` where `h.0.kind` is `ValueObject`.
/// Ensures: the returned ID has the high bit set (distinguishing it from
///   live-entity IDs allocated by `next_entity_id`). Two calls with
///   structurally identical value objects return the same ID.
pub fn content_hash_id(v: &Value) -> EntityId {
    debug_assert!(
        matches!(v, Value::Instance(h) if matches!(h.0.kind, InstanceKind::ValueObject(_))),
        "content_hash_id called on non-ValueObject"
    );
    let mut hash = FNV_OFFSET;
    hash_value(v, &mut hash);
    EntityId(hash | VALUE_OBJECT_HIGH_BIT)
}

// ── collect_value_subtree ─────────────────────────────────────────────────────

/// Collect the full value sub-tree rooted at `v` into `buf`, bottom-up.
///
/// Requires: `v` is a `Value::Instance` with `InstanceKind::ValueObject`.
/// Ensures: `buf` contains one entry per distinct value id in the sub-tree,
///   innermost values first. Duplicate ids (shared nested values) produce one
///   entry. The returned `EntityId` is the root's hash id.
pub(crate) fn collect_value_subtree(v: &Value, buf: &mut Vec<(EntityId, StoredValue)>) -> EntityId {
    let Value::Instance(h) = v else {
        panic!("collect_value_subtree: not a Value::Instance");
    };
    let InstanceKind::ValueObject(obj) = &h.0.kind else {
        panic!("collect_value_subtree: not a ValueObject");
    };

    let class_name: Arc<str> = h.0.class.name.clone();

    let mut named_fields: Vec<(Arc<str>, Term)> = Vec::new();
    {
        let slots = obj.slots.lock();
        for ivar_name in h.0.class.all_instance_vars() {
            let val = slots.get(ivar_name.as_ref()).cloned().unwrap_or(Value::Nil);
            if let Value::Instance(inner) = &val
                && matches!(inner.0.kind, InstanceKind::ValueObject(_))
            {
                let nested_id = collect_value_subtree(&val, buf);
                named_fields.push((ivar_name, Term::ValueRef(nested_id)));
                continue;
            }
            match value_to_bridge(&val) {
                BridgeResult::Assert(term) => named_fields.push((ivar_name, term)),
                BridgeResult::Retract | BridgeResult::Skip => {}
            }
        }
    }

    let mut indexed_fields: Vec<Term> = Vec::new();
    if let Some(indexed) = &obj.indexed {
        use crate::object::IndexedStorage;
        match indexed {
            IndexedStorage::Pointer(slots) => {
                for val in slots.lock().iter() {
                    if let Value::Instance(inner) = val
                        && matches!(inner.0.kind, InstanceKind::ValueObject(_))
                    {
                        let nested_id = collect_value_subtree(val, buf);
                        indexed_fields.push(Term::ValueRef(nested_id));
                        continue;
                    }
                    match value_to_bridge(val) {
                        BridgeResult::Assert(term) => indexed_fields.push(term),
                        // nil in an indexed slot: store InternedSymbol(0) which round-trips to Nil.
                        BridgeResult::Retract => {
                            indexed_fields.push(Term::InternedSymbol(SymbolId(0)))
                        }
                        BridgeResult::Skip => {
                            indexed_fields.push(Term::InternedSymbol(SymbolId(0)))
                        }
                    }
                }
            }
            IndexedStorage::Byte(slots) => {
                for &b in slots.lock().iter() {
                    indexed_fields.push(Term::Int(b as i64));
                }
            }
            IndexedStorage::Float64(slots) => {
                for &f in slots.lock().iter() {
                    indexed_fields.push(Term::Float(f));
                }
            }
        }
    }

    let root_id = content_hash_id(v);

    if !buf.iter().any(|(id, _)| *id == root_id) {
        buf.push((
            root_id,
            StoredValue {
                class_name,
                named_fields,
                indexed_fields,
            },
        ));
    }

    root_id
}

// ── Durable-buffer routing ─────────────────────────────────────────────────────

/// Run `f` with the durable store buffer that should receive `entity_id`'s ops
/// right now, and return its result (or `None` if no buffer is active).
///
/// Routing — the single place that distinguishes shared vs. private transactions:
///   - **Inside a shared transaction** (`ev.current_tx_ctx` set and the entity is
///     enrolled): the entity's **top frame buffer**, so the op commits/rolls back
///     with that transaction level.
///   - **Otherwise**: the evaluator's private per-message `current_tx`.
///
/// Ensures: `None` (no-op) when neither buffer is active (e.g. outside any
///   handler). The shared-tx lookup is gated behind a cheap `current_tx_ctx`
///   check, so the common non-transactional path is unchanged.
fn with_durable_tx<R>(
    ev: &Evaluator,
    entity_id: EntityId,
    f: impl FnOnce(&mut triple_store::store::OwnedStoreTransaction) -> R,
) -> Option<R> {
    // Ctx-aware (C32): route durable ops to the frame of the tx this dispatch runs
    // under, not the stack top (which under interleaving may be another tx's).
    let cur_ctx_id = ev.current_tx_ctx.borrow().as_ref().map(|c| c.ctx_id());
    if let Some(cid) = cur_ctx_id {
        let state = ev
            .registry
            .instance_entities
            .read()
            .get(&entity_id)
            .cloned()
            .or_else(|| ev.registry.class_entities.read().get(&entity_id).cloned());
        if let Some(state) = state {
            let data = match &state.kind {
                InstanceKind::Entity(d) | InstanceKind::ClassEntity(d) => Some(d.as_ref()),
                InstanceKind::ValueObject(_) => None,
            };
            if let Some(d) = data
                && d.has_frame_for(cid)
            {
                return Some(d.with_frame_tx_for(cid, f));
            }
        }
    }
    let mut tx_ref = ev.current_tx.borrow_mut();
    tx_ref.as_mut().map(f)
}

// ── Store buffer helpers ──────────────────────────────────────────────────────

/// Buffer a named-ivar fact into the entity's current transaction.
///
/// Requires: `entity_id` is the ID of the entity that owns this ivar.
/// Ensures: if `current_tx` is `None` (outside a handler), this is a no-op.
///   If the predicate is not registered (e.g. class was never installed), no-op.
///   Nil values produce a retract; non-storable values (Block, Future, Namespace) are skipped.
pub(crate) fn buffer_ivar(
    ev: &Evaluator,
    entity_id: EntityId,
    ivar_name: &Arc<str>,
    value: &Value,
) {
    let Some(pred) = ev.registry.store.lookup(ivar_name) else {
        return;
    };
    buffer_fact(ev, entity_id, pred, value);
}

/// Buffer an indexed-slot fact into the entity's current transaction.
///
/// Requires: `entity_id` is the ID of the entity that owns this indexed slot.
///   `value` is the value actually stored (caller converts Byte/Float64 before calling).
/// Ensures: if `current_tx` is `None` (outside a handler), this is a no-op.
///   Nil values produce a retract; non-storable values are skipped.
///   For value objects, the structural record is staged via `buffer_value` before
///   the `Term::ValueRef` fact is buffered.
pub(crate) fn buffer_indexed_slot(
    ev: &Evaluator,
    entity_id: EntityId,
    index: usize,
    value: &Value,
) {
    let pred = ev.registry.indexed_slots_pred;
    let key = OrderingKey::new(index as f64).expect("index as f64 is always finite");
    with_durable_tx(ev, entity_id, |tx| {
        // For value objects, collect the full subtree and stage structural records.
        if let Value::Instance(h) = value
            && matches!(h.0.kind, InstanceKind::ValueObject(_))
        {
            let mut val_buf = Vec::new();
            let hash_id = collect_value_subtree(value, &mut val_buf);
            for (id, sv) in val_buf {
                tx.buffer_value(id, sv);
            }
            let _ = tx.assert_indexed(
                Fact {
                    subject: entity_id,
                    predicate: pred,
                    object: Term::ValueRef(hash_id),
                    asserter: entity_id,
                    valid: None,
                },
                key,
            );
            return;
        }
        match value_to_bridge(value) {
            BridgeResult::Assert(term) => {
                let _ = tx.assert_indexed(
                    Fact {
                        subject: entity_id,
                        predicate: pred,
                        object: term,
                        asserter: entity_id,
                        valid: None,
                    },
                    key,
                );
            }
            BridgeResult::Retract => {
                let _ = tx.retract_indexed(entity_id, pred, entity_id, key);
            }
            BridgeResult::Skip => {}
        }
    });
}

fn buffer_fact(ev: &Evaluator, entity_id: EntityId, pred: Predicate, value: &Value) {
    with_durable_tx(ev, entity_id, |tx| {
        // For value objects, collect the full subtree and stage structural records.
        if let Value::Instance(h) = value
            && matches!(h.0.kind, InstanceKind::ValueObject(_))
        {
            let mut val_buf = Vec::new();
            let hash_id = collect_value_subtree(value, &mut val_buf);
            for (id, sv) in val_buf {
                tx.buffer_value(id, sv);
            }
            let _ = tx.assert_single(Fact {
                subject: entity_id,
                predicate: pred,
                object: Term::ValueRef(hash_id),
                asserter: entity_id,
                valid: None,
            });
            return;
        }
        match value_to_bridge(value) {
            BridgeResult::Assert(term) => {
                let _ = tx.assert_single(Fact {
                    subject: entity_id,
                    predicate: pred,
                    object: term,
                    asserter: entity_id,
                    valid: None,
                });
            }
            BridgeResult::Retract => {
                let _ = tx.retract_single(entity_id, pred, entity_id);
            }
            BridgeResult::Skip => {}
        }
    });
}

// ── term_to_primitive ─────────────────────────────────────────────────────────

/// Map a non-`EntityRef` `Term` to a primitive `Value`.
///
/// Requires: `t` is not `Term::EntityRef`. For EntityRef resolution (which
///   requires access to the live entity registry) use `Evaluator::resolve_term`.
/// Ensures: `Term::Timestamp` is mapped to `Value::Int(epoch_millis as i64)`.
pub fn term_to_primitive(t: &Term) -> Value {
    match t {
        Term::Int(n) => Value::Int(*n),
        Term::Float(f) => Value::Float(*f),
        Term::Bool(b) => Value::Bool(*b),
        Term::Symbol(s) => Value::Symbol(s.clone()),
        Term::LargeInt(s) => {
            use num_bigint::BigInt;
            use std::str::FromStr;
            match BigInt::from_str(s) {
                Ok(n) => Value::LargeInt(Arc::new(n)),
                Err(_) => Value::Str(s.clone()),
            }
        }
        Term::Str(s) => Value::Str(s.clone()),
        Term::Bytes(b) => Value::Str(Arc::from(String::from_utf8_lossy(b).as_ref())),
        Term::Timestamp(ts) => Value::Int(ts.0.min(i64::MAX as u64) as i64),
        Term::InternedSymbol(_) => Value::Nil,
        Term::EntityRef(_) | Term::ValueRef(_) => {
            panic!("term_to_primitive called on EntityRef/ValueRef — use Evaluator::resolve_term")
        }
    }
}

// ── Evaluator::resolve_term ───────────────────────────────────────────────────

impl Evaluator {
    /// Resolve a `Term` to a `Value`, looking up entity handles for `EntityRef` terms.
    ///
    /// For `EntityRef(id)`:
    ///   - High bit set → look up in `value_objects` registry.
    ///   - Otherwise → look up in `instance_entities`, then `registry.class_entities`.
    ///   - Fallback → `Value::Int(id.0 as i64)` with a debug warning.
    ///
    /// For all other terms → delegates to `term_to_primitive`.
    pub fn resolve_term(&self, t: &Term) -> Value {
        match t {
            Term::EntityRef(id) => {
                if let Some(state) = self.registry.instance_entities.read().get(id) {
                    return Value::Instance(crate::entity::InstanceHandle(state.clone()));
                }
                if let Some(state) = self.registry.class_entities.read().get(id) {
                    return Value::Instance(crate::entity::InstanceHandle(state.clone()));
                }
                eprintln!(
                    "[smol] resolve_term: EntityRef({}) not found in live registry — \
                     entity may have terminated or be from a prior session",
                    id.0
                );
                Value::Int(id.0.to_i64().unwrap_or(i64::MAX))
            }
            Term::ValueRef(id) => {
                if let Some(state) = self.value_objects.borrow().get(id) {
                    return Value::Instance(crate::entity::InstanceHandle(state.clone()));
                }
                eprintln!(
                    "[smol] resolve_term: ValueRef({}) not found in value_objects registry — \
                     value object structural storage is deferred (C27.5)",
                    id.0
                );
                Value::Int(id.0.to_i64().unwrap_or(i64::MAX))
            }
            other => term_to_primitive(other),
        }
    }
}

#[cfg(test)]
#[path = "tests/store_bridge.rs"]
mod tests;
