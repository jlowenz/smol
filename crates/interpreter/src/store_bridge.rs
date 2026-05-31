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
use triple_store::term::{Predicate, Term};

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
pub(crate) fn buffer_indexed_slot(
    ev: &Evaluator,
    entity_id: EntityId,
    index: usize,
    value: &Value,
) {
    let pred = ev.registry.indexed_slots_pred;
    let key = OrderingKey::new(index as f64).expect("index as f64 is always finite");
    let mut tx_ref = ev.current_tx.borrow_mut();
    let Some(tx) = tx_ref.as_mut() else { return };
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
}

fn buffer_fact(ev: &Evaluator, entity_id: EntityId, pred: Predicate, value: &Value) {
    let mut tx_ref = ev.current_tx.borrow_mut();
    let Some(tx) = tx_ref.as_mut() else { return };
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
mod tests {
    use super::*;
    use crate::entity::{InstanceHandle, InstanceKind, InstanceState};
    use crate::eval::Evaluator;
    use std::sync::Arc;
    use triple_store::term::Term;

    // ── Primitive round-trips ─────────────────────────────────────────────────

    #[test]
    fn int_round_trip() {
        let v = Value::Int(42);
        if let BridgeResult::Assert(Term::Int(n)) = value_to_bridge(&v) {
            assert_eq!(n, 42);
        } else {
            panic!("expected Assert(Int)");
        }
        assert_eq!(term_to_primitive(&Term::Int(42)), Value::Int(42));
    }

    #[test]
    fn float_round_trip() {
        let v = Value::Float(2.5);
        if let BridgeResult::Assert(Term::Float(f)) = value_to_bridge(&v) {
            assert!((f - 2.5).abs() < f64::EPSILON);
        } else {
            panic!("expected Assert(Float)");
        }
        assert_eq!(term_to_primitive(&Term::Float(2.5)), Value::Float(2.5));
    }

    #[test]
    fn bool_round_trip() {
        let v = Value::Bool(true);
        if let BridgeResult::Assert(Term::Bool(b)) = value_to_bridge(&v) {
            assert!(b);
        } else {
            panic!("expected Assert(Bool)");
        }
        assert_eq!(term_to_primitive(&Term::Bool(true)), Value::Bool(true));
    }

    #[test]
    fn nil_maps_to_retract() {
        assert!(matches!(
            value_to_bridge(&Value::Nil),
            BridgeResult::Retract
        ));
    }

    #[test]
    fn symbol_round_trip() {
        let v = Value::Symbol(Arc::from("foo"));
        if let BridgeResult::Assert(Term::Symbol(s)) = value_to_bridge(&v) {
            assert_eq!(s.as_ref(), "foo");
        } else {
            panic!("expected Assert(Symbol)");
        }
        assert_eq!(
            term_to_primitive(&Term::Symbol(Arc::from("foo"))),
            Value::Symbol(Arc::from("foo"))
        );
    }

    #[test]
    fn str_round_trip() {
        let v = Value::Str(Arc::from("hello"));
        if let BridgeResult::Assert(Term::Str(s)) = value_to_bridge(&v) {
            assert_eq!(s.as_ref(), "hello");
        } else {
            panic!("expected Assert(Str)");
        }
        assert_eq!(
            term_to_primitive(&Term::Str(Arc::from("hello"))),
            Value::Str(Arc::from("hello"))
        );
    }

    #[test]
    fn large_int_round_trip() {
        use num_bigint::BigInt;
        use std::str::FromStr;
        let big = BigInt::from_str("99999999999999999999").unwrap();
        let v = Value::LargeInt(Arc::new(big.clone()));
        if let BridgeResult::Assert(Term::LargeInt(s)) = value_to_bridge(&v) {
            assert_eq!(s.as_ref(), "99999999999999999999");
        } else {
            panic!("expected Assert(LargeInt)");
        }
        assert_eq!(
            term_to_primitive(&Term::LargeInt(Arc::from("99999999999999999999"))),
            Value::LargeInt(Arc::new(big))
        );
    }

    #[test]
    fn block_maps_to_skip() {
        use crate::value::Block;
        let blk = Value::Block(Arc::new(Block {
            params: vec![],
            locals: vec![],
            body: vec![],
            env: crate::env::Env::new(),
            home_depth: 0,
            home_entity: None,
        }));
        assert!(matches!(value_to_bridge(&blk), BridgeResult::Skip));
    }

    #[test]
    fn future_maps_to_skip() {
        let f = Value::Future(crate::entity::FutureHandle::new());
        assert!(matches!(value_to_bridge(&f), BridgeResult::Skip));
    }

    // ── ValueObject content hash stability ────────────────────────────────────

    fn make_value_object(class_name: &str, ivars: &[(&str, Value)]) -> InstanceHandle {
        use crate::class::Class;
        use parking_lot::RwLock;
        use std::collections::HashMap;
        let cls = Arc::new(Class {
            name: Arc::from(class_name),
            superclass: None,
            own_instance_vars: ivars.iter().map(|(n, _)| Arc::from(*n)).collect(),
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: true,
            class_object: std::sync::OnceLock::new(),
        });
        let obj = Arc::new(crate::object::Object::new(cls.clone()));
        for (name, val) in ivars {
            obj.set_slot(Arc::from(*name), val.clone());
        }
        InstanceHandle(Arc::new(InstanceState {
            class: cls,
            kind: InstanceKind::ValueObject(obj),
        }))
    }

    #[test]
    fn value_object_hash_is_stable() {
        let h1 = make_value_object("Point", &[("x", Value::Int(1)), ("y", Value::Int(2))]);
        let h2 = make_value_object("Point", &[("x", Value::Int(1)), ("y", Value::Int(2))]);
        let id1 = content_hash_id(&Value::Instance(h1));
        let id2 = content_hash_id(&Value::Instance(h2));
        assert_eq!(id1, id2, "identical value objects must hash identically");
        assert_ne!(id1.0 & VALUE_OBJECT_HIGH_BIT, 0, "high bit must be set");
    }

    #[test]
    fn value_object_hash_differs_on_different_values() {
        let h1 = make_value_object("Point", &[("x", Value::Int(1)), ("y", Value::Int(2))]);
        let h2 = make_value_object("Point", &[("x", Value::Int(1)), ("y", Value::Int(3))]);
        let id1 = content_hash_id(&Value::Instance(h1));
        let id2 = content_hash_id(&Value::Instance(h2));
        assert_ne!(id1, id2, "different value objects must hash differently");
    }

    #[test]
    fn value_object_bridge_returns_assert_value_ref() {
        let h = make_value_object("Point", &[("x", Value::Int(0)), ("y", Value::Int(0))]);
        let v = Value::Instance(h);
        let expected_id = content_hash_id(&v);
        if let BridgeResult::Assert(Term::ValueRef(id)) = value_to_bridge(&v) {
            assert_eq!(id, expected_id);
        } else {
            panic!("expected Assert(ValueRef)");
        }
    }

    // ── EntityRef resolution ──────────────────────────────────────────────────

    #[test]
    fn resolve_term_primitive_delegates() {
        let ev = Evaluator::new_without_kernel();
        assert_eq!(ev.resolve_term(&Term::Int(7)), Value::Int(7));
        assert_eq!(ev.resolve_term(&Term::Bool(false)), Value::Bool(false));
    }

    #[test]
    fn resolve_term_unknown_entity_ref_returns_int_fallback() {
        let ev = Evaluator::new_without_kernel();
        let unknown_id = EntityId(9999);
        assert_eq!(
            ev.resolve_term(&Term::EntityRef(unknown_id)),
            Value::Int(9999)
        );
    }

    #[test]
    fn resolve_term_unknown_value_ref_returns_int_fallback() {
        let ev = Evaluator::new_without_kernel();
        let hash_id = EntityId(0x8000_0000_0000_0001);
        // Values above i64::MAX saturate to i64::MAX via to_i64().unwrap_or(i64::MAX).
        assert_eq!(
            ev.resolve_term(&Term::ValueRef(hash_id)),
            Value::Int(i64::MAX)
        );
    }
}
