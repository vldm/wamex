pub use wamex_link::{load, Result};
pub use wamex_macro::split;
pub use wamex_types::{BumpVersion, ModuleId};

mod loader_combinator;
pub use loader_combinator::{unsafe_fn, WamexLoadRunner};
