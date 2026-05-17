use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::Stmt;
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
    Binary { op: Arc<str>, param: Arc<str> },
    /// `(keyword, param_name)` pairs; e.g. `[(at:, i), (put:, v)]` → selector `at:put:`.
    Keyword { parts: Vec<(Arc<str>, Arc<str>)> },
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
    pub body: Vec<Stmt>,
}

/// A user-defined class.
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
    pub class_var_storage: RefCell<HashMap<Arc<str>, Value>>,
    /// `None` means no indexed region.
    pub indexed_slot_kind: Option<IndexedSlotKind>,
    pub methods: RefCell<HashMap<Arc<str>, Arc<Method>>>,
}

impl Class {
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
        if let Some(m) = self.methods.borrow().get(selector) {
            return Some((m.clone(), self.clone()));
        }
        if let Some(sc) = &self.superclass {
            return sc.lookup(selector);
        }
        None
    }

    /// Walk the superclass chain to find which class declares a given class variable.
    ///
    /// Ensures: returns the highest ancestor that declares `name`, so writes always
    /// go to the same storage entry regardless of which subclass's instance triggers them.
    pub fn lookup_class_var_owner(self: &Arc<Self>, name: &str) -> Option<Arc<Class>> {
        let in_super = self.superclass.as_ref().and_then(|sc| sc.lookup_class_var_owner(name));
        if in_super.is_some() {
            return in_super;
        }
        if self.own_class_vars.iter().any(|v| v.as_ref() == name) {
            Some(self.clone())
        } else {
            None
        }
    }
}
