//! Collects entities from input modules into an output module according to an OutputModulePlan.

use anyhow::Result;
use cranelift_entity::EntityRef;

use crate::{
    emit::{
        plan::{OutputModulePlan, AddressingMode, TransformPolicy},
        relocation::resolver::OutputEntitiesResolver,
        modify::{
            code_abs_to_got::{CodeAbsToGot, GotInfo},
            data_abs_to_got::DataAbsToGot,
        },
    },
    linkage::file_db::FileRelocs,
    typed::{
        Building, FileId, FileLoader, Module, common_index::EntitiesSnapshot,
    },
};

pub struct CollectedModule<'src> {
    pub module: Module<'src>,
    pub resolver: OutputEntitiesResolver,
    pub relocs: FileRelocs,
}

pub struct EntityCollector;

impl EntityCollector {
    /// Collect symbols from multiple input files according to plan, and apply required modifications.
    pub fn collect<'src>(
        plan: &OutputModulePlan,
        files: &'src FileLoader,
        snapshot: &EntitiesSnapshot,
    ) -> Result<CollectedModule<'src>> {
        // TODO: Move the actual copying loop from `create_split_module` here.
        // For now, this is a skeleton for the new architecture.
        
        let mut module: Module<'src, Building> = Module::new();
        let mut resolver = OutputEntitiesResolver::new();
        
        // Match addressing strategy
        match &plan.addressing {
            AddressingMode::Static => {
                // Direct linking mode
            }
            AddressingMode::GotRelative(got_config) => {
                // Apply CodeAbsToGot and DataAbsToGot
            }
        }
        
        // Iterate plan.entities over multiple files...
        
        // Resolve relocs...
        let relocs = FileRelocs::default();
        
        Ok(CollectedModule {
            module: module.into_locked(),
            resolver,
            relocs,
        })
    }
}
