#[cfg(test)]
mod test_helpers;

#[cfg(test)]
#[path = "tests/kernel.rs"]
mod kernel_tests;

pub mod ast;
pub mod builtins;
pub(crate) mod builtins_iterative;
#[cfg(test)]
pub(crate) mod builtins_recursive;
pub mod class;
pub mod class_registry;
pub mod entity;
pub mod env;
pub mod eval;
pub(crate) mod eval_iterative;
#[cfg(test)]
pub(crate) mod eval_recursive;
pub mod lexer;
pub mod namespace;
pub mod object;
pub mod parser;
pub mod store_bridge;
pub mod value;
