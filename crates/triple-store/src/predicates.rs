//! Predicate-level metadata: cardinality and the ordering key used by the
//! Indexed cardinality. (`PredicateMeta` itself is added in a later phase.)

/// The supersession/disambiguation rule for asserts of a given predicate.
///
/// Each predicate is declared with one cardinality. The store enforces the
/// matching invariant on assert; the `FactStore` exposes one assert/retract
/// method pair per cardinality. Cardinality always applies *within an asserter's
/// virtual namespace*: cross-asserter conflicts are preserved regardless.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
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
}
