//! Predicate-level metadata: cardinality and the ordering key used by the
//! Indexed cardinality. (`PredicateMeta` itself is added in a later phase.)

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The supersession/disambiguation rule for asserts of a given predicate.
///
/// Each predicate is declared with one cardinality. The store enforces the
/// matching invariant on assert; the `FactStore` exposes one assert/retract
/// method pair per cardinality. Cardinality always applies *within an asserter's
/// virtual namespace*: cross-asserter conflicts are preserved regardless.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Cardinality {
    /// At most one open fact per `(subject, predicate, asserter)`.
    /// Asserting closes any prior open fact for that key.
    Single,

    /// At most one open fact per `(subject, predicate, asserter, object)`.
    /// Asserting an already-open `(s, p, asserter, o)` is a no-op; distinct
    /// objects coexist as open facts.
    Set,

    /// At most one open fact per `(subject, predicate, asserter, index_key)`,
    /// where `index_key` is an `OrderingKey`. Multiple objects can coexist
    /// with distinct keys; ordering is given by the key.
    Indexed,
}

/// A fractional ordering key for the `Indexed` cardinality.
///
/// Inserting between two keys `a < b` is `OrderingKey::midpoint(a, b)`. After
/// many between-insertions, `f64` precision degrades; periodic normalization
/// redistributes keys evenly (mechanism deferred beyond v0).
///
/// NaN is rejected at construction so `OrderingKey` has a total order.
/// Equality and hashing use the underlying bit pattern (so `0.0 != -0.0`,
/// matching `Term::Float`); ordering uses `f64::total_cmp`.
#[derive(Copy, Clone, Debug)]
pub struct OrderingKey(f64);

impl OrderingKey {
    pub const ZERO: Self = Self(0.0);

    /// Construct an ordering key.
    ///
    /// Ensures: returns `Some(_)` iff `v` is not NaN; else `None`.
    pub fn new(v: f64) -> Option<Self> {
        if v.is_nan() { None } else { Some(Self(v)) }
    }

    /// Return the underlying raw `f64` value.
    pub fn as_f64(self) -> f64 {
        self.0
    }

    /// Midpoint of two keys, suitable for inserting between `a` and `b`.
    ///
    /// Requires: `a` and `b` are both finite (caller's responsibility);
    /// typically `a < b`.
    pub fn midpoint(a: Self, b: Self) -> Self {
        Self((a.0 + b.0) / 2.0)
    }
}

impl PartialEq for OrderingKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for OrderingKey {}

impl PartialOrd for OrderingKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderingKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl std::hash::Hash for OrderingKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

// Serialize/deserialize as raw u64 bit pattern so NaN and -0.0 round-trip
// with bit-exact identity regardless of the backing format.
impl Serialize for OrderingKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.0.to_bits())
    }
}

impl<'de> Deserialize<'de> for OrderingKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bits = u64::deserialize(d)?;
        Ok(Self(f64::from_bits(bits)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn new_rejects_nan() {
        assert!(OrderingKey::new(f64::NAN).is_none());
    }

    #[test]
    fn new_accepts_finite_and_infinite() {
        assert!(OrderingKey::new(0.0).is_some());
        assert!(OrderingKey::new(1.5).is_some());
        assert!(OrderingKey::new(f64::INFINITY).is_some());
        assert!(OrderingKey::new(f64::NEG_INFINITY).is_some());
    }

    #[test]
    fn equality_is_bit_pattern() {
        let a = OrderingKey::new(0.0).unwrap();
        let b = OrderingKey::new(-0.0).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn ordering_is_total() {
        let a = OrderingKey::new(-1.0).unwrap();
        let b = OrderingKey::new(0.0).unwrap();
        let c = OrderingKey::new(1.0).unwrap();
        assert!(a < b && b < c);
        assert_eq!(a.cmp(&a), Ordering::Equal);
    }

    #[test]
    fn midpoint_between_two_keys() {
        let a = OrderingKey::new(0.0).unwrap();
        let b = OrderingKey::new(1.0).unwrap();
        let m = OrderingKey::midpoint(a, b);
        assert_eq!(m, OrderingKey::new(0.5).unwrap());
        assert!(a < m && m < b);
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn as_f64_returns_inner_value() {
        let k = OrderingKey::new(3.14).unwrap();
        assert_eq!(k.as_f64(), 3.14);

        let neg = OrderingKey::new(-42.0).unwrap();
        assert_eq!(neg.as_f64(), -42.0);
    }

    #[test]
    fn partial_ord_delegates_to_ord() {
        use std::cmp::Ordering;
        let a = OrderingKey::new(1.0).unwrap();
        let b = OrderingKey::new(2.0).unwrap();
        assert_eq!(a.partial_cmp(&b), Some(Ordering::Less));
        assert_eq!(b.partial_cmp(&a), Some(Ordering::Greater));
        assert_eq!(a.partial_cmp(&a), Some(Ordering::Equal));
    }

    #[test]
    fn hash_of_equal_keys_is_equal() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let a = OrderingKey::new(1.5).unwrap();
        let b = OrderingKey::new(1.5).unwrap();
        assert_eq!(a, b);

        let hash_of = |k: OrderingKey| {
            let mut h = DefaultHasher::new();
            k.hash(&mut h);
            h.finish()
        };
        assert_eq!(hash_of(a), hash_of(b));
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn serde_roundtrip_finite_value() {
        let k = OrderingKey::new(2.718).unwrap();
        let encoded = bincode::serialize(&k).unwrap();
        let decoded: OrderingKey = bincode::deserialize(&encoded).unwrap();
        assert_eq!(k, decoded);
    }

    #[test]
    fn serde_roundtrip_preserves_negative_zero_bit_pattern() {
        let neg_zero = OrderingKey::new(-0.0).unwrap();
        let pos_zero = OrderingKey::new(0.0).unwrap();
        // Bit patterns differ; serialization must preserve them.
        let encoded = bincode::serialize(&neg_zero).unwrap();
        let decoded: OrderingKey = bincode::deserialize(&encoded).unwrap();
        assert_eq!(neg_zero, decoded);
        assert_ne!(decoded, pos_zero);
    }

    #[test]
    fn cardinality_serde_roundtrip() {
        for c in [Cardinality::Single, Cardinality::Set, Cardinality::Indexed] {
            let encoded = bincode::serialize(&c).unwrap();
            let decoded: Cardinality = bincode::deserialize(&encoded).unwrap();
            assert_eq!(c, decoded);
        }
    }
}
