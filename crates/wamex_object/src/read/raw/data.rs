pub use wasmparser::Data;

use crate::index::IdVec2;
#[derive(Debug, Default)]
pub struct DataSection<'a> {
    pub data_segments: IdVec2<Data<'a>>,
}
