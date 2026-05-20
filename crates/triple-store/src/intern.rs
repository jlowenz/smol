//! String interning for predicates and symbols.
//!
//! Per-`Store`-instance, not global, so test instances are fully isolated
//! (PRD §1.1, §2.5).

use std::collections::HashMap;
use std::sync::Arc;

use crate::term::{Predicate, SymbolId};

/// Maps strings to compact `u32` handles. Concurrent `intern` calls require
/// external synchronization (the type does not provide its own).
#[derive(Debug, Default, Clone)]
pub struct StringInterner {
    by_string: HashMap<Arc<str>, u32>,
    by_id: Vec<Arc<str>>,
}

impl StringInterner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern `s` and return its handle.
    ///
    /// Ensures: `intern(s) == intern(s)` for any `s`;
    /// `resolve(intern(s)) == Some(s)`.
    /// Panics: if more than `u32::MAX` distinct strings have been interned.
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.by_string.get(s) {
            return id;
        }
        let arc: Arc<str> = Arc::from(s);
        let id = u32::try_from(self.by_id.len()).expect("intern table overflowed u32");
        self.by_id.push(arc.clone());
        self.by_string.insert(arc, id);
        id
    }

    /// Resolve a previously-issued id back to its string.
    ///
    /// Ensures: returns `Some(s)` iff `id` was issued by a prior `intern(s)`,
    /// else `None`. Returns a cloned `Arc<str>` (atomic increment) so callers
    /// are not lifetime-coupled to the interner.
    pub fn resolve(&self, id: u32) -> Option<Arc<str>> {
        self.by_id.get(id as usize).cloned()
    }

    /// Look up the id for an already-interned string without modifying the table.
    ///
    /// Ensures: returns `Some(id)` iff `s` has been interned, else `None`.
    pub fn lookup(&self, s: &str) -> Option<u32> {
        self.by_string.get(s).copied()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Typed wrapper over `StringInterner` for predicate names.
#[derive(Debug, Default, Clone)]
pub struct PredicateInterner(StringInterner);

impl PredicateInterner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern `name` and return its `Predicate` handle.
    ///
    /// Ensures: `intern(name) == intern(name)`; `resolve(intern(name)) == Some(name)`.
    pub fn intern(&mut self, name: &str) -> Predicate {
        Predicate(self.0.intern(name))
    }

    pub fn resolve(&self, p: Predicate) -> Option<Arc<str>> {
        self.0.resolve(p.0)
    }

    pub fn lookup(&self, name: &str) -> Option<Predicate> {
        self.0.lookup(name).map(Predicate)
    }
}

/// Typed wrapper over `StringInterner` for symbol values.
#[derive(Debug, Default, Clone)]
pub struct SymbolInterner(StringInterner);

impl SymbolInterner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern `name` and return its `SymbolId` handle.
    ///
    /// Ensures: `intern(name) == intern(name)`; `resolve(intern(name)) == Some(name)`.
    pub fn intern(&mut self, name: &str) -> SymbolId {
        SymbolId(self.0.intern(name))
    }

    pub fn resolve(&self, s: SymbolId) -> Option<Arc<str>> {
        self.0.resolve(s.0)
    }

    pub fn lookup(&self, name: &str) -> Option<SymbolId> {
        self.0.lookup(name).map(SymbolId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_string_interns_to_same_id() {
        let mut i = StringInterner::new();
        assert_eq!(i.intern("foo"), i.intern("foo"));
    }

    #[test]
    fn distinct_strings_get_distinct_ids() {
        let mut i = StringInterner::new();
        assert_ne!(i.intern("foo"), i.intern("bar"));
    }

    #[test]
    fn resolve_round_trips() {
        let mut i = StringInterner::new();
        let id = i.intern("hello");
        assert_eq!(i.resolve(id).as_deref(), Some("hello"));
    }

    #[test]
    fn lookup_finds_interned_and_misses_uninterned() {
        let mut i = StringInterner::new();
        let id = i.intern("foo");
        assert_eq!(i.lookup("foo"), Some(id));
        assert_eq!(i.lookup("bar"), None);
    }

    #[test]
    fn predicate_interner_is_typed() {
        let mut p = PredicateInterner::new();
        let id = p.intern("account.balance");
        assert_eq!(p.resolve(id).as_deref(), Some("account.balance"));
        assert_eq!(p.lookup("account.balance"), Some(id));
    }

    #[test]
    fn symbol_interner_is_typed() {
        let mut s = SymbolInterner::new();
        let id = s.intern("nil");
        assert_eq!(s.resolve(id).as_deref(), Some("nil"));
    }

    #[test]
    fn predicate_and_symbol_namespaces_are_separate() {
        let mut p = PredicateInterner::new();
        let mut s = SymbolInterner::new();
        let pid = p.intern("name");
        let sid = s.intern("name");
        // Both happen to be u32(0), but they live in different typed namespaces.
        assert_eq!(pid.0, sid.0);
    }

    #[test]
    fn string_interner_len_and_is_empty() {
        let mut i = StringInterner::new();
        assert!(i.is_empty());
        assert_eq!(i.len(), 0);

        i.intern("a");
        assert!(!i.is_empty());
        assert_eq!(i.len(), 1);

        i.intern("b");
        assert_eq!(i.len(), 2);

        // Re-interning an existing string does not grow the interner.
        i.intern("a");
        assert_eq!(i.len(), 2);
    }

    #[test]
    fn string_interner_default_is_empty() {
        let i = StringInterner::default();
        assert!(i.is_empty());
    }

    #[test]
    fn predicate_interner_default_is_usable() {
        let mut p = PredicateInterner::default();
        let id = p.intern("x");
        assert_eq!(p.resolve(id).as_deref(), Some("x"));
    }

    #[test]
    fn symbol_interner_default_is_usable() {
        let mut s = SymbolInterner::default();
        let id = s.intern("x");
        assert_eq!(s.resolve(id).as_deref(), Some("x"));
    }
}
