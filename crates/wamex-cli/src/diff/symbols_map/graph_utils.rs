use std::hash::Hash;

use crate::{
    analysis::ModuleInfo,
    index::{DataSegmentId, DataSymbolId, InputFuncId},
    read::linking::SymbolIndex,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherReloc {
    pub relocation_type: wasmparser::RelocationType,
    pub id: u32,
    pub append: i64,
}

impl Hash for OtherReloc {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.relocation_type as u8).hash(state);
        // self.id.hash(state);
        self.append.hash(state);
    }
}

impl Ord for OtherReloc {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.relocation_type as u8)
            .cmp(&(other.relocation_type as u8))
            .then_with(|| self.id.cmp(&other.id))
            .then_with(|| self.append.cmp(&other.append))
    }
}
impl PartialOrd for OtherReloc {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// Extended Version of DepNode that includes simple symbols.
pub enum Child {
    Function(InputFuncId),
    DataSymbol {
        id: (DataSegmentId, DataSymbolId),
        offset: i64,
    },
    OtherReloc(OtherReloc),
}

impl Child {
    pub fn from_relocation(module: &ModuleInfo, entry: &wasmparser::RelocationEntry) -> Self {
        use wasmparser::RelocationType::*;
        match entry.ty {
                FunctionIndexI32 | FunctionIndexLeb => {
                    let SymbolIndex::Func(func_id) = module.source.linking.linking_symbols.original_indexes[entry.index as usize]
                    else {
                        panic!("Relocation type with wrong index information: {:?}", entry);
                    };
                    assert!(entry.addend == 0, "FunctionIndexI32 relocation should not have addend");
                    Child::Function(func_id)
                }
                MemoryAddrI32 | MemoryAddrLeb | MemoryAddrSleb => {
                    let SymbolIndex::DataDefined(segment, idx) = module.source.linking.linking_symbols.original_indexes[entry.index as usize]
                    else {
                        panic!("Relocation type with wrong index information: {:?}", entry);
                    };
                    Child::DataSymbol { id: (segment, idx), offset: entry.addend.try_into().unwrap() }
                }
                // Symbols that we don't handle as "dependencies" in dep graph
                GlobalIndexI32 | GlobalIndexLeb | TableIndexI32
                | TableIndexSleb | TableNumberLeb | TypeIndexLeb => {
                   Child::OtherReloc(OtherReloc {
                       relocation_type: entry.ty,
                       id: entry.index,
                       append: entry.addend,
                   })
                }
                // Offsets in function bodies
                FunctionOffsetI32 | FunctionOffsetI64 |
                // -fPIC not supported
                MemoryAddrRelSleb | MemoryAddrRelSleb64 | TableIndexRelSleb
                | TableIndexRelSleb64 | MemoryAddrLocrelI32 |
                // 64 bit not supported
                MemoryAddrLeb64 | MemoryAddrSleb64 | MemoryAddrI64 | TableIndexI64
                | TableIndexSleb64 | MemoryAddrTlsSleb64|
                // TLS not supported
                MemoryAddrTlsSleb |
                // TBD
                EventIndexLeb | SectionOffsetI32
                => {
                    panic!("Unsupported relocation type: {:?}", entry);
                }
            }
    }
}
