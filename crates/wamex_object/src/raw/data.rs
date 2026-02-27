use cranelift_entity::PrimaryMap;
pub use wasmparser::Data;

use crate::raw::DataSegmentId;
#[derive(Debug, Default)]
pub struct DataSection<'a> {
    pub data_segments: PrimaryMap<DataSegmentId, Data<'a>>,
}
