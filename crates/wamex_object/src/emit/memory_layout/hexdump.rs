use std::{borrow::Cow, fmt::Write, ops::Range};

pub struct DataPart<'a> {
    // pub name: &'a str,
    pub bytes: &'a mut dyn Iterator<Item = u8>,
    pub refs: Vec<Ref<'a>>,
}

#[derive(Clone, Debug)]
pub struct Ref<'a> {
    pub range: Range<usize>,
    pub name: Cow<'a, str>,
}

/// Renders a hexdump of a DataPart with optional color highlighting for references.
/// part_base - initial offset of the part in the overall data segment.
/// color - whether to use ANSI color codes for highlighting.
/// part - the DataPart to render.
pub fn render_part(mut out: impl Write, part_base: usize, part: DataPart, color: bool) {
    // Collect bytes into a vector for indexing
    let bytes: Vec<u8> = part.bytes.collect();
    dbg!(&bytes);
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
        for &b in chunk {
            let ch = if (0x20..=0x7E).contains(&b) {
                b as char
            } else {
                '.'
            };
            write!(out, "{ch}").ok();
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

/// Returns (fg_code, bg_code)
fn palette(i: usize) -> (String, String) {
    let i = (i * 3) % 8;
    let bg = 40 + i;
    let fg_code = if i >= 5 { "30" } else { "97" }; // 30=black, 97=bright white
    (fg_code.to_string(), bg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn show_example() {
        let part = DataPart {
            bytes: &mut [
                0x01, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0xDE, 0xAD,
                0xBE, 0xEF, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x00, 0x00, 0x00, 0x34, 0x12, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22, 0x33, 0x44,
                0x55, 0x66, 0x77, 0x88, 0x99, 0x00, 0x00, 0x00, 0x00, 0x00, 0xAA, 0xBB, 0xCC, 0xDD,
                0xEE, 0xFF, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0x00,
            ]
            .iter()
            .copied(),
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
