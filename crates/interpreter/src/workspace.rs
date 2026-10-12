//! Workspaces: the long-lived top-level evaluation contexts that chunks run in
//! (env-frame redesign §3, §11.7c).
//!
//! A workspace is what a REPL session, a notebook, or a script run evaluates its
//! chunks against. It has a namespace — anonymous for an ordinary workspace — in
//! which its chunks' definitions (classes, traits) are registered and through which
//! their free names resolve (own entries → imports → Kernel → root names). It also
//! holds the temps its chunks have published. Those are never visible to methods.

use std::collections::HashMap;
use std::sync::Arc;

use crate::eval::Evaluator;
use crate::namespace::Namespace;
use crate::value::Value;

/// A top-level evaluation context; see the module docs.
pub struct Workspace {
    namespace: Arc<Namespace>,
    /// Temps published by earlier chunks, by name. A later chunk that redeclares a
    /// name replaces its binding here; blocks that captured the earlier one keep
    /// their snapshot.
    temps: HashMap<Arc<str>, Value>,
}

impl Workspace {
    /// Create a workspace for evaluating chunks on `ev`.
    ///
    /// Ensures: the workspace has a fresh anonymous namespace, held by `ev`'s
    ///   registry, and no temps; chunks evaluated in it see Kernel (implicitly
    ///   imported) and the root namespace names, but no other workspace's
    ///   definitions.
    pub fn new(ev: &Evaluator) -> Self {
        Self {
            namespace: ev.registry.new_workspace_namespace(),
            temps: HashMap::new(),
        }
    }

    /// A workspace whose namespace is Kernel itself, so definitions made in it
    /// become Kernel's — the workspace kernel source (`kernel.smol`) is loaded in.
    pub fn kernel(ev: &Evaluator) -> Self {
        Self {
            namespace: ev.registry.kernel(),
            temps: HashMap::new(),
        }
    }

    /// The namespace this workspace's chunks define into and resolve through.
    pub fn namespace(&self) -> &Arc<Namespace> {
        &self.namespace
    }

    /// The value of the temp `name` published by an earlier chunk, if any.
    pub(crate) fn temp(&self, name: &str) -> Option<&Value> {
        self.temps.get(name)
    }

    /// Publish a chunk's temp, replacing any earlier binding of `name`.
    ///
    /// Ensures: `self.temp(&name) == Some(&value)`.
    pub(crate) fn publish(&mut self, name: Arc<str>, value: Value) {
        self.temps.insert(name, value);
    }
}
