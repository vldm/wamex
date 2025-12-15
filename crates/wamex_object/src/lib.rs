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
pub mod read;

pub use anyhow::Result;
pub use emit::split::{ModuleIdentifier, SplitModuleIdentifier, SplitProgramInfo};
pub use read::InputModule;
