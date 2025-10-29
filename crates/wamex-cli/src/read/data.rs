pub use wasmparser::Data;

use crate::index::IdVec;
#[derive(Debug, Default)]
pub struct DataSection<'a> {
    pub data_segments: IdVec<Data<'a>>,
}
