pub use wasmparser::Data;
#[derive(Debug, Default)]
pub struct DataSection<'a> {
    pub data_segments: Vec<Data<'a>>,
}
