//! Workspaces: the long-lived top-level evaluation contexts that chunks run in
//! (env-frame redesign §3, §11.7c).
//!
//! A workspace is what a REPL session, a notebook, or a script run evaluates its
//! chunks against. It has a namespace — anonymous for an ordinary workspace — in
//! which its chunks' definitions (classes, traits) are registered and through which
//! their free names resolve (own entries → imports → Kernel → root names). Its temp
//! bindings are never visible to methods.
//!
//! Current state (d2c-c2): temps still live in a name-keyed `Env`; c3 resolves
//! chunks as slot scopes whose temps are published here.

use std::sync::Arc;

use crate::env::Env;
use crate::eval::Evaluator;
use crate::namespace::Namespace;

/// A top-level evaluation context; see the module docs.
pub struct Workspace {
    namespace: Arc<Namespace>,
    env: Env,
}

impl Workspace {
    /// Create a workspace for evaluating chunks on `ev`.
    ///
    /// Ensures: the workspace has a fresh anonymous namespace, held by `ev`'s
    ///   registry; chunks evaluated in it see Kernel (implicitly imported) and the
    ///   root namespace names, but no other workspace's definitions.
    pub fn new(ev: &Evaluator) -> Self {
        Self {
            namespace: ev.registry.new_workspace_namespace(),
            env: Env::new(),
        }
    }

    /// A workspace whose namespace is Kernel itself, so definitions made in it
    /// become Kernel's — the workspace kernel source (`kernel.smol`) is loaded in.
    pub fn kernel(ev: &Evaluator) -> Self {
        Self {
            namespace: ev.registry.kernel(),
            env: Env::new(),
        }
    }

    /// The namespace this workspace's chunks define into and resolve through.
    pub fn namespace(&self) -> &Arc<Namespace> {
        &self.namespace
    }

    /// The name-keyed environment holding this workspace's temps.
    pub(crate) fn env_mut(&mut self) -> &mut Env {
        &mut self.env
    }
}
