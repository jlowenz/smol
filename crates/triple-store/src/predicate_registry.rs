//! Predicate registry: name interning with visibility and cardinality metadata.
//!
//! Per-`Store`-instance, never global (PRD §1.1, §2.5). Each store owns a
//! `PredicateRegistry` behind a `RwLock`; test instances are fully isolated.
//!
//! # Defaults
//!
//! Predicates interned implicitly by assert paths (via `intern_default`) are
//! assigned `Visibility::Private` and `Cardinality::Single` until explicitly
//! declared otherwise. This is the most conservative choice: broader access and
//! cardinality semantics must be opted into.

use std::sync::Arc;

use crate::intern::PredicateInterner;
use crate::predicates::Cardinality;
use crate::term::Predicate;

/// Access scope for a predicate.
///
/// Ordered from most restrictive to most permissive; `Ord` follows declaration
/// order: `Private < Shared < Public`. This enables "at least this visible"
/// comparisons with the standard `>=` operator.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Visibility {
    /// Writable and readable only by the owning entity.
    Private,
    /// Readable by all entities; writable only by the owner.
    Shared,
    /// Open-world: any entity may assert or retract.
    Public,
}

/// Metadata recorded for a declared predicate.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PredicateMeta {
    pub id: Predicate,
    pub visibility: Visibility,
    pub cardinality: Cardinality,
}

/// Errors from predicate registry operations.
#[derive(Debug, PartialEq, Eq)]
pub enum PredicateError {
    /// A prior declaration for `name` exists with different visibility or
    /// cardinality. The `existing` field shows what was previously recorded;
    /// `requested` shows what the caller tried to declare.
    ConflictingDeclaration {
        name: String,
        existing: PredicateMeta,
        requested: PredicateMeta,
    },
}

const DEFAULT_VISIBILITY: Visibility = Visibility::Private;
const DEFAULT_CARDINALITY: Cardinality = Cardinality::Single;

/// Per-store registry mapping predicate names to `Predicate` handles and their
/// visibility and cardinality metadata.
///
/// Owns a `PredicateInterner` internally; the interner is not exposed directly
/// so that the registry remains the single policy-bearing entry point.
#[derive(Debug, Default, Clone)]
pub struct PredicateRegistry {
    interner: PredicateInterner,
    /// Parallel to the interner's dense id space. `None` for a slot means the
    /// predicate was interned implicitly; default metadata applies.
    meta: Vec<Option<PredicateMeta>>,
}

impl PredicateRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a predicate with explicit visibility and cardinality.
    ///
    /// Requires: if `name` has been declared before, its prior visibility and
    ///   cardinality must equal `visibility` and `cardinality` respectively.
    /// Ensures: on `Ok(id)`, `self.meta(id).visibility == visibility` and
    ///   `self.meta(id).cardinality == cardinality`. Re-declaration with
    ///   identical metadata is idempotent and returns the same `id`.
    pub fn declare(
        &mut self,
        name: &str,
        visibility: Visibility,
        cardinality: Cardinality,
    ) -> Result<Predicate, PredicateError> {
        let id = self.interner.intern(name);
        self.ensure_slot(id);

        let requested = PredicateMeta {
            id,
            visibility,
            cardinality,
        };

        match self.meta[id.0 as usize] {
            None => {
                self.meta[id.0 as usize] = Some(requested);
                Ok(id)
            }
            Some(existing) if existing == requested => Ok(id),
            Some(existing) => Err(PredicateError::ConflictingDeclaration {
                name: name.to_owned(),
                existing,
                requested,
            }),
        }
    }

    /// Look up the handle for a previously-declared or implicitly-interned name.
    ///
    /// Ensures: `Some(id)` iff `name` has been passed to `declare` or
    ///   `intern_default`; else `None`. Does not intern.
    pub fn lookup(&self, name: &str) -> Option<Predicate> {
        self.interner.lookup(name)
    }

    /// Return metadata for a predicate.
    ///
    /// Ensures: never panics for any `Predicate` issued by this registry.
    ///   Undeclared predicates (interned via `intern_default`) return
    ///   `Visibility::Private` and `Cardinality::Single`.
    pub fn meta(&self, id: Predicate) -> PredicateMeta {
        self.meta
            .get(id.0 as usize)
            .and_then(|slot| *slot)
            .unwrap_or(PredicateMeta {
                id,
                visibility: DEFAULT_VISIBILITY,
                cardinality: DEFAULT_CARDINALITY,
            })
    }

    /// Intern `name` without an explicit declaration.
    ///
    /// Used by assert paths that do not require a prior declaration. If `name`
    /// was already declared, the existing metadata is preserved unchanged.
    ///
    /// Ensures: the returned `Predicate` is consistent with `lookup(name)`.
    pub fn intern_default(&mut self, name: &str) -> Predicate {
        let id = self.interner.intern(name);
        self.ensure_slot(id);
        id
    }

    /// Resolve a predicate handle to its name.
    ///
    /// Ensures: `Some(name)` iff `id` was issued by this registry; else `None`.
    pub fn resolve(&self, id: Predicate) -> Option<Arc<str>> {
        self.interner.resolve(id)
    }

    fn ensure_slot(&mut self, id: Predicate) {
        let needed = id.0 as usize + 1;
        if self.meta.len() < needed {
            self.meta.resize(needed, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> PredicateRegistry {
        PredicateRegistry::new()
    }

    // ── declare ──────────────────────────────────────────────────────────────

    #[test]
    fn declare_returns_handle_and_round_trips_meta() {
        let mut r = reg();
        let id = r
            .declare("account.balance", Visibility::Private, Cardinality::Single)
            .unwrap();
        let m = r.meta(id);
        assert_eq!(m.id, id);
        assert_eq!(m.visibility, Visibility::Private);
        assert_eq!(m.cardinality, Cardinality::Single);
    }

    #[test]
    fn declare_same_meta_twice_is_idempotent() {
        let mut r = reg();
        let id1 = r
            .declare("p", Visibility::Shared, Cardinality::Set)
            .unwrap();
        let id2 = r
            .declare("p", Visibility::Shared, Cardinality::Set)
            .unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn declare_conflicting_visibility_errors() {
        let mut r = reg();
        r.declare("p", Visibility::Private, Cardinality::Single)
            .unwrap();
        let err = r
            .declare("p", Visibility::Public, Cardinality::Single)
            .unwrap_err();
        assert!(matches!(err, PredicateError::ConflictingDeclaration { name, .. } if name == "p"));
    }

    #[test]
    fn declare_conflicting_cardinality_errors() {
        let mut r = reg();
        r.declare("p", Visibility::Private, Cardinality::Single)
            .unwrap();
        let err = r
            .declare("p", Visibility::Private, Cardinality::Set)
            .unwrap_err();
        assert!(matches!(err, PredicateError::ConflictingDeclaration { .. }));
    }

    #[test]
    fn declare_conflicting_error_carries_both_existing_and_requested() {
        let mut r = reg();
        r.declare("p", Visibility::Private, Cardinality::Single)
            .unwrap();
        let id = r.lookup("p").unwrap();
        let err = r
            .declare("p", Visibility::Public, Cardinality::Set)
            .unwrap_err();
        let PredicateError::ConflictingDeclaration {
            existing,
            requested,
            ..
        } = err;
        assert_eq!(existing.id, id);
        assert_eq!(existing.visibility, Visibility::Private);
        assert_eq!(existing.cardinality, Cardinality::Single);
        assert_eq!(requested.visibility, Visibility::Public);
        assert_eq!(requested.cardinality, Cardinality::Set);
    }

    // ── intern_default / meta defaults ───────────────────────────────────────

    #[test]
    fn intern_default_yields_private_single_meta() {
        let mut r = reg();
        let id = r.intern_default("implicit");
        let m = r.meta(id);
        assert_eq!(m.visibility, Visibility::Private);
        assert_eq!(m.cardinality, Cardinality::Single);
    }

    #[test]
    fn intern_default_preserves_prior_declaration() {
        let mut r = reg();
        let declared = r
            .declare("p", Visibility::Public, Cardinality::Set)
            .unwrap();
        let interned = r.intern_default("p");
        assert_eq!(declared, interned);
        assert_eq!(r.meta(declared).visibility, Visibility::Public);
    }

    #[test]
    fn meta_for_never_seen_id_returns_defaults() {
        let r = reg();
        // Predicate(42) was never issued; meta must not panic and returns defaults.
        let m = r.meta(Predicate(42));
        assert_eq!(m.visibility, Visibility::Private);
        assert_eq!(m.cardinality, Cardinality::Single);
    }

    // ── lookup / resolve ─────────────────────────────────────────────────────

    #[test]
    fn lookup_undeclared_returns_none() {
        let r = reg();
        assert_eq!(r.lookup("unknown"), None);
    }

    #[test]
    fn lookup_finds_declared_predicate() {
        let mut r = reg();
        let id = r
            .declare("p", Visibility::Private, Cardinality::Single)
            .unwrap();
        assert_eq!(r.lookup("p"), Some(id));
    }

    #[test]
    fn lookup_finds_implicitly_interned_predicate() {
        let mut r = reg();
        let id = r.intern_default("q");
        assert_eq!(r.lookup("q"), Some(id));
    }

    #[test]
    fn resolve_round_trips_declared_name() {
        let mut r = reg();
        let id = r
            .declare("account.balance", Visibility::Private, Cardinality::Single)
            .unwrap();
        assert_eq!(r.resolve(id).as_deref(), Some("account.balance"));
    }

    // ── Visibility ordering ───────────────────────────────────────────────────

    #[test]
    fn visibility_order_is_private_lt_shared_lt_public() {
        assert!(Visibility::Private < Visibility::Shared);
        assert!(Visibility::Shared < Visibility::Public);
        assert!(Visibility::Private < Visibility::Public);
    }

    #[test]
    fn visibility_ge_comparison() {
        assert!(Visibility::Shared >= Visibility::Private);
        assert!(Visibility::Public >= Visibility::Public);
    }
}
