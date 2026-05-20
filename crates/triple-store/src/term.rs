//! The closed primitive value union (PRD §2.4) and its index policy.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::id::EntityId;
use crate::time::Timestamp;

/// Interned-symbol handle. The interner is owned by `Store` (Phase 2);
/// in Phase 0 this is just an opaque ID.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SymbolId(pub u32);

/// Interned-predicate handle. The registry is owned by `Store`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Predicate(pub u32);

/// Closed primitive value union (PRD §2.4). Composite/user-defined values
/// are deferred to v1+ via the `OpaqueValue` extension path.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Term {
    Int(i64),
    Float(f64),
    Bool(bool),
    Symbol(SymbolId),
    Str(Arc<str>),
    Bytes(Arc<[u8]>),
    Timestamp(Timestamp),
    EntityRef(EntityId),
}

// Index policy for `Float`: bit-pattern equality and hashing. This means
// `0.0 != -0.0` and `f64::NAN` *is* equal to itself when bit patterns match.
// This is the storage/index policy, not a domain semantics decision —
// see PRD §8.2 (open question).
impl PartialEq for Term {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a.to_bits() == b.to_bits(),
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Symbol(a), Self::Symbol(b)) => a == b,
            (Self::Str(a), Self::Str(b)) => a == b,
            (Self::Bytes(a), Self::Bytes(b)) => a == b,
            (Self::Timestamp(a), Self::Timestamp(b)) => a == b,
            (Self::EntityRef(a), Self::EntityRef(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Term {}

impl std::hash::Hash for Term {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Int(v) => v.hash(state),
            Self::Float(v) => v.to_bits().hash(state),
            Self::Bool(v) => v.hash(state),
            Self::Symbol(v) => v.hash(state),
            Self::Str(v) => v.hash(state),
            Self::Bytes(v) => v.hash(state),
            Self::Timestamp(v) => v.hash(state),
            Self::EntityRef(v) => v.hash(state),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn int_equality() {
        assert_eq!(Term::Int(42), Term::Int(42));
        assert_ne!(Term::Int(42), Term::Int(43));
    }

    #[test]
    fn float_zero_and_negative_zero_are_distinct() {
        assert_ne!(Term::Float(0.0), Term::Float(-0.0));
    }

    #[test]
    fn float_nan_is_equal_to_itself_by_bit_pattern() {
        let n = f64::NAN;
        assert_eq!(Term::Float(n), Term::Float(n));
    }

    #[test]
    fn variants_with_coincident_payloads_do_not_collide() {
        let mut set = HashSet::new();
        set.insert(Term::Int(0));
        set.insert(Term::Float(0.0));
        set.insert(Term::Bool(false));
        set.insert(Term::Symbol(SymbolId(0)));
        set.insert(Term::Timestamp(Timestamp(0)));
        set.insert(Term::EntityRef(EntityId(0)));
        assert_eq!(set.len(), 6);
    }

    #[test]
    fn arc_str_value_equality() {
        let a = Term::Str(Arc::from("hello"));
        let b = Term::Str(Arc::from("hello"));
        assert_eq!(a, b);
    }

    #[test]
    fn bool_equality_and_hash() {
        assert_eq!(Term::Bool(true), Term::Bool(true));
        assert_ne!(Term::Bool(true), Term::Bool(false));

        let mut set = HashSet::new();
        set.insert(Term::Bool(true));
        set.insert(Term::Bool(false));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn symbol_equality_and_hash() {
        assert_eq!(Term::Symbol(SymbolId(1)), Term::Symbol(SymbolId(1)));
        assert_ne!(Term::Symbol(SymbolId(1)), Term::Symbol(SymbolId(2)));

        let mut set = HashSet::new();
        set.insert(Term::Symbol(SymbolId(10)));
        set.insert(Term::Symbol(SymbolId(10)));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn bytes_equality_and_hash() {
        let a: Arc<[u8]> = Arc::from(vec![1u8, 2, 3].as_slice());
        let b: Arc<[u8]> = Arc::from(vec![1u8, 2, 3].as_slice());
        let c: Arc<[u8]> = Arc::from(vec![4u8, 5].as_slice());
        assert_eq!(Term::Bytes(a.clone()), Term::Bytes(b.clone()));
        assert_ne!(Term::Bytes(a.clone()), Term::Bytes(c.clone()));

        let mut set = HashSet::new();
        set.insert(Term::Bytes(a));
        set.insert(Term::Bytes(b)); // same content — should not grow set
        assert_eq!(set.len(), 1);
        set.insert(Term::Bytes(c));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn timestamp_equality_and_hash() {
        assert_eq!(Term::Timestamp(Timestamp(100)), Term::Timestamp(Timestamp(100)));
        assert_ne!(Term::Timestamp(Timestamp(100)), Term::Timestamp(Timestamp(200)));

        let mut set = HashSet::new();
        set.insert(Term::Timestamp(Timestamp(1)));
        set.insert(Term::Timestamp(Timestamp(1)));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn entity_ref_equality_and_hash() {
        assert_eq!(Term::EntityRef(EntityId(5)), Term::EntityRef(EntityId(5)));
        assert_ne!(Term::EntityRef(EntityId(5)), Term::EntityRef(EntityId(6)));

        let mut set = HashSet::new();
        set.insert(Term::EntityRef(EntityId(42)));
        set.insert(Term::EntityRef(EntityId(42)));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn cross_variant_inequality() {
        // The `_ => false` branch in PartialEq covers all cross-variant pairs.
        assert_ne!(Term::Int(0), Term::Float(0.0));
        assert_ne!(Term::Int(0), Term::Bool(false));
        assert_ne!(Term::Bool(false), Term::Int(0));
        assert_ne!(Term::Str(Arc::from("0")), Term::Symbol(SymbolId(0)));
        assert_ne!(Term::Bytes(Arc::from([].as_ref())), Term::Str(Arc::from("")));
        assert_ne!(Term::Timestamp(Timestamp(0)), Term::EntityRef(EntityId(0)));
    }
}
