use wamex_object::linkage::reloc::Relative;

#[derive(Clone, Debug)]
pub struct HexdumpRow {
    pub offset: usize,
    pub bytes: Vec<(u8, Option<Relative>)>,
}

#[derive(Clone, Debug)]
pub struct RawBlockView {
    pub title: String,
    pub rows: Vec<HexdumpRow>,
}

pub fn plain_hexdump_rows(bytes: &[u8], base_offset: usize) -> Vec<HexdumpRow> {
    bytes
        .chunks(16)
        .enumerate()
        .map(|(row_idx, chunk)| HexdumpRow {
            offset: base_offset + row_idx * 16,
            bytes: chunk.iter().map(|byte| (*byte, None)).collect(),
        })
        .collect()
}
