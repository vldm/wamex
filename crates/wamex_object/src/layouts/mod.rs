//!
//! Layouts representation for data and elements.
//!
//! After creating module and filling it with elements and data entities,
//! we need to assign them to specific offsets in the file and (more importantly) in the virtual address space.
//! This addresses are used in instructions to require needed data chunk or element.
//!
//! The layout can be in two states:
//! - `Sealed` - when each of elements is assigned to a specific offset in the virtual address space.
//!   The bound is indirect and can be represented as tupple `(unit_id, segment_id, offset)`:
//!   1. The full layout is splitted into multiple units (`memory` for data and `table` for elements).
//!      Each unit has isolated address space.
//!   2. Each unit is splitted into segments, which are contiguous ranges of bytes/elements.
//!      The segment might have some constant starting offset, or can offset based on some expression
//!      (currently only got based expressions supported). Additionally, segment can be passive,
//!      which means that it doesn't have init offset, and all relocations should be done dynamically.
//! - `Builder` - when layout is represented as a seperate:
//!   1. list of segments and their information (placement, alignment, etc.)
//!   2. list of elements/data chunks and information about segment they belong to.
//!

use anyhow::{Result, bail, ensure};

// use elements::ElementItemId;
pub use self::data::*;
pub use self::elements::*;
use crate::typed::{FunctionRef, GlobalRef};
impl_entity_index! {
    #[display = "vs"]
    pub struct VirtualSpaceId;
    #[display = "item"]
    pub struct PartId;
}

pub mod data;
mod elements;
mod recover;
pub type IndirectFunctionsSealed<'src> = elements::ElementLayoutSealed<'src, FunctionRef>;
pub type IndirectFunctionsBuilder<'src> = elements::ElementLayoutBuilder<'src, FunctionRef>;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SegmentPlacement<GR = GlobalRef> {
    /// Place chunk at offset (starting from mem_start) in active memory, where offset is calculated as value of global + offset.
    GotBased { global: GR, offset: u32 },
    /// Place chunk at offset (starting from mem_start) in active memory.
    ConstantOffset(u32),
}

impl<GR> SegmentPlacement<GR>
where
    GR: Copy,
{
    pub fn offset(&self) -> u32 {
        match self {
            SegmentPlacement::GotBased { offset, .. } => *offset,
            SegmentPlacement::ConstantOffset(offset) => *offset,
        }
    }
    pub fn global_ref(&self) -> Option<GR> {
        match self {
            SegmentPlacement::GotBased { global, .. } => Some(*global),
            SegmentPlacement::ConstantOffset(_) => None,
        }
    }
    pub fn with_zero_offset(&self) -> Self {
        match self {
            SegmentPlacement::GotBased { global, .. } => SegmentPlacement::GotBased {
                global: *global,
                offset: 0,
            },
            SegmentPlacement::ConstantOffset(_) => SegmentPlacement::ConstantOffset(0),
        }
    }

    pub fn add_offset(&self, offset: u32) -> Self {
        match self {
            SegmentPlacement::GotBased {
                global,
                offset: base,
            } => SegmentPlacement::GotBased {
                global: *global,
                offset: base + offset,
            },
            SegmentPlacement::ConstantOffset(base) => {
                SegmentPlacement::ConstantOffset(base + offset)
            }
        }
    }
}

impl SegmentPlacement<GlobalRef> {
    /// Decode simple offset expressions:
    /// - `i32.const` for constant offsets
    /// - `global.get` for GOT based offsets
    /// - `global.get + i32.const` for GOT based offsets with constant offset.
    pub fn try_from_const_expr(offset_expr: &wasmparser::ConstExpr) -> Result<Self> {
        let mut reader = offset_expr.get_operators_reader();

        let mut offset = None;
        let mut got = None;

        match reader.read()? {
            wasmparser::Operator::I32Const { value } => {
                ensure!(
                    offset.is_none(),
                    "Too complex expression (more than one offset)"
                );
                offset = Some(value);
            }
            wasmparser::Operator::GlobalGet { global_index } => {
                ensure!(
                    got.is_none(),
                    "Too complex expression (more than one got reference)"
                );
                got = Some(GlobalRef::from_u32(global_index))
            }
            op => bail!(
                "Too complex expression found unexpected instruction: {:?}",
                op
            ),
        };
        if got.is_some() && offset.is_some() {
            match reader.read()? {
                wasmparser::Operator::I32Add => {}
                op => bail!(
                    "Too complex expression found expected I32Add found: {:?}",
                    op
                ),
            }
        }
        match reader.read()? {
            wasmparser::Operator::End => {}
            op => bail!("Expected End after const expr: {:?}", op),
        }
        let offset = offset.unwrap_or_default() as u32;
        Ok(match got {
            Some(global) => SegmentPlacement::GotBased { global, offset },
            None => SegmentPlacement::ConstantOffset(offset),
        })
    }
    pub fn to_init_expr(&self) -> wasm_encoder::ConstExpr {
        match self {
            SegmentPlacement::GotBased { global, offset } => {
                wasm_encoder::ConstExpr::global_get(global.as_u32())
                    .with_i32_const((*offset).try_into().unwrap())
                    .with_i32_add()
            }
            SegmentPlacement::ConstantOffset(base) => {
                wasm_encoder::ConstExpr::i32_const((*base).try_into().unwrap())
            }
        }
    }
}
