//! Shared WASM object model for WAMEX.
//!
//! This crate contains the low-level building blocks that were historically implemented in
//! `wamex-cli`: lossless module parsing (`read`), structural inspection (`analysis`), and
//! emission/relocation (`emit`).

#[macro_use]
pub mod index;

// pub mod emit;
pub mod helpers;
pub mod read;
pub mod symbols;

pub use anyhow::Result;
// pub use emit::split::{ModuleIdentifier, SplitModuleIdentifier, SplitProgramInfo};
pub use read::{Module, ObjectReader};
// pub use symbols::Symbols;
// pub use symbols::StaticModuleInfo;

// SmallVec with default inline size of 4
type SVec<T, const N: usize = 4> = smallvec::SmallVec<[T; N]>;
