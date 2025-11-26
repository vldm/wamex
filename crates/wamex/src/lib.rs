pub use wamex_loader::{__wamex_mark_for_update as mark_for_update, load, Result};
pub use wamex_macro::split;
pub use wamex_types::{BumpVersion, ModuleId};

mod loader_combinator;
pub use loader_combinator::{
    load_and_execute, load_and_execute_sync, NonAsync, UnsafeFn, WamexLoadRunner,
};
