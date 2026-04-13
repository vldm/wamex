//! Convert absolute addresses in data to GOT accesses.
//!
//! Internally replace dirrect symbol reference with dynamically initialized data symbol in _start function.
//!

use anyhow::{Result, bail};
use smallvec::SmallVec;
use wasmparser::FuncType;

use crate::{
    SVec,
    emit::{
        modify::{
            Cursor, EntityRelocationEntry, Rewrite,
            abs_to_got::FixupFromRelocs,
            blacklist::{Blacklist, IsSet},
            wasm_emitter::Encoder,
        },
        relocation::{EntityLocation, resolver::OutputEntitiesResolver},
    },
    index::Temp,
    layouts::DataSymbolRef,
    linkage::reloc::{Encoding, Relative, RelocationWidth},
    typed::{
        DefinedFunction, EntityBody, EntityKind, ExportNames, FileId, FunctionRef,
        snapshot::EntitiesSnapshot,
    },
};

//TODO: Add got for data symbol.
pub type FinalDataSymbolInit = DataSymbolInit<DataSymbolRef, EntityKind>;

pub struct DataSymbolInit<S = Temp<DataSymbolRef>, RelocSymbol = EntityLocation> {
    // Where to store address.
    // DataSymbolRef in output module space.
    pub storage: S,
    // What to store.
    // Symbol in input module space.
    pub relocated_symbol: RelocSymbol,

    /// Whenever `relocated_symbol` is not in main module.
    /// And therefore need GOT address.
    pub is_got_based: bool,
}

#[derive(derive_more::Debug)]
pub struct DataAbsToGot<F>
where
    Blacklist<F>: IsSet,
{
    // Symbols (in input space) that need to be always treated as static (not converted to GOT-relative)
    #[debug("is_static_symbol: <function>")]
    pub static_symbols: Blacklist<F>,
    pub start_fn: Temp<FunctionRef>,
}

impl<F> DataAbsToGot<F>
where
    Blacklist<F>: IsSet,
{
    pub fn is_dyn_symbol(&self, input_snapshot: &EntitiesSnapshot, sym: &EntityKind) -> bool {
        let sym = input_snapshot.pack_ref(*sym);
        // 1. For main - there should be no imported deps. (CodeRelocationHandler shouldn't be constructed for main module)
        // 2. for other modules - static symbols can be refered as-is, other should be converted to GOT-relative.
        !self.static_symbols.contains(sym)
    }

    pub fn fill_start_fn(
        &self,
        module: &mut crate::typed::Module,
        data_inits: Vec<FinalDataSymbolInit>,
        start_fn_id: FunctionRef,
    ) -> Result<(), anyhow::Error> {
        match &mut module
            .functions
            .get_entity_mut(start_fn_id)
            .to_defined()
            .unwrap()
        {
            DefinedFunction {
                body:
                    EntityBody::New {
                        new_relocs,
                        new_bytes,
                    },
                ..
            } => self.generate_start_fn(new_relocs, new_bytes, data_inits)?,
            _ => panic!("Start function should be defined with EntityBody::New"),
        }
        Ok(())
    }

    /// Generate relocations initializer.
    fn generate_start_fn(
        &self,
        new_relocs: &mut SVec<EntityRelocationEntry, 2>,
        new_bytes: &mut SVec<u8, 32>,
        resolved_inits: Vec<FinalDataSymbolInit>,
    ) -> Result<(), anyhow::Error> {
        const MEMORY_INDEX: u32 = 0; // TODO: Support multiple memories if needed.

        // Create fn header
        // todo: don't use wasm_encoder (it copies a lot)
        let func = wasm_encoder::Function::new([]);
        new_bytes.clear();
        new_bytes.extend(func.into_raw_body());
        let len = new_bytes.len() as u32;
        let mut encoder = Encoder::new(new_bytes, len);
        for data_entry in &resolved_inits {
            Self::push_init(&mut encoder, new_relocs, data_entry, MEMORY_INDEX)?;
        }
        encoder.end()?;
        Ok(())
    }

    fn push_init(
        encoder: &mut Encoder<&mut SVec<u8, 32>>,
        new_relocs: &mut SVec<EntityRelocationEntry, 2>,
        // Offset in data segment where to store the address of global var
        data_entry: &FinalDataSymbolInit,
        memory_index: u32,
    ) -> Result<(), anyhow::Error> {
        // store pointer in specific data symbol
        // value = (GOT+src) | src
        // *(GOT+dst) = value

        let got_offset = encoder.global_get_invalid()?;
        let storage_offset = encoder.i32_const_invalid()?;

        new_relocs.push(EntityRelocationEntry::index_base(
            got_offset,
            data_entry.storage.into(),
        ));
        new_relocs.push(EntityRelocationEntry::runtime_addr(
            storage_offset,
            data_entry.storage.into(),
            true, // got
        ));

        encoder.i32_add()?;

        if data_entry.is_got_based {
            let relocated_got = encoder.global_get_invalid()?;
            let relocated_addr = encoder.i32_const_invalid()?;

            new_relocs.push(EntityRelocationEntry::index_base(
                relocated_got,
                data_entry.relocated_symbol,
            ));
            new_relocs.push(EntityRelocationEntry::runtime_addr(
                relocated_addr,
                data_entry.relocated_symbol,
                true, // got
            ));
            encoder.i32_add()?;
        } else {
            let relocated_addr = encoder.i32_const_invalid()?;

            new_relocs.push(EntityRelocationEntry::runtime_addr(
                relocated_addr,
                data_entry.relocated_symbol,
                false, // not got
            ));
        };

        let _ = encoder.i32_store(wasm_encoder::MemArg {
            offset: 0,
            align: 2, // i32 - addr/index
            memory_index,
        })?;
        Ok(())
    }
    /// Convert temp ids to stable and resolve input `symbol_ids` to output ones.
    pub fn convert_to_stable_refs_and_resolve(
        module: &mut crate::typed::Module,
        module_info: &OutputEntitiesResolver,
        temps: Vec<DataSymbolInit>,
    ) -> Vec<FinalDataSymbolInit> {
        temps
            .into_iter()
            .map(|temp| {
                let relocated_symbol = module_info
                    .get_output_entity(&temp.relocated_symbol)
                    .expect("Relocated symbol be defined in output module at this point");
                let storage = module.extra.mem_layout.stable_id(temp.storage);
                FinalDataSymbolInit {
                    storage,
                    relocated_symbol,
                    is_got_based: temp.is_got_based,
                }
            })
            .collect()
    }
    fn new_entry(&self, _buffer: Cursor<'_>, entry: EntityRelocationEntry) -> Result<Rewrite> {
        debug_assert_eq!(entry.encoding, Encoding::Fixed);
        debug_assert_eq!(entry.relocation_range().len(), 4);

        Ok(Rewrite {
            old_range: entry.relocation_range(),
            // this will be emited in _start function with dyn relocation.
            new_relocs: SVec::new(),
            new_bytes: smallvec::smallvec![0xde, 0xad, 0xbe, 0xef], // dummy value, will be replaced with actual code in _start function
        })
    }

    fn check_whitelisted_data_relocation(entry: &EntityRelocationEntry) -> Result<()> {
        if matches!(entry.width, RelocationWidth::Bits64) {
            bail!("U64 memory pointers is currently not supported")
        }
        if !matches!(entry.encoding, Encoding::Fixed) {
            bail!("Only fixed encoding cannot be found in data segment")
        }
        if !matches!(entry.relation, Relative::None) {
            bail!("Relocation memory pointers is currently not supported")
        }
        Ok(())
    }
}

impl<'src, F> FixupFromRelocs<'src> for DataAbsToGot<F>
where
    Blacklist<F>: IsSet,
{
    type SetupData = Blacklist<F>;
    type ExtraData = DataSymbolInit;
    type EntityRef = DataSymbolRef;

    fn setup(
        shared: Self::SetupData,
        module: &mut crate::typed::ModuleBuilder<'src>,
    ) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        if !module.extra.has_got() {
            // No need to create handler if we won't convert any symbol to got-based.
            return Ok(None);
        }

        let start_fn = module.functions.push_defined(DefinedFunction {
            entity_type: FuncType::new([], []),
            body: EntityBody::New {
                new_relocs: SmallVec::new(),
                new_bytes: SmallVec::new(),
            },
            name: Some("__wamex_reloc_init".into()),
            export_as: ExportNames::default(),
        });
        module.extra.start_functions.push(start_fn);

        Ok(Some(Self {
            static_symbols: shared,
            start_fn,
        }))
    }

    fn finish(
        &self,
        module: &mut crate::typed::Module<'src>,
        resolver: &OutputEntitiesResolver,
        agregated_data: Vec<Self::ExtraData>,
    ) -> Result<()> {
        let final_data = Self::convert_to_stable_refs_and_resolve(module, resolver, agregated_data);
        self.fill_start_fn(
            module,
            final_data,
            module.functions.stable_id(self.start_fn),
        )
    }
    fn create_entry(
        &self,
        entity_ref: Temp<Self::EntityRef>,
        buffer: Cursor<'src>,
        (input_file, input_snapshot): (FileId, &EntitiesSnapshot),
        entry: EntityRelocationEntry,
    ) -> Result<Option<(Rewrite, Self::ExtraData)>> {
        match entry.symbol_id {
            EntityKind::Function(_) | EntityKind::DataSymbol(_) => {
                // TODO: move outside of this creation
                Self::check_whitelisted_data_relocation(&entry)?;
                let extra = DataSymbolInit {
                    storage: entity_ref,
                    relocated_symbol: EntityLocation::from_parts(input_file, entry.symbol_id),
                    is_got_based: self.is_dyn_symbol(input_snapshot, &entry.symbol_id),
                };
                return Ok(Some((self.new_entry(buffer, entry)?, extra)));
            }
            _ => {}
        }

        Ok(None)
    }
}
