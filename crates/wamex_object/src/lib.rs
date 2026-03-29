//! Shared WASM object model for WAMEX.
//!
//! This crate contains the low-level building blocks that were historically implemented in
//! `wamex-cli`: lossless module parsing (`read`), structural inspection (`analysis`), and
//! emission/relocation (`emit`).

#[macro_use]
pub mod index;

pub mod analysis;
pub mod emit;
pub mod helpers;
pub mod layouts;
pub mod linkage;
pub mod raw;
pub mod typed;

#[cfg(test)]
pub mod testfiles;

pub use anyhow::Result;
// pub use emit::split::{ModuleIdentifier, SplitModuleIdentifier, SplitProgramInfo};
pub use raw::ObjectReader;
// pub use symbols::Symbols;
// pub use symbols::StaticModuleInfo;

// SmallVec with default inline size of 4
type SVec<T, const N: usize = 4> = smallvec::SmallVec<[T; N]>;
