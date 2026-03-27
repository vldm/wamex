//!
//! A glue between split-points and actual emitting.
//!
//! Contain a code required to convert SplitProgramInfo into copy plan + modifications.
//!

use cranelift_entity::{EntityRef, PrimaryMap};

use crate::{
    analysis::SplitProgramInfo,
    emit::plan::{
        AddressingMode, CopySpec, EmitContext, GotInfo, ImportSpec, OutputModuleCopyPlan,
        PlannedGotInfo,
    },
    typed::{FileId, FileLoader},
};

impl SplitProgramInfo {
    pub fn into_emit_context<'src>(&self, input_files: &'src FileLoader) -> EmitContext<'src> {
        todo - !();
        // todo: implement main routine - convert split point import fn to defined with indirect fn layout
        // TODO: mark __stack_pointer and __indirect_function_table
        // as exported
        let snapshot = input_files.get_snapshot();
        let outputs = self
            .output_modules
            .iter()
            .map(|(id, info)| {
                let entities = info
                    .defined_symbols
                    .iter()
                    .map(|sym| {
                        (
                            *sym,
                            if info.exports.contains(sym) {
                                let loc = snapshot.unpack_ref(*sym);
                                let module = &input_files.get_file(loc.file_id).module;
                                let name = module.get_name(loc.entity).to_string();
                                CopySpec::wamex_export(name)
                            } else {
                                CopySpec::AsIs
                            },
                        )
                    })
                    .collect();

                // Add imports with original names
                let mut imports = PrimaryMap::new();
                for imported in &info.imports {
                    let loc = snapshot.unpack_ref(*imported);
                    let module = &input_files.get_file(loc.file_id).module;
                    let name = module.get_name(loc.entity).to_string();
                    let Some(ty) = module.get_type(loc.entity) else {
                        continue;
                    };
                    imports.push(ImportSpec::wamex_import(name, ty, *imported));
                }

                let addressing = if id.is_main() {
                    AddressingMode::Static
                } else {
                    let our_got = GotInfo {
                        memory_base: imports.push(ImportSpec::memory_base("")),
                        table_base: imports.push(ImportSpec::table_base("")),
                    };
                    let deps = info
                        .dependencies
                        .keys()
                        .map(|id| {
                            let idx = self
                                .output_modules
                                .iter()
                                .position(|(module_id, _)| module_id == id)
                                .expect("Module from dependencies should be in output modules");
                            let file_id = FileId::from_u32(idx as u32);
                            let got_info = GotInfo {
                                memory_base: imports.push(ImportSpec::memory_base(&id.to_string())),
                                table_base: imports.push(ImportSpec::table_base(&id.to_string())),
                            };
                            (file_id, got_info)
                        })
                        .collect();

                    let planned_deps = PlannedGotInfo { our_got, deps };
                    AddressingMode::GotRelative(planned_deps)
                };

                (
                    id.to_string(),
                    OutputModuleCopyPlan {
                        entities,
                        imports,
                        addressing,
                    },
                )
            })
            .collect();

        EmitContext::new_plan(
            input_files,
            outputs,
            self.symbol_output_module
                .iter()
                .map(|(sym, module_idx)| (sym, FileId::new(*module_idx)))
                .collect(),
        )
    }
}
