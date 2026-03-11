//!
//! Emitter for instructions.
//! Similar to `wasm_encoder::InstructionSink` but with relocation support.
//!
//! The support is limited and only contain two modifications:
//! - `Encoder` is generic over writer - can be used to write directly into file, or into fixed array.
//! - `Encoder` stores and returns "offset" of wasm entities arguments that was used as params.
//! - For leb and sleb encoding is done in full form (5 or 10 bytes) instead of variable-length.
//! - Unlike `InstructionSink` methods cannot be used in sequence as in builder.
//!

use std::{
    borrow::Borrow,
    fmt::Debug,
    io::{Cursor, Seek, SeekFrom, Write},
    ops::Range,
};

use wasm_encoder::{Encode, MemArg};

use crate::{
    emit::{
        memory_layout::{self, DataStream},
        relocation::encode,
    },
    typed::data::SpecificLocation,
};

/// Wrapper around Write/Seek that track written range, and position and
/// ensures that seek are done within written range.
#[derive(Debug)]
pub struct Encoder<W> {
    writer: W,
    offset: u32,
    written_range: Range<u32>,
}

impl<W> Encoder<W> {
    pub fn new(writer: W, starting_offset: u32) -> Self {
        Encoder {
            writer,
            offset: starting_offset,
            written_range: starting_offset..starting_offset,
        }
    }
    /// Returns start point for the `Encoder`.
    pub fn start(&self) -> u32 {
        self.written_range.start
    }
    /// Returns the current offset within the encoder.
    pub fn offset(&self) -> u32 {
        self.offset
    }
    /// Consume encoder and return the inner writer.
    pub fn into_inner(self) -> W {
        self.writer
    }
    /// Extend current written range with additional space,
    /// the additional_offset is added to the current offset.
    ///
    /// This call should be used with call to internal writer, to declare used range.
    fn extend_range(&mut self, additional_offset: u32) {
        self.offset += additional_offset;
        if self.offset > self.written_range.end {
            self.written_range.end = self.offset;
        }
    }

    /// Call a func with a child encoder, starting at the given offset and prevent going above it.
    pub fn with_child_encoder<F, U>(
        &mut self,
        starting_offset: u32,
        func: F,
    ) -> Result<U, std::io::Error>
    where
        F: FnOnce(&mut Encoder<&mut W>) -> Result<U, std::io::Error>,
    {
        if starting_offset < self.written_range.start || starting_offset > self.written_range.end {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Starting offset is out of written range",
            ));
        }

        let mut sliced_encoder = Encoder {
            writer: &mut self.writer,
            offset: starting_offset,
            written_range: starting_offset..starting_offset,
        };
        let result = func(&mut sliced_encoder)?;
        self.offset = sliced_encoder.offset;
        // After func call, we need to extend the main encoder's range with the sliced encoder's range.
        self.written_range.end = sliced_encoder.written_range.end;

        // and return back
        Ok(result)
    }
}

impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let res = self.writer.write(buf)?;
        self.extend_range(res as u32);
        Ok(res)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

impl<S: Seek> Seek for Encoder<S> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let checked_add = |pos: u32, offset: i64| -> std::io::Result<u32> {
            let err = || std::io::Error::new(std::io::ErrorKind::InvalidInput, "Seek overflow");
            (pos as i64)
                .checked_add(offset)
                .ok_or_else(err)?
                .try_into()
                .map_err(|_| err())
        };

        let new_offset = match pos {
            SeekFrom::Current(c) => checked_add(self.offset, c)?,
            SeekFrom::Start(s) => checked_add(self.written_range.start, s as i64)?,
            SeekFrom::End(e) => checked_add(self.written_range.end, e)?,
        };
        // offset should be in range, or at the end of written range (to allow appending)
        if new_offset < self.written_range.start || new_offset > self.written_range.end {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Seek out of written range",
            ));
        }

        self.offset = new_offset;
        let seek_pos = self.offset - self.written_range.start;
        self.writer.seek(SeekFrom::Start(seek_pos as u64))?;
        Ok(seek_pos as u64)
    }
}

impl<W> Encoder<W>
where
    W: Write,
{
    pub fn push_byte(&mut self, byte: u8) -> Result<(), std::io::Error> {
        self.write_all(&[byte])?;
        Ok(())
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), std::io::Error> {
        self.write_all(bytes)?;
        Ok(())
    }

    pub fn encode_leb_any_size(&mut self, v: u32) -> Result<(), std::io::Error> {
        let (buf, _) = leb128fmt::encode_u32(v).unwrap();
        self.push_bytes(&buf)?;
        Ok(())
    }

    pub fn encode_leb_5byte(&mut self, v: u32) -> Result<u32, std::io::Error> {
        let mut buf = [0; 5];
        encode::encode_leb128_u32_5byte(v, &mut buf);
        let res = self.offset;
        self.push_bytes(&buf)?;
        Ok(res)
    }

    pub fn encode_sleb_any_size(&mut self, v: i32) -> Result<(), std::io::Error> {
        let (buf, _) = leb128fmt::encode_s32(v).unwrap();
        self.push_bytes(&buf)?;
        Ok(())
    }

    pub fn encode_sleb_5byte(&mut self, v: i32) -> Result<u32, std::io::Error> {
        let mut buf = [0; 5];
        encode::encode_leb128_i32_5byte(v, &mut buf);
        let res = self.offset;
        self.push_bytes(&buf)?;
        Ok(res)
    }

    /// Encode constant with 5 byte len, reserved for sleb/leb encoding
    /// with invalid value `0xdeadbeef00`
    pub fn encode_5byte_invalid(&mut self) -> Result<u32, std::io::Error> {
        let mut buf = [0; 5];
        buf.copy_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0x00]);
        let res = self.offset;
        self.push_bytes(&buf)?;
        Ok(res)
    }

    // TODO: const expr can have multiple vals so we can only return Vec<u32>
    pub fn encode_const_expr(
        &mut self,
        expr: &wasm_encoder::ConstExpr,
    ) -> Result<(), std::io::Error>
    where
        W: Write,
    {
        let mut tmp_vec = Vec::new();
        wasm_encoder::Encode::encode(&expr, &mut tmp_vec);

        log::error!(
            "const_expr: {const_expr:?}",
            const_expr = hex::encode(&tmp_vec)
        );
        self.push_bytes(&tmp_vec)?;
        Ok(())
    }

    /// Encode length prefixed bytes.
    /// Return offset of bytes start.
    pub fn encode_len_prefixed_bytes(&mut self, bytes: &[u8]) -> Result<u32, std::io::Error> {
        let len = bytes.len() as u32;
        self.encode_leb_5byte(len)?;
        let pos = self.offset();
        self.push_bytes(bytes)?;
        Ok(pos)
    }

    /// Encode [`Instruction::GlobalGet`] and return offset of global_id start
    pub fn global_get(&mut self, g: u32) -> Result<u32, std::io::Error> {
        self.push_byte(0x23)?;
        self.encode_leb_5byte(g)
    }

    /// Encode [`Instruction::GlobalGet`] with 0xdeadbeef global_id and return offset of global_id start
    pub fn global_get_invalid(&mut self) -> Result<u32, std::io::Error> {
        self.push_byte(0x23)?;
        self.encode_5byte_invalid()
    }

    /// Encode [`Instruction::GlobalSet`] and return offset of global_id start
    pub fn global_set(&mut self, g: u32) -> Result<u32, std::io::Error> {
        self.push_byte(0x24)?;
        self.encode_leb_5byte(g)
    }

    /// Encode [`Instruction::I32Const`] and return offset of constant start
    pub fn i32_const(&mut self, c: i32) -> Result<u32, std::io::Error> {
        self.push_byte(0x41)?;
        self.encode_sleb_5byte(c)
    }

    /// Encode [`Instruction::I32Const`] with 0xdeadbeef value and return offset of constant start
    pub fn i32_const_invalid(&mut self) -> Result<u32, std::io::Error> {
        self.push_byte(0x41)?;
        self.encode_5byte_invalid()
    }

    /// Encode [`Instruction::I32Add`] and return offset of instruction start
    pub fn i32_add(&mut self) -> Result<(), std::io::Error> {
        self.push_byte(0x6a)?;
        Ok(())
    }

    /// Encode memarg, return offset to memory_index
    fn encode_memarg32(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        if m.memory_index == 0 {
            let _ = self.encode_leb_5byte(m.align)?;
            let offset = self.encode_leb_5byte(m.offset.try_into().unwrap())?;
            Ok(MemArgOffsets {
                offset,
                memory_index: None,
            })
        } else {
            let _ = self.encode_leb_5byte(m.align | (1 << 6))?;
            let idx = self.encode_leb_5byte(m.memory_index)?;
            let offset = self.encode_leb_5byte(m.offset.try_into().unwrap())?;
            Ok(MemArgOffsets {
                offset,
                memory_index: Some(idx),
            })
        }
    }

    /// Encode [`Instruction::I32Load`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i32_load(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x28)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I64Load`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i64_load(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x29)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::F32Load`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn f32_load(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x2A)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::F64Load`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn f64_load(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x2B)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I32Load8S`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i32_load8_s(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x2C)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I32Load8U`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i32_load8_u(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x2D)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I32Load16S`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i32_load16_s(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x2E)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I32Load16U`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i32_load16_u(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x2F)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I64Load8S`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i64_load8_s(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x30)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I64Load8U`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i64_load8_u(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x31)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I64Load16S`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i64_load16_s(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x32)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I64Load16U`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i64_load16_u(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x33)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I64Load32S`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i64_load32_s(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x34)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I64Load32U`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i64_load32_u(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x35)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I32Store`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i32_store(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x36)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I64Store`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i64_store(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x37)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::F32Store`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn f32_store(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x38)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::F64Store`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn f64_store(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x39)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I32Store8`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i32_store8(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x3A)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I32Store16`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i32_store16(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x3B)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I64Store8`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i64_store8(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x3C)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I64Store16`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i64_store16(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x3D)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::I64Store32`] and return offset to memory_index and offset to `memarg.offset`.
    pub fn i64_store32(&mut self, m: MemArg) -> Result<MemArgOffsets, std::io::Error> {
        self.push_byte(0x3E)?;
        self.encode_memarg32(m)
    }

    /// Encode [`Instruction::End`].
    pub fn end(&mut self) -> Result<(), std::io::Error> {
        self.push_byte(0x0B)
    }
}

pub trait EncodeWithRelocOffset {
    type Offsets;

    fn encode<W>(&self, encoder: &mut Encoder<W>) -> Result<Self::Offsets, std::io::Error>
    where
        W: Write;
}

impl EncodeWithRelocOffset for u32 {
    type Offsets = u32;
    fn encode<W>(&self, encoder: &mut Encoder<W>) -> Result<Self::Offsets, std::io::Error>
    where
        W: Write,
    {
        encoder.encode_leb_5byte(*self)
    }
}
#[derive(Debug)]
pub struct MemArgOffsets {
    pub offset: u32,
    pub memory_index: Option<u32>,
}

/// Wrapper for emitting list-like sections in streaming fashion.
///
/// Creating a new `SectionList` will push id and reserve place for <bytes_len> of section and <list_count>.
/// Calling `push_item` allow `SectionList` to count items,
/// and after calling `finish`, the `SectionList` will update <bytes_len> and <list_count> in the `Encoder` and return it.
#[derive(Debug)]
pub struct SectionList<W> {
    encoder: Encoder<W>,
    bytes_pos: u32,
    count_pos: u32,

    items_count: u32,
}

impl<W: Write> SectionList<W> {
    /// Create new section in the given writer.
    /// Reserve space for <bytes_len> and <list_count>.
    ///
    /// This constructor will consume `Encoder` and return a `SectionList` object.
    /// To return back `Encoder`, one should call `finish`.
    pub fn create_section(
        mut encoder: Encoder<W>,
        id: u8,
    ) -> Result<SectionList<W>, std::io::Error> {
        if encoder.offset != encoder.written_range.end {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Encoder should be at the end of written range",
            ));
        }
        encoder.push_byte(id)?;
        let start = encoder.start();
        let bytes_pos = encoder.encode_5byte_invalid()? - start;
        let start = encoder.start();
        let count_pos = encoder.encode_5byte_invalid()? - start;
        Ok(SectionList {
            encoder,
            bytes_pos,
            count_pos,
            items_count: 0,
        })
    }

    /// Finish section, update <bytes_len> and <list_count> and return back `Encoder`.
    pub fn finish(mut self) -> Result<Encoder<W>, std::io::Error>
    where
        W: Seek,
    {
        let bytes_len = self.encoder.written_range.end - self.count_pos;
        self.encoder.seek(SeekFrom::Start(self.bytes_pos as u64))?;
        self.encoder.encode_leb_5byte(bytes_len)?;
        self.encoder.seek(SeekFrom::Start(self.count_pos as u64))?;
        self.encoder.encode_leb_5byte(self.items_count)?;
        self.encoder.seek(SeekFrom::End(0))?;

        Ok(self.encoder)
    }

    /// Execute a function with a child encoder for the next item in the section.
    ///
    pub fn item_from_encoder<F, U>(&mut self, func: F) -> Result<U, std::io::Error>
    where
        F: FnOnce(&mut Encoder<&mut W>) -> Result<U, std::io::Error>,
    {
        self.items_count += 1;
        self.encoder.with_child_encoder(self.encoder.offset, func)
    }

    /// Return current position in section.
    /// Offset 0 is immediately after the id and size of the section
    pub fn pos_in_section(&self) -> u32 {
        self.encoder.offset() - self.count_pos // id, bytes_len
    }

    /// Push raw bytes with calculated length prefix to section as an item.
    ///
    /// Returns offset of item start related to the start of the section.
    pub fn raw_len_prefixed(&mut self, item: &[u8]) -> Result<u32, std::io::Error>
    where
        Encoder<W>: std::fmt::Debug,
    {
        self.item_from_encoder(|encoder| {
            encoder.encode_leb_5byte(item.len() as u32)?;
            let start_offset = encoder.offset(); // id, bytes_len
            encoder.push_bytes(item)?;
            Ok(start_offset)
        })
        .map(|offset| offset - self.count_pos)
    }
}

/// Adapter to wasm_encoder
#[derive(Debug)]
pub struct SectionAdapter {
    bytes: Vec<u8>,
}

impl wasm_encoder::Section for SectionAdapter {
    fn id(&self) -> u8 {
        self.bytes[0]
    }
    fn append_to(&self, dst: &mut Vec<u8>) {
        dst.extend_from_slice(&self.bytes);
    }
}

impl wasm_encoder::Encode for SectionAdapter {
    fn encode(&self, dst: &mut Vec<u8>) {
        // Encode requires writing section without id
        dst.extend_from_slice(&self.bytes[1..]);
    }
}

pub type MemWriter = Cursor<Vec<u8>>;
impl SectionAdapter {
    pub fn new<F>(id: u8, func: F) -> Result<Self, std::io::Error>
    where
        F: FnOnce(&mut SectionList<MemWriter>) -> Result<(), std::io::Error>,
    {
        let buf = Cursor::new(Vec::new());
        let encoder = Encoder::new(buf, 0);
        let section_list = SectionList::create_section(encoder, id)?;
        let mut section_list = section_list;
        log::error!(
            "section_list: {section_list:?}",
            section_list = DebugHexEncoder(&section_list.encoder)
        );
        func(&mut section_list)?;

        log::error!(
            "section_list: {section_list:?}",
            section_list = DebugHexEncoder(&section_list.encoder)
        );
        let encoder = section_list.finish()?;

        log::error!(
            "encoder after: {encoder:?}",
            encoder = DebugHexEncoder(&encoder)
        );
        Ok(SectionAdapter {
            bytes: encoder.into_inner().into_inner(),
        })
    }
}

pub fn data_segment_adapter<W>(
    encoder: &mut Encoder<W>,
    location: Option<SpecificLocation>,
    data_stream: DataStream<'_>,
) -> Result<(), std::io::Error>
where
    W: Write,
{
    // where segment:
    // - header (mode/offset)
    // - len of data
    // - data bytes

    let encoded = memory_layout::SegmentLayout::segment_header(location)?;
    encoder.push_bytes(&encoded)?;
    log::error!("data segment header: {encoded:02x?}");
    log::error!("data_stream: {data_stream:?}");
    data_stream.encode(encoder)?;
    Ok(())
}

struct DebugHexEncoder<'a, W>(&'a Encoder<W>);

impl<W> Debug for DebugHexEncoder<'_, Cursor<W>>
where
    W: AsRef<[u8]>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SectionList")
            .field("encoder", &hex::encode(self.0.writer.get_ref().as_ref()))
            .finish()
    }
}
#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn test_encoder() {
        let mut buf = Cursor::new(Vec::new());
        let mut encoder = Encoder::new(&mut buf, 5);

        assert_eq!(encoder.offset, 5);
        assert_eq!(encoder.written_range, 5..5);

        encoder.push_byte(0x01).unwrap();
        assert_eq!(encoder.offset, 6);
        assert_eq!(encoder.written_range, 5..6);

        encoder.push_bytes(&[0x02, 0x03, 0x04, 0x05]).unwrap();
        assert_eq!(encoder.offset, 10);
        assert_eq!(encoder.written_range, 5..10);

        encoder.seek(SeekFrom::Start(1)).unwrap();

        encoder.push_byte(0xFF).unwrap();

        assert_eq!(encoder.offset, 7); // 5 + 1 (seek) + 1 (push)
        assert_eq!(encoder.written_range, 5..10);

        // go to end
        encoder.seek(SeekFrom::End(0)).unwrap();

        encoder.seek(SeekFrom::Current(-2)).unwrap();

        encoder.push_byte(0xEE).unwrap();
        assert_eq!(encoder.offset, 9); //
        assert_eq!(encoder.written_range, 5..10);

        encoder.seek(SeekFrom::End(-1)).unwrap();

        encoder.push_byte(0xDD).unwrap();

        assert_eq!(encoder.offset, 10);
        assert_eq!(encoder.written_range, 5..10);

        // Check final buffer content
        assert_eq!(&buf.get_ref()[..], &[0x01, 0xFF, 0x03, 0xEE, 0xDD]);
    }

    #[test]
    fn test_section_list() {
        let mut buf = Cursor::new(Vec::new());
        let encoder = Encoder::new(&mut buf, 0);
        let mut section_list = SectionList::create_section(encoder, 0x12).unwrap();

        let offset = section_list.raw_len_prefixed(&[0x01, 0x02]).unwrap();

        assert_eq!(offset, 10); // 5 bytes for count + 5 bytes of item len
        let offset = section_list.raw_len_prefixed(&[0x03]).unwrap();
        assert_eq!(offset, 17); // 5 bytes for count + (5 + 2) bytes for previous item + 5 bytes of this item len
        section_list
            .item_from_encoder(|e| e.push_bytes(&[0x04, 0x05, 0x06]))
            .unwrap();

        let _ = section_list.finish().unwrap();

        assert_eq!(
            &buf.get_ref()[..],
            &[
                0x12, //section id
                0x95, 0x80, 0x80, 0x80, 0x00, // len
                0x83, 0x80, 0x80, 0x80, 0x00, // count
                0x82, 0x80, 0x80, 0x80, 0x00, // len of first item
                0x01, 0x02, // content of first item
                0x81, 0x80, 0x80, 0x80, 0x00, // len of second item
                0x03, // content of second item
                0x04, 0x05, 0x06 // content of raw third item
            ]
        );
    }
}
