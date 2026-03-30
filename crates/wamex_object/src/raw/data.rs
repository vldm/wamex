use cranelift_entity::PrimaryMap;
pub use wasmparser::Data;

use crate::raw::SegmentId;
#[derive(Debug, Default)]
pub struct DataSection<'a> {
    pub data_segments: PrimaryMap<SegmentId, Data<'a>>,
}
