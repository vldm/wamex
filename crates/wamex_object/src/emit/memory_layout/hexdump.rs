use std::{fmt::Write, ops::Range};

use cranelift_entity::packed_option::ReservedValue;

use crate::{
    helpers::RangeExt,
    linkage::{file_db::FileRelocs, reloc::EntityRelocationEntry},
    typed::{EntityBody, Module, common_index::EntityKind, data::DataSymbolRef},
};

pub struct DataPart {
    // pub name: &'a str,
    pub bytes: Vec<u8>,
    pub refs: Vec<Ref>,
}

#[derive(Clone, Debug)]
pub struct Ref {
    pub range: Range<usize>,
    pub name: String,
}

/// Renders a hexdump of a DataPart with optional color highlighting for references.
/// part_base - initial offset of the part in the overall data segment.
/// color - whether to use ANSI color codes for highlighting.
/// part - the DataPart to render.
pub fn render_part(mut out: impl Write, part_base: usize, part: DataPart, color: bool) {
    // Collect bytes into a vector for indexing
    let bytes: Vec<u8> = part.bytes;
    // Mark bytes to ref index
    let mut byte_to_ref: Vec<Option<usize>> = vec![None; bytes.len()];
    for (idx, r) in part.refs.iter().enumerate() {
        for b in r.range.clone() {
            byte_to_ref[b] = Some(idx);
        }
    }

    let cols = 16;
    let hex_cell = 3; // "AB "
    let extra_gap_after = 8;

    for (line_idx, chunk) in bytes.chunks(cols).enumerate() {
        let line_abs_off = part_base + line_idx * cols;

        write!(out, "{line_abs_off:08X}  ").ok();

        for j in 0..cols {
            if j == extra_gap_after {
                write!(out, " ").ok();
            }
            if j < chunk.len() {
                let b = chunk[j];
                if color {
                    if let Some(ref_id) = byte_to_ref[line_idx * cols + j] {
                        let (fg, bg) = palette(ref_id);
                        write!(out, "\x1b[{fg};{bg}m{:02X}\x1b[0m ", b).ok();
                    } else {
                        write!(out, "{:02X} ", b).ok();
                    }
                } else {
                    write!(out, "{:02X} ", b).ok();
                }
            } else {
                write!(out, "   ").ok();
            }
        }

        write!(out, " |").ok();
        for (j, &b) in chunk.iter().enumerate() {
            let ch = if (0x20..=0x7E).contains(&b) {
                b as char
            } else {
                '·' // U+00B7 middle dot — ligature-safe
            };
            if color {
                if let Some(ref_id) = byte_to_ref[line_idx * cols + j] {
                    let (fg, bg) = palette(ref_id);
                    write!(out, "\x1b[{fg};{bg}m{ch}\x1b[0m").ok();
                } else {
                    write!(out, "{ch}").ok();
                }
            } else {
                write!(out, "{ch}").ok();
            }
        }
        writeln!(out, "|").ok();

        // If not color - print annotations bellow HEX
        if !color {
            let mut ann = vec![' '; cols * hex_cell + cols / extra_gap_after];
            let mut skip_annotation = true;
            for (local_idx, r) in part.refs.iter().enumerate() {
                let ln_start = line_idx * cols;
                let line = ln_start..ln_start + cols;

                if r.range.end <= line.start || r.range.start >= line.end {
                    continue; // No overlap
                }

                let seg = r.range.start.max(line.start)..r.range.end.min(line.end);
                let start_on_this_chunk = seg.start == r.range.start;
                let span = seg.end - seg.start;
                if span == 0 {
                    continue;
                }
                skip_annotation = false;

                for k in 0..span {
                    let j = (seg.start - line.start) + k;
                    let cell_start = j * hex_cell + if j >= extra_gap_after { 1 } else { 0 };
                    let cell = &mut ann[cell_start..cell_start + hex_cell];

                    let start = k == 0;
                    let end = k + 1 == span;

                    cell[0] = '─';
                    cell[1] = '─';
                    cell[2] = '─';
                    if start {
                        cell[0] = '└';
                    }
                    if end {
                        cell[2] = '┘';
                    }
                }

                if start_on_this_chunk {
                    let j0 = seg.start - line.start;
                    let cell_start = j0 * hex_cell + if j0 >= extra_gap_after { 1 } else { 0 };
                    let mut pos = cell_start + 1;

                    let label = if span == 1 {
                        format!("{}", local_idx + 1)
                    } else {
                        format!("[{}]", local_idx + 1)
                    };
                    for ch in label.chars() {
                        if pos >= ann.len() {
                            break;
                        }
                        if ann[pos] == '─' {
                            ann[pos] = ch;
                        }
                        pos += 1;
                    }
                }
            }
            if !skip_annotation {
                write!(out, "          ").ok(); // offset space
                writeln!(out, "{}", ann.iter().collect::<String>()).ok();
            }
        }
    }

    if !part.refs.is_empty() {
        writeln!(out, "  REFS (this part)").ok();
        for (i, r) in part.refs.iter().enumerate() {
            let a = r.range.start + part_base;
            let b = r.range.end + part_base;
            let index = if color {
                let (fg, bg) = palette(i);
                format!("\x1b[{fg};{bg}m{}\x1b[0m ", i + 1)
            } else {
                format!("{}", i + 1)
            };
            writeln!(
                out,
                "  [{}] {}   range=0x{a:04X}..0x{b:04X} ({})",
                index,
                r.name,
                r.range.end.saturating_sub(r.range.start)
            )
            .ok();
        }
    }

    writeln!(out).ok();
}

/// Returns (fg_code, bg_code) — high-contrast ANSI pairs, excluding default white-on-black.
fn palette(i: usize) -> (String, String) {
    // Hand-picked (fg, bg) pairs: every combination has strong contrast,
    // avoids default terminal colors (white on black), and adjacent indices
    // use distinct hues for easy visual discrimination.
    const PAIRS: &[(&str, &str)] = &[
        ("97", "41"),  // bright white on red
        ("30", "42"),  // black on green
        ("97", "44"),  // bright white on blue
        ("30", "43"),  // black on yellow
        ("97", "45"),  // bright white on magenta
        ("30", "46"),  // black on cyan
        ("30", "47"),  // black on white
        ("97", "101"), // bright white on bright red
        ("30", "102"), // black on bright green
        ("97", "104"), // bright white on bright blue
        ("30", "103"), // black on bright yellow
        ("30", "106"), // black on bright cyan
        ("97", "105"), // bright white on bright magenta
    ];
    let (fg, bg) = PAIRS[i % PAIRS.len()];
    (fg.to_string(), bg.to_string())
}

pub trait SymbolDebugExt {
    fn get_symbol_shifted_relocs(&self) -> Box<[EntityRelocationEntry]>;
    fn entity_name(&self, entity: EntityKind) -> String;
    fn bytes(&self) -> Vec<u8>;

    // return true if body should be skipped.
    fn print_header(&self, out: impl Write) -> bool;
    fn debug_symbol_ext(&self, mut out: impl Write, base: &mut usize, color: bool) {
        if self.print_header(&mut out) {
            *base += self.bytes().len();
            return;
        }

        let input_symbol = self.get_symbol_shifted_relocs();
        let refs = input_symbol
            .iter()
            .map(|reloc| {
                let id = reloc.symbol_id;
                let name = self.entity_name(id);
                Ref {
                    range: reloc.relocation_range(),
                    name,
                }
            })
            .collect();
        let bytes = self.bytes();
        let len = bytes.len();
        let part = DataPart { bytes, refs };
        render_part(&mut out, *base, part, color);
        *base += len;
    }
}

pub struct SymbolDebug<'a> {
    pub module: &'a Module<'a>,
    pub file_relocs: &'a FileRelocs,
    pub segment: &'a str,
    pub symbol_name: &'a str,
    pub symbol_index: DataSymbolRef,
    pub body: &'a EntityBody<'a>,
}

impl SymbolDebugExt for SymbolDebug<'_> {
    fn get_symbol_shifted_relocs(&self) -> Box<[EntityRelocationEntry]> {
        self.file_relocs
            .get_data_relocs(self.symbol_index)
            .unwrap_or_default()
            .iter()
            .map(|r| r.shift_left(self.body.original_range().start))
            .collect()
    }

    fn entity_name(&self, entity: EntityKind) -> String {
        self.module.get_name(entity).to_string()
    }
    fn bytes(&self) -> Vec<u8> {
        self.body.iter_bytes().collect()
    }

    fn print_header(&self, mut out: impl Write) -> bool {
        if self.symbol_index.is_reserved_value() {
            writeln!(
                out,
                "[{segment}] <padding> (size: {body_len})",
                segment = self.segment,
                body_len = self.body.len(),
            )
            .unwrap();
            return true;
        }
        writeln!(
            out,
            "[{segment}:{symbol_index}] {name}",
            segment = self.segment,
            symbol_index = self.symbol_index,
            name = self.symbol_name,
        )
        .unwrap();
        false
    }
}

pub struct SectionDebug<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
    pub relocs: &'a [EntityRelocationEntry],
}
impl SymbolDebugExt for SectionDebug<'_> {
    fn get_symbol_shifted_relocs(&self) -> Box<[EntityRelocationEntry]> {
        self.relocs.to_vec().into_boxed_slice()
    }

    fn entity_name(&self, entity: EntityKind) -> String {
        entity.to_string()
    }
    fn bytes(&self) -> Vec<u8> {
        self.bytes.to_vec()
    }

    fn print_header(&self, mut out: impl Write) -> bool {
        writeln!(
            out,
            "[{name}] (size: {size})",
            name = self.name,
            size = self.bytes.len()
        )
        .unwrap();
        false
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn show_example() {
        let part = DataPart {
            bytes: vec![
                0x01, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0xDE, 0xAD,
                0xBE, 0xEF, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x00, 0x00, 0x00, 0x34, 0x12, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22, 0x33, 0x44,
                0x55, 0x66, 0x77, 0x88, 0x99, 0x00, 0x00, 0x00, 0x00, 0x00, 0xAA, 0xBB, 0xCC, 0xDD,
                0xEE, 0xFF, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0x00,
            ],
            refs: vec![
                Ref {
                    range: 0..6,
                    name: ".Lanon.faeb22a22ed4190fdf8d8c764500d80d.47".into(),
                },
                Ref {
                    range: 8..21,
                    name: "very_very_long_human_readable_field_name".into(),
                },
                Ref {
                    range: 24..27,
                    name: ".Lanon.a91b73428f0e239f7d2e4cbd3eaa0011.02".into(),
                },
                Ref {
                    range: 46..47,
                    name: "tiny".into(),
                },
            ],
        };

        let mut res = String::new();
        render_part(&mut res, 0, part, false);
        println!("{res}");
    }
}
