pub use wamex_object::{
    Module,
    symbols::{StaticModuleInfo, Symbols},
};

pub mod debug;
pub mod dep_graph;
pub mod split_point;
#[cfg(test)]
pub mod testing;

pub mod symbols {
    pub use wamex_object::symbols::*;
}
