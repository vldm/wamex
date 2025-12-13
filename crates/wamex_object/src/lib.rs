//! Shared WASM object model for WAMEX.
//!
//! This crate contains the low-level building blocks that were historically implemented in
//! `wamex-cli`: lossless module parsing (`read`), structural inspection (`analysis`), and
//! emission/relocation (`emit`).

use clap::ValueEnum;

pub mod analysis;
pub mod emit;
pub mod helpers;
pub mod index;
pub mod read;

pub use anyhow::Result;

pub use analysis::split_point::{ModuleIdentifier, SplitModuleIdentifier, SplitProgramInfo};
pub use read::InputModule;

/// Strategy for identifying split points in the input module.
///
/// Note: this is used both by `wamex-cli` (CLI flag) and by library callers.
#[derive(Debug, ValueEnum, Clone, Copy)]
pub enum SplitPointExtractor {
    /// Use regexp and `_wasm_split_` prefix to identify split points.
    Legacy,
    /// Use `__wamex_` prefix and `.start_with` instead of regexp.
    Wamex,
}
