/// The former per-activation name → value environment, now empty.
///
/// Every name is resolved before it runs: to a slot (`Local`), a block's capture
/// snapshot (`Upvalue`), the receiver (`SelfRef`), or a receiver variable or
/// namespace definition read live (`Dynamic`). Nothing binds or reads names here
/// any more; the type survives only as a parameter threaded through `Cont`
/// variants and handlers, which the env-frame redesign (P2.1d-d4) removes.
#[derive(Debug, Clone, Default)]
pub struct Env;

impl Env {
    pub fn new() -> Self {
        Env
    }
}
