use semdump::{DataPart, SemanticDump};

#[derive(Clone, Debug)]
pub struct RawBlockView {
    pub title: String,
    pub dump: SemanticDump<'static>,
}

pub fn plain_semantic_dump(bytes: &[u8], base_offset: usize) -> SemanticDump<'static> {
    let mut dump = SemanticDump::new(base_offset);
    dump.push_part(DataPart::from_bytes(bytes.to_vec()));
    dump
}
