/// Re-export shim: `builtins` is the iterative dispatch module.
/// All production code that uses `crate::builtins::*` will transparently
/// resolve to `crate::builtins_iterative::*`.  This shim will be removed
/// once all callers have been updated to name the module directly.
pub use crate::builtins_iterative::*;
