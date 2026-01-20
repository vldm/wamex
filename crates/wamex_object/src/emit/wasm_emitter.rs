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

use std::io::Write;

use wasm_encoder::MemArg;

use crate::emit::modify::encode;

pub struct Encoder<W> {
    writer: W,
    offset: u32,
}

impl<W> Encoder<W>
where
    W: Write,
{
    pub fn new(writer: W, starting_offset: u32) -> Self {
        Encoder {
            writer,
            offset: starting_offset,
        }
    }

    fn push_byte(&mut self, byte: u8) -> Result<(), std::io::Error> {
        self.writer.write(&[byte])?;
        self.offset += 1;
        Ok(())
    }

    fn encode_leb_5byte(&mut self, v: u32) -> Result<u32, std::io::Error> {
        let mut buf = [0; 5];
        encode::encode_leb128_u32_5byte(v, &mut buf);
        let res = self.offset;
        self.writer.write(&buf)?;
        Ok(res)
    }

    fn encode_sleb_5byte(&mut self, v: i32) -> Result<u32, std::io::Error> {
        let mut buf = [0; 5];
        encode::encode_leb128_i32_5byte(v, &mut buf);
        let res = self.offset;
        self.writer.write(&buf)?;
        Ok(res)
    }

    /// Encode [`Instruction::GlobalGet`] and return offset of global_id start
    pub fn global_get(&mut self, g: u32) -> Result<u32, std::io::Error> {
        self.push_byte(0x23)?;
        self.encode_leb_5byte(g)
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
            return Ok(MemArgOffsets {
                offset,
                memory_index: None,
            });
        } else {
            let _ = self.encode_leb_5byte(m.align | (1 << 6))?;
            let idx = self.encode_leb_5byte(m.memory_index.try_into().unwrap())?;
            let offset = self.encode_leb_5byte(m.offset.try_into().unwrap())?;
            return Ok(MemArgOffsets {
                offset,
                memory_index: Some(idx),
            });
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
}

trait EncodeWithRelocOffset {
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

pub struct MemArgOffsets {
    pub offset: u32,
    pub memory_index: Option<u32>,
}
