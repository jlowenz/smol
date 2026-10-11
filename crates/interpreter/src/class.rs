use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::RwLock;

use crate::ast::TaggedStmt;
use crate::value::Value;

/// Indexed storage kind declared at class definition time.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexedSlotKind {
    Pointer,
    Byte,
    Float64,
}

/// Parameter binding pattern for a method definition.
#[derive(Debug, Clone, PartialEq)]
pub enum MethodPattern {
    Unary(Arc<str>),
    Binary {
        op: Arc<str>,
        param: Arc<str>,
    },
    /// `(keyword, param_name)` pairs; e.g. `[(at:, i), (put:, v)]` → selector `at:put:`.
    Keyword {
        parts: Vec<(Arc<str>, Arc<str>)>,
    },
}

impl MethodPattern {
    /// The full selector string.
    pub fn selector(&self) -> Arc<str> {
        match self {
            MethodPattern::Unary(name) => name.clone(),
            MethodPattern::Binary { op, .. } => op.clone(),
            MethodPattern::Keyword { parts } => {
                let s: String = parts.iter().map(|(kw, _)| kw.as_ref()).collect();
                Arc::from(s.as_str())
            }
        }
    }

    /// Parameter names in definition order.
    pub fn params(&self) -> Vec<Arc<str>> {
        match self {
            MethodPattern::Unary(_) => vec![],
            MethodPattern::Binary { param, .. } => vec![param.clone()],
            MethodPattern::Keyword { parts } => parts.iter().map(|(_, p)| p.clone()).collect(),
        }
    }
}

impl std::fmt::Debug for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Class({})", self.name)
    }
}

/// A compiled method.
pub struct Method {
    pub pattern: MethodPattern,
    /// Local variable names declared with `| x y |` in the method body.
    pub locals: Vec<Arc<str>>,
    pub body: Vec<TaggedStmt>,
}

/// A user-defined class.
///
/// `is_value` is inherited: a class with `is_value: true` is a passive value
/// type (immutable, no actor thread). `Object` has `is_value: false`; `Value`
/// has `is_value: true`.
pub struct Class {
    pub name: Arc<str>,
    /// `None` only for the root `Object` class.
    pub superclass: Option<Arc<Class>>,
    /// Instance variable names owned by this class (not inherited).
    ///
    /// Requires: disjoint from all superclass instance variable names.
    pub own_instance_vars: Vec<Arc<str>>,
    /// Class variable names owned by this class.
    pub own_class_vars: Vec<Arc<str>>,
    /// Shared storage for this class's own class variables.
    pub class_var_storage: RwLock<HashMap<Arc<str>, Value>>,
    /// `None` means no indexed region.
    pub indexed_slot_kind: Option<IndexedSlotKind>,
    pub methods: RwLock<HashMap<Arc<str>, Arc<Method>>>,
    /// Methods defined on the class side (`ClassName class >> selector`).
    pub class_methods: RwLock<HashMap<Arc<str>, Arc<Method>>>,
    /// If `true`, instances are passive value objects (no actor thread).
    /// Inherited through the superclass chain: a subclass of a value class
    /// is also a value class unless a mixed-hierarchy error is raised first.
    pub is_value: bool,
    /// Back-reference to the canonical `InstanceState` (ClassObject kind) for this class.
    ///
    /// Set once after the `InstanceState` is allocated by `ClassRegistry::new()` or
    /// `handle_subclass`; always populated before any message dispatch can reach this class.
    /// `Weak` avoids a reference cycle between `InstanceState → Class → InstanceState`.
    pub class_object: OnceLock<Weak<crate::entity::InstanceState>>,
    /// The namespace this class is defined in — the scope a method's free names
    /// resolve through. Set once by `Namespace::register` (first registration wins:
    /// a class belongs to exactly one namespace). `Weak` avoids the cycle
    /// `Namespace → InstanceHandle → Class → Namespace`; the namespace is owned by
    /// the registry (root namespaces) or its parent (sub-namespaces).
    pub namespace: OnceLock<Weak<crate::namespace::Namespace>>,
}

impl Class {
    /// The namespace this class is defined in, if it was registered in one that is
    /// still live.
    pub fn namespace(&self) -> Option<Arc<crate::namespace::Namespace>> {
        self.namespace.get()?.upgrade()
    }

    /// All instance variable names in definition order: superclass vars first, then own.
    pub fn all_instance_vars(&self) -> Vec<Arc<str>> {
        let mut vars = match &self.superclass {
            Some(sc) => sc.all_instance_vars(),
            None => vec![],
        };
        vars.extend(self.own_instance_vars.iter().cloned());
        vars
    }

    /// Look up a method by selector, walking the superclass chain.
    ///
    /// Returns the method and the `Arc<Class>` it was found in (needed for `super` dispatch).
    pub fn lookup(self: &Arc<Self>, selector: &str) -> Option<(Arc<Method>, Arc<Class>)> {
        if let Some(m) = self.methods.read().get(selector) {
            return Some((m.clone(), self.clone()));
        }
        if let Some(sc) = &self.superclass {
            return sc.lookup(selector);
        }
        None
    }

    /// Look up a class-side method by selector, walking the superclass chain.
    ///
    /// Returns the method and the `Arc<Class>` it was found in (needed for `super` dispatch).
    pub fn lookup_class_method(
        self: &Arc<Self>,
        selector: &str,
    ) -> Option<(Arc<Method>, Arc<Class>)> {
        if let Some(m) = self.class_methods.read().get(selector) {
            return Some((m.clone(), self.clone()));
        }
        if let Some(sc) = &self.superclass {
            return sc.lookup_class_method(selector);
        }
        None
    }

    /// Walk the superclass chain to find which class declares a given class variable.
    ///
    /// Ensures: returns the highest ancestor that declares `name`, so writes always
    /// go to the same storage entry regardless of which subclass's instance triggers them.
    pub fn lookup_class_var_owner(self: &Arc<Self>, name: &str) -> Option<Arc<Class>> {
        let in_super = self
            .superclass
            .as_ref()
            .and_then(|sc| sc.lookup_class_var_owner(name));
        if in_super.is_some() {
            return in_super;
        }
        if self.own_class_vars.iter().any(|v| v.as_ref() == name) {
            Some(self.clone())
        } else {
            None
        }
    }

    /// Returns `true` if `child` is `parent` or any ancestor of `child` is `parent`.
    ///
    /// Requires: both classes are fully constructed (not mid-definition).
    /// Ensures: `is_subclass_of(c, c)` is `true`; `is_subclass_of(Sub, Super)` is `true`
    ///   iff `Super` appears in `Sub`'s superclass chain.
    pub fn is_subclass_of(child: &Arc<Class>, parent: &Arc<Class>) -> bool {
        if Arc::ptr_eq(child, parent) {
            return true;
        }
        match &child.superclass {
            Some(sc) => Class::is_subclass_of(sc, parent),
            None => false,
        }
    }

    /// Returns `true` if this class (or any ancestor) has `is_value: true`.
    pub fn inherits_is_value(&self) -> bool {
        if self.is_value {
            return true;
        }
        if let Some(sc) = &self.superclass {
            sc.inherits_is_value()
        } else {
            false
        }
    }
}
