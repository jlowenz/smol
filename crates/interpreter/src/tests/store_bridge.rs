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
        snapshot: Arc::from(Vec::new()),
        env: crate::env::Env::new(),
        home_depth: 0,
        home_entity: None,
        home_binding: None,
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

    let cls = Arc::new(Class::new(
        Arc::from(class_name),
        None,
        ivars.iter().map(|(n, _)| Arc::from(*n)).collect(),
        vec![],
        None,
        true,
    ));
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

// ── collect_value_subtree ─────────────────────────────────────────────────

#[test]
fn collect_flat_value_primitive_ivars() {
    let h = make_value_object("Point", &[("x", Value::Int(3)), ("y", Value::Int(4))]);
    let v = Value::Instance(h);
    let mut buf = Vec::new();
    let id = collect_value_subtree(&v, &mut buf);
    assert_eq!(buf.len(), 1, "one entry for the flat value");
    let (bid, sv) = &buf[0];
    assert_eq!(*bid, id);
    assert_eq!(sv.class_name.as_ref(), "Point");
    assert_eq!(sv.named_fields.len(), 2);
    assert!(
        sv.named_fields
            .iter()
            .any(|(n, t)| n.as_ref() == "x" && *t == Term::Int(3))
    );
    assert!(
        sv.named_fields
            .iter()
            .any(|(n, t)| n.as_ref() == "y" && *t == Term::Int(4))
    );
    assert!(sv.indexed_fields.is_empty());
}

#[test]
fn collect_nested_value_bottom_up() {
    // Inner: Money(amount: 100)
    let inner = make_value_object("Money", &[("amount", Value::Int(100))]);
    let inner_val = Value::Instance(inner);
    let inner_id = content_hash_id(&inner_val);

    // Outer: LineItem(price: <Money>)
    let outer_cls = {
        use crate::class::Class;

        Arc::new(Class::new(
            Arc::from("LineItem"),
            None,
            vec![Arc::from("price")],
            vec![],
            None,
            true,
        ))
    };
    let outer_obj = Arc::new(crate::object::Object::new(outer_cls.clone()));
    outer_obj.set_slot(Arc::from("price"), inner_val.clone());
    let outer_state = Arc::new(InstanceState {
        class: outer_cls,
        kind: InstanceKind::ValueObject(outer_obj),
    });
    let outer_val = Value::Instance(InstanceHandle(outer_state));
    let outer_id = content_hash_id(&outer_val);

    let mut buf = Vec::new();
    let root_id = collect_value_subtree(&outer_val, &mut buf);

    assert_eq!(root_id, outer_id);
    assert_eq!(buf.len(), 2, "inner + outer = 2 entries");
    // inner is first (bottom-up)
    assert_eq!(buf[0].0, inner_id, "inner must come first");
    assert_eq!(buf[1].0, outer_id, "outer must come second");
    // outer's price field is a ValueRef to inner
    let outer_sv = &buf[1].1;
    assert!(
        outer_sv
            .named_fields
            .iter()
            .any(|(n, t)| { n.as_ref() == "price" && *t == Term::ValueRef(inner_id) }),
        "outer must reference inner via ValueRef"
    );
}

#[test]
fn collect_shared_nested_value_deduplicates() {
    // point appears twice as fields of Container
    let point = make_value_object("Point", &[("x", Value::Int(1)), ("y", Value::Int(1))]);
    let point_val = Value::Instance(point);
    let point_id = content_hash_id(&point_val);

    use crate::class::Class;

    let cls = Arc::new(Class::new(
        Arc::from("Container"),
        None,
        vec![Arc::from("a"), Arc::from("b")],
        vec![],
        None,
        true,
    ));
    let obj = Arc::new(crate::object::Object::new(cls.clone()));
    obj.set_slot(Arc::from("a"), point_val.clone());
    obj.set_slot(Arc::from("b"), point_val.clone());
    let container = Value::Instance(InstanceHandle(Arc::new(InstanceState {
        class: cls,
        kind: InstanceKind::ValueObject(obj),
    })));

    let mut buf = Vec::new();
    collect_value_subtree(&container, &mut buf);

    let point_count = buf.iter().filter(|(id, _)| *id == point_id).count();
    assert_eq!(
        point_count, 1,
        "shared nested value must appear exactly once"
    );
}

#[test]
fn collect_value_with_indexed_slots() {
    use crate::class::Class;
    use crate::class::IndexedSlotKind;

    let cls = Arc::new(Class::new(
        Arc::from("ImmTuple"),
        None,
        vec![],
        vec![],
        Some(IndexedSlotKind::Pointer),
        true,
    ));
    let obj = Arc::new(crate::object::Object::new_with_indexed(cls.clone(), 2));
    if let Some(crate::object::IndexedStorage::Pointer(slots)) = &obj.indexed {
        let mut guard = slots.lock();
        guard[0] = Value::Int(10);
        guard[1] = Value::Int(20);
    }
    let v = Value::Instance(InstanceHandle(Arc::new(InstanceState {
        class: cls,
        kind: InstanceKind::ValueObject(obj),
    })));

    let mut buf = Vec::new();
    collect_value_subtree(&v, &mut buf);

    assert_eq!(buf.len(), 1);
    let sv = &buf[0].1;
    assert_eq!(sv.indexed_fields, vec![Term::Int(10), Term::Int(20)]);
}

// ── S5 integration: buffer_ivar → commit → lookup_value ──────────────────

#[test]
fn buffer_ivar_value_object_commits_structural_record() {
    use triple_store::exec_context::ExecContext;
    use triple_store::predicate_registry::Visibility;
    use triple_store::predicates::Cardinality;

    let ev = Evaluator::new_without_kernel();

    // Declare the ivar predicate so buffer_ivar can look it up.
    ev.registry
        .store
        .declare("price", Visibility::Private, Cardinality::Single)
        .unwrap();

    let entity_id = EntityId(42);
    let point = make_value_object("Money", &[("amount", Value::Int(100))]);
    let point_val = Value::Instance(point);
    let expected_hash = content_hash_id(&point_val);

    // Set up a transaction on the evaluator (as entity loop does).
    let store = ev.registry.store.clone_handle();
    let tx = store.begin_owned(ExecContext::Entity(entity_id));
    *ev.current_tx.borrow_mut() = Some(tx);

    buffer_ivar(&ev, entity_id, &Arc::from("price"), &point_val);

    // Commit the transaction.
    let tx = ev.current_tx.borrow_mut().take().unwrap();
    tx.commit().expect("commit must succeed");

    let sv = store
        .lookup_value(expected_hash)
        .expect("StoredValue must be present after commit");
    assert_eq!(sv.class_name.as_ref(), "Money");
    assert!(
        sv.named_fields
            .iter()
            .any(|(n, t)| { n.as_ref() == "amount" && *t == Term::Int(100) })
    );
}

#[test]
fn buffer_ivar_reassign_value_object_adds_new_record_old_remains() {
    use triple_store::exec_context::ExecContext;
    use triple_store::predicate_registry::Visibility;
    use triple_store::predicates::Cardinality;

    let ev = Evaluator::new_without_kernel();
    ev.registry
        .store
        .declare("color", Visibility::Private, Cardinality::Single)
        .unwrap();

    let entity_id = EntityId(43);
    let store = ev.registry.store.clone_handle();

    // First assignment.
    let v1 = Value::Instance(make_value_object("Color", &[("r", Value::Int(255))]));
    let id1 = content_hash_id(&v1);
    let tx1 = store.begin_owned(ExecContext::Entity(entity_id));
    *ev.current_tx.borrow_mut() = Some(tx1);
    buffer_ivar(&ev, entity_id, &Arc::from("color"), &v1);
    ev.current_tx.borrow_mut().take().unwrap().commit().unwrap();

    // Second assignment with a different value.
    let v2 = Value::Instance(make_value_object("Color", &[("r", Value::Int(0))]));
    let id2 = content_hash_id(&v2);
    assert_ne!(id1, id2, "different values must have different hash ids");

    let tx2 = store.begin_owned(ExecContext::Entity(entity_id));
    *ev.current_tx.borrow_mut() = Some(tx2);
    buffer_ivar(&ev, entity_id, &Arc::from("color"), &v2);
    ev.current_tx.borrow_mut().take().unwrap().commit().unwrap();

    // Both records must be present.
    assert!(
        store.lookup_value(id1).is_some(),
        "old value record must still exist"
    );
    assert!(
        store.lookup_value(id2).is_some(),
        "new value record must exist"
    );
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
