#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmitConfig {
    pub main: ModuleConfig,
    pub sub: ModuleConfig,
    /// If `None`, skip generating shared modules and instead embed shared symbols into main module.
    pub shared: Option<ModuleConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleConfig {
    /// Memory mode to use.
    pub memory_mode: EmitMemoryMode,
    /// How to call functions in the emitted modules.
    pub call_abi: CallABI,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallABI {
    /// Try to `import` functions from env.
    /// Basically, this means that glue code should generate some wrappers for this functions,
    /// since other modules is not guaranteed to be loaded before instantiation of this module.
    ///
    // TODO: We can combine ABI, for main module keeping CallIndirect, with manual 'ensure' and for leaf modules use ImportExport.
    ImportExport,
    /// Use `call_indirect` to call functions.
    CallIndirect {
        /// Sets whether to share main __indirect_table or create a new isolated table for each sub-module.
        share_main_table: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitMemoryMode {
    ///
    /// Convert Symbols offsets usage to Global variables:
    /// - Create new import `lib_base` (aka GOT) that points at the start of data segments.
    /// - Replace all usage of data symbols from `i32.const` form to `global.get $VAR`
    ///     where `$VAR` is offset calculated at instantiation time based on `lib_base`.
    ///
    /// This is usefull in development, when you want to reload part of the code on demand.
    /// Checkout `emit::modify::constant_extraction` for implementation details.
    ConvertSymbolsToGlobals,
    /// Memory layout remain the same as in the original module.
    /// Data segments are cutted into pieces - places for sub modules memory are reserved.
    /// This is usefull in production, when only one thing you need is to load needed chunks on demand.
    KeepOffsets,
    /// Compact memory layout, and apply relocations to dependent symbols.
    Shrink,
}

impl Default for EmitConfig {
    fn default() -> Self {
        let default_config = ModuleConfig {
            memory_mode: EmitMemoryMode::ConvertSymbolsToGlobals,
            call_abi: CallABI::ImportExport,
        };
        Self {
            main: default_config,
            sub: default_config,
            shared: None,
        }
    }
}
