#[cfg(test)]
mod test_helpers;

#[cfg(test)]
#[path = "tests/kernel.rs"]
mod kernel_tests;

pub mod ast;
pub mod builtins;
pub(crate) mod builtins_iterative;
pub mod class;
pub mod class_registry;
pub mod entity;
pub mod env;
pub mod eval;
pub(crate) mod eval_iterative;
pub mod lexer;
pub mod namespace;
pub mod object;
pub mod os_interface;
pub mod parser;
pub mod shared_tx;
pub mod store_bridge;
pub mod traits;
pub mod value;
