//! Provides a way to track and validate execution context
//! for correct access control.

use crate::id::EntityId;
use crate::predicate_registry::{PredicateMeta, Visibility};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecContext {
    Entity(EntityId),
    System,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AccessError {
    Denied {
        predicate: PredicateMeta,
        reason: &'static str,
    },
}

impl ExecContext {
    pub fn check_assert(
        &self,
        subject: EntityId,
        predicate: PredicateMeta,
    ) -> Result<(), AccessError> {
        match self {
            // System can assert anything (bypass access control)
            ExecContext::System => Ok(()),
            // Entity can assert any owned predicate
            ExecContext::Entity(asserter) if *asserter == subject => Ok(()),
            // Any entity can assert a Public predicate
            ExecContext::Entity(_) if predicate.visibility == Visibility::Public => Ok(()),
            // An external entity cannot assert another shared/private predicate
            ExecContext::Entity(_) => Err(AccessError::Denied {
                predicate: predicate,
                reason: "Cannot assert a non-public predicate on another entity.",
            }),
        }
    }

    pub fn check_read(
        &self,
        subject: EntityId,
        predicate: PredicateMeta,
    ) -> Result<(), AccessError> {
        match self {
            // System can read anything (bypass access control)
            ExecContext::System => Ok(()),
            // Entity can read any owned predicate
            ExecContext::Entity(asserter) if *asserter == subject => Ok(()),
            // Any entity can read a shared or public predicate
            ExecContext::Entity(_) if predicate.visibility >= Visibility::Shared => Ok(()),
            // An external entity cannot read a private predicate
            ExecContext::Entity(_) => Err(AccessError::Denied {
                predicate: predicate,
                reason: "Cannot read a private predicate on another entity.",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{predicates::Cardinality, term::Predicate};

    use super::*;

    fn make_pred(id: u32, visibility: Visibility) -> PredicateMeta {
        PredicateMeta {
            id: Predicate(id),
            visibility,
            cardinality: Cardinality::Single,
        }
    }

    fn priv_pred(id: u32) -> PredicateMeta {
        make_pred(id, Visibility::Private)
    }
    fn shared_pred(id: u32) -> PredicateMeta {
        make_pred(id, Visibility::Shared)
    }
    fn public_pred(id: u32) -> PredicateMeta {
        make_pred(id, Visibility::Public)
    }

    // - `System` permits internal assert for any subject.
    // - `Entity(1)` permits internal assert for subject 1.
    // - `Entity(1)` denies internal assert for subject 2.
    // - `Entity(x)` denies external assert on `Private` predicate.
    // - `Entity(x)` denies external assert on `Shared` predicate.
    // - `System` denies external assert on `Private` predicate.
    // - Any context permits external assert on `Public` predicate.
    // - `Private` predicate: only owner and `System` can read.
    // - `Shared` predicate: any context can read.

    #[test]
    fn system_can_assert_any_predicate_for_any_subject() {
        let subj1 = EntityId(1);
        let subj2 = EntityId(2);
        let ctx = ExecContext::System;

        assert_eq!(ctx.check_assert(subj1, priv_pred(42)), Ok(()));
        assert_eq!(ctx.check_assert(subj1, shared_pred(42)), Ok(()));
        assert_eq!(ctx.check_assert(subj2, public_pred(42)), Ok(()));
    }

    #[test]
    fn entity_can_assert_any_owned_predicate() {
        let subj1 = EntityId(1);
        let ctx = ExecContext::Entity(subj1);

        assert_eq!(ctx.check_assert(subj1, priv_pred(42)), Ok(()));
        assert_eq!(ctx.check_assert(subj1, shared_pred(42)), Ok(()));
        assert_eq!(ctx.check_assert(subj1, public_pred(42)), Ok(()));
    }

    #[test]
    fn entity_cannot_assert_unowned_nonpublic_predicate() {
        let subj1 = EntityId(1);
        let subj2 = EntityId(2);
        let ctx = ExecContext::Entity(subj1);

        let mut pred = priv_pred(42);
        let mut err = ctx.check_assert(subj2, pred).unwrap_err();
        assert!(matches!(err, AccessError::Denied { .. }));
        pred = shared_pred(42);
        err = ctx.check_assert(subj2, pred).unwrap_err();
        assert!(matches!(err, AccessError::Denied { .. }));
    }

    #[test]
    fn entity_can_assert_unowned_public_predicate() {
        let subj1 = EntityId(1);
        let subj2 = EntityId(2);
        let ctx = ExecContext::Entity(subj1);

        assert_eq!(ctx.check_assert(subj2, public_pred(42)), Ok(()));
    }

    #[test]
    fn any_context_can_assert_public_predicate() {
        let subj1 = EntityId(1);
        let subj2 = EntityId(2);
        let ctxs = [
            ExecContext::Entity(subj1),
            ExecContext::Entity(subj2),
            ExecContext::System,
        ];

        let pred = public_pred(42);
        for ctx in ctxs {
            assert_eq!(ctx.check_assert(subj1, pred), Ok(()));
        }
    }

    #[test]
    fn system_can_read_any_predicate_for_any_subject() {
        let subj1 = EntityId(1);
        let ctx = ExecContext::System;
        let preds = [priv_pred(42), shared_pred(42), public_pred(42)];

        for pred in preds {
            assert_eq!(ctx.check_read(subj1, pred), Ok(()));
        }
    }

    #[test]
    fn entity_can_read_any_owned_predicate() {
        let subj1 = EntityId(1);
        let ctx = ExecContext::Entity(subj1);

        assert_eq!(ctx.check_read(subj1, priv_pred(42)), Ok(()));
        assert_eq!(ctx.check_read(subj1, shared_pred(42)), Ok(()));
        assert_eq!(ctx.check_read(subj1, public_pred(42)), Ok(()));
    }

    #[test]
    fn entity_cannot_read_unowned_private_predicate() {
        let subj1 = EntityId(1);
        let subj2 = EntityId(2);
        let ctx = ExecContext::Entity(subj1);

        let pred = priv_pred(42);
        let err = ctx.check_read(subj2, pred).unwrap_err();
        assert!(matches!(err, AccessError::Denied { .. }));
    }

    #[test]
    fn entity_can_read_unowned_nonprivate_predicate() {
        let subj1 = EntityId(1);
        let subj2 = EntityId(2);
        let ctx = ExecContext::Entity(subj1);

        assert_eq!(ctx.check_read(subj2, shared_pred(42)), Ok(()));
        assert_eq!(ctx.check_read(subj2, public_pred(42)), Ok(()));
    }

    #[test]
    fn any_context_can_read_public_predicate() {
        let subj1 = EntityId(1);
        let subj2 = EntityId(2);
        let ctxs = [
            ExecContext::Entity(subj1),
            ExecContext::Entity(subj2),
            ExecContext::System,
        ];

        let pred = public_pred(42);
        for ctx in ctxs {
            assert_eq!(ctx.check_read(subj1, pred), Ok(()));
        }
    }
}
