//! Extra module - to be extracted.
//! Used to analyze wasm module dependencies based on relocation:
//! - Build dep graph
//! - Split modules based on dep graph

mod debug;
mod dep_graph;
mod split_point;
#[cfg(test)]
mod testing;
