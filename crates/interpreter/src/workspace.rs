//! Workspaces: the long-lived top-level evaluation contexts that chunks run in
//! (env-frame redesign §3, §11.7c).
//!
//! A workspace is what a REPL session, a notebook, or a script run evaluates its
//! chunks against. Its temp bindings are never visible to methods.
//!
//! Current state (d2c-c1): a workspace wraps the name-keyed `Env` that top-level
//! code has always used, seeded with Kernel's definitions and the root namespace
//! names. Later sub-steps give it an anonymous namespace (c2) and resolve chunks as
//! slot scopes whose temps are published here (c3).

use crate::env::Env;
use crate::eval::Evaluator;

/// A top-level evaluation context; see the module docs.
pub struct Workspace {
    env: Env,
}

impl Workspace {
    /// Create a workspace for evaluating chunks on `ev`.
    ///
    /// Ensures: Kernel's definitions and every root namespace name are visible to
    ///   chunks evaluated in the result.
    pub fn new(ev: &Evaluator) -> Self {
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        Self { env }
    }

    /// The name-keyed environment top-level code currently evaluates in.
    pub(crate) fn env_mut(&mut self) -> &mut Env {
        &mut self.env
    }
}
