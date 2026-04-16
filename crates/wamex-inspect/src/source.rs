use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use wamex_object::{
    index::SectionId,
    layouts::{DataKind, ElementKind, SealedDataSegment, SealedElementSegment},
    raw::ObjectReader,
    typed::{FileId, FileLoader, FunctionRef, LoadedFile, Module},
};

use crate::scene::SectionKind;

// ─── Data symbols ────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct RawSectionBlock {
    pub section_index: usize,
    pub section_id: SectionId,
    pub name: String,
    pub range: Range<usize>,
    pub count: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct SectionSummary {
    pub kind: SectionKind,
    pub title: String,
    pub count: usize,
    pub note: String,
}

#[derive(Clone, Debug)]
pub struct StructuralRow {
    pub text: String,
    pub kind: Option<SectionKind>,
}

#[derive(Clone, Debug)]
pub struct RawSummary {
    pub file_size: usize,
    pub target_features: String,
    pub validation_error: Option<String>,
    pub structural_rows: Vec<SectionSummary>,
}

// ─── SourceFile ───────────────────────────────────────────────────────────────

pub struct SourceFile {
    pub path: PathBuf,
    pub bytes: Box<[u8]>,
    pub raw_sections: Vec<RawSectionBlock>,
    pub loader: FileLoader,
    pub file_id: FileId,
    pub summary: RawSummary,
    pub structural_overview: Vec<StructuralRow>,
}

impl SourceFile {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn summary(&self) -> &RawSummary {
        &self.summary
    }

    pub fn loaded(&self) -> &LoadedFile<'_> {
        self.loader.get_file(self.file_id)
    }
}

// ─── RawSummary construction ─────────────────────────────────────────────────

impl RawSummary {
    pub fn from_parts(
        loaded: &LoadedFile<'_>,
        validation_error: Option<String>,
        file_size: usize,
    ) -> Self {
        let module = &loaded.module;
        let raw = loaded.raw_reader();

        let data_count =
            module.extra.mem_layout.item_places().len() + module.extra.mem_layout.external().len();
        let structural_rows = vec![
            SectionSummary {
                kind: SectionKind::Types,
                title: "Types".to_owned(),
                count: raw.types.len(),
                note: format!("{} canonical function signatures", raw.types.len()),
            },
            SectionSummary {
                kind: SectionKind::Imports,
                title: "Imports".to_owned(),
                count: raw.imports.len(),
                note: format!("{} linker imports", raw.imports.len()),
            },
            SectionSummary {
                kind: SectionKind::Functions,
                title: "Functions".to_owned(),
                count: module.functions.len(),
                note: format!(
                    "{} imports, {} bodies",
                    module.functions.imports_iter().len(),
                    module.functions.defined_iter().len(),
                ),
            },
            SectionSummary {
                kind: SectionKind::Tables,
                title: "Tables".to_owned(),
                count: module.tables.len(),
                note: format!(
                    "{} imports, {} defined",
                    module.tables.imports_iter().len(),
                    module.tables.defined_iter().len(),
                ),
            },
            SectionSummary {
                kind: SectionKind::Memories,
                title: "Memories".to_owned(),
                count: module.memories.len(),
                note: format!(
                    "{} imports, {} defined",
                    module.memories.imports_iter().len(),
                    module.memories.defined_iter().len(),
                ),
            },
            SectionSummary {
                kind: SectionKind::Globals,
                title: "Globals".to_owned(),
                count: module.globals.len(),
                note: format!(
                    "{} imports, {} defined",
                    module.globals.imports_iter().len(),
                    module.globals.defined_iter().len(),
                ),
            },
            SectionSummary {
                kind: SectionKind::Exports,
                title: "Exports".to_owned(),
                count: raw.exports.len(),
                note: format!("{} exported entities", raw.exports.len()),
            },
            SectionSummary {
                kind: SectionKind::Start,
                title: "Start".to_owned(),
                count: usize::from(module.extra.start_function.is_some()),
                note: module.extra.start_function.map_or_else(
                    || "no start section".to_owned(),
                    |func_ref| format!("starts at func {}", func_ref.as_u32()),
                ),
            },
            SectionSummary {
                kind: SectionKind::Elements,
                title: "Elements".to_owned(),
                count: raw.elements.len(),
                note: format!("{} element segments", raw.elements.len()),
            },
            SectionSummary {
                kind: SectionKind::Data,
                title: "Data".to_owned(),
                count: data_count,
                note: format!(
                    "{} raw segments, {} data symbols",
                    raw.data.data_segments.len(),
                    data_count,
                ),
            },
            SectionSummary {
                kind: SectionKind::Tags,
                title: "Tags".to_owned(),
                count: module.tags.len(),
                note: format!(
                    "{} imports, {} defined",
                    module.tags.imports_iter().len(),
                    module.tags.defined_iter().len(),
                ),
            },
        ];

        Self {
            file_size,
            target_features: target_features_label(raw),
            validation_error,
            structural_rows,
        }
    }
}

// ─── Structural overview ─────────────────────────────────────────────────────

pub fn build_structural_overview(loaded: &LoadedFile<'_>) -> Vec<StructuralRow> {
    let module = &loaded.module;
    let mut rows = Vec::new();

    let entity_row = |label: &str,
                      imports: usize,
                      defined: usize,
                      exported: usize,
                      kind: SectionKind| StructuralRow {
        text: format!("{label:<10}  imports: {imports}  defined: {defined}  exported: {exported}"),
        kind: Some(kind),
    };

    rows.push(entity_row(
        "functions",
        module.functions.imports_iter().len(),
        module.functions.defined_iter().len(),
        module.functions.exports_iter().count(),
        SectionKind::Functions,
    ));
    rows.push(entity_row(
        "tables",
        module.tables.imports_iter().len(),
        module.tables.defined_iter().len(),
        module.tables.exports_iter().count(),
        SectionKind::Tables,
    ));
    rows.push(entity_row(
        "memories",
        module.memories.imports_iter().len(),
        module.memories.defined_iter().len(),
        module.memories.exports_iter().count(),
        SectionKind::Memories,
    ));
    rows.push(entity_row(
        "globals",
        module.globals.imports_iter().len(),
        module.globals.defined_iter().len(),
        module.globals.exports_iter().count(),
        SectionKind::Globals,
    ));
    rows.push(entity_row(
        "tags",
        module.tags.imports_iter().len(),
        module.tags.defined_iter().len(),
        module.tags.exports_iter().count(),
        SectionKind::Tags,
    ));

    // Data segments
    let segments = module.extra.mem_layout.segments();
    if !segments.is_empty() {
        rows.push(StructuralRow {
            text: String::new(),
            kind: None,
        });
        rows.push(StructuralRow {
            text: String::from("- Data segments:"),
            kind: None,
        });
        for (_seg_id, segment) in segments {
            rows.push(data_segment_row(segment, module));
        }
    }

    // Element segments
    let elem_segments = &module.extra.function_elements.segments;
    if !elem_segments.is_empty() {
        rows.push(StructuralRow {
            text: String::new(),
            kind: None,
        });
        rows.push(StructuralRow {
            text: String::from("- Element segments:"),
            kind: None,
        });
        for (_seg_id, segment) in elem_segments {
            rows.push(element_segment_row(segment, module));
        }
    }

    rows
}

fn element_segment_row(
    segment: &SealedElementSegment<'_, FunctionRef>,
    module: &Module<'_>,
) -> StructuralRow {
    let name = segment.name.as_ref();
    let name = if name.len() > 20 { &name[..20] } else { name };
    let mut table_name = String::new();
    let location = match &segment.kind {
        ElementKind::Active {
            table_ref,
            location,
        } => {
            table_name = module
                .tables
                .get_entity(*table_ref)
                .name()
                .map_or("<anon>".to_owned(), ToString::to_string);
            if let Some(base) = location.global_ref().map(|g| module.get_name(g.into())) {
                format!("va_offset: {base}+0x{:09x}", location.offset())
            } else {
                format!("va_offset: 0x{:09x}", location.offset())
            }
        }
        ElementKind::Passive => "passive".to_owned(),
        ElementKind::Declared => "declared".to_owned(),
    };
    let name = format!("{table_name}:{name}");
    StructuralRow {
        text: format!("{name:<40}  {location}  items: {}", segment.parts.len()),
        kind: Some(SectionKind::Elements),
    }
}
fn data_segment_row(segment: &SealedDataSegment<'_>, module: &Module<'_>) -> StructuralRow {
    let total_size: usize = segment
        .parts
        .values()
        .map(|item| item.defined_entity.body.len())
        .sum();
    let items = segment.parts.len();
    let name = segment.name.as_ref();
    let name = if name.len() > 20 { &name[..20] } else { name };
    let mut memory_name = String::new();
    let location = match &segment.va_address {
        DataKind::Active {
            location,
            memory_ref,
        } => {
            memory_name = module
                .memories
                .get_entity(*memory_ref)
                .name()
                .map_or("<anon>".to_owned(), ToString::to_string);
            if let Some(base) = location.global_ref().map(|g| module.get_name(g.into())) {
                format!("va_offset: {base}+0x{:09x}", location.offset())
            } else {
                format!("va_offset: 0x{:09x}", location.offset())
            }
        }
        DataKind::Passive => "passive".to_owned(),
    };
    let name = format!("{memory_name}:{name}");
    StructuralRow {
        text: format!(
            "{name:<25}  size: {:<30}  {location}  items: {}",
            format_size_len(total_size),
            items,
        ),
        kind: Some(SectionKind::Data),
    }
}

// ─── Collecting raw section headers ──────────────────────────────────────────

pub fn collect_raw_sections(raw: &ObjectReader<'_>) -> Vec<RawSectionBlock> {
    raw.section_headers
        .iter()
        .map(|header| {
            let mut range = header.content_range.clone();
            range.start = header.raw_start;
            raw_block(
                header.index,
                header.id,
                header.name.as_ref(),
                range,
                header.count,
            )
        })
        .collect()
}

fn raw_block(
    section_index: usize,
    section_id: SectionId,
    name: &str,
    range: Range<usize>,
    count: Option<usize>,
) -> RawSectionBlock {
    RawSectionBlock {
        section_index,
        section_id,
        name: name.to_owned(),
        range,
        count,
    }
}

// ─── Validation ──────────────────────────────────────────────────────────────

pub fn validate_wasm(bytes: &[u8]) -> Option<String> {
    wasmparser::Validator::new()
        .validate_all(bytes)
        .err()
        .map(|err| err.message().to_owned())
}

// ─── Format helpers ───────────────────────────────────────────────────────────

pub fn entity_name(name: Option<impl AsRef<str>>) -> String {
    name.map_or_else(|| "<anon>".to_owned(), |name| name.as_ref().to_owned())
}

pub fn format_size_len(bytes: usize) -> String {
    format!(
        "{:>10} ({})",
        format_size_units(bytes),
        format_hex_len(bytes)
    )
}

#[allow(clippy::cast_precision_loss)]
fn format_size_units(bytes: usize) -> String {
    if bytes >= 1 << 30 {
        format!("{:.2} GB", bytes as f64 / f64::from(1 << 30))
    } else if bytes >= 1 << 20 {
        format!("{:.2} MB", bytes as f64 / f64::from(1 << 20))
    } else if bytes >= 1 << 10 {
        format!("{:.2} KB", bytes as f64 / f64::from(1 << 10))
    } else {
        format!("{bytes} B")
    }
}

fn format_hex_len(bytes: usize) -> String {
    let width = format!("{bytes:x}").len().max(8);
    format!("0x{bytes:0width$x}")
}

pub fn global_type_label(global_type: wasmparser::GlobalType) -> String {
    let mutability = if global_type.mutable { "mut " } else { "" };
    let shared = if global_type.shared { "shared " } else { "" };
    format!(
        "{mutability}{shared}{}",
        val_type_label(global_type.content_type)
    )
}

pub fn table_type_label(table_type: &wasmparser::TableType) -> String {
    let limits = format!(
        "size: {}{}",
        table_type.initial,
        table_type
            .maximum
            .map_or_else(String::new, |max| format!("..={max}"))
    );
    let shared = if table_type.shared { "shared " } else { "" };
    let table64 = if table_type.table64 { "64-bit " } else { "" };
    format!("{shared}{table64}{limits}")
}

pub fn memory_type_label(mem_type: &wasmparser::MemoryType) -> String {
    let limits = format!(
        "size: {}{} pages",
        mem_type.initial,
        mem_type
            .maximum
            .map_or_else(String::new, |max| format!("..={max}"))
    );
    let page_size = if let Some(page_size) = mem_type.page_size_log2 {
        1 << page_size
    } else {
        64 * 1024 // default page size is 64KiB
    };
    let shared = if mem_type.shared { "shared " } else { "" };
    let mem64 = if mem_type.memory64 { "64-bit " } else { "" };
    format!("{shared}{mem64}{limits} (page size: {page_size} bytes)")
}

pub fn function_type_label(func_type: &wasmparser::FuncType) -> String {
    let params = func_type
        .params()
        .iter()
        .copied()
        .map(val_type_label)
        .collect::<Vec<_>>()
        .join(", ");
    let results = func_type
        .results()
        .iter()
        .copied()
        .map(val_type_label)
        .collect::<Vec<_>>()
        .join(", ");

    if results.is_empty() {
        format!("fn({params})")
    } else {
        format!("fn({params}) -> {results}")
    }
}

fn val_type_label(ty: wasmparser::ValType) -> String {
    format!("{ty:?}").to_lowercase()
}

fn target_features_label(raw: &ObjectReader<'_>) -> String {
    let features = &raw.target_features.features;
    let mut enabled = Vec::new();

    if features.mutable_global {
        enabled.push("mutable-globals");
    }
    if features.multi_value {
        enabled.push("multivalue");
    }
    if features.sign_extension {
        enabled.push("sign-ext");
    }
    if features.extended_const {
        enabled.push("extended-const");
    }
    if features.reference_types {
        enabled.push("reference-types");
    }
    if features.saturating_float_to_int {
        enabled.push("nontrapping-fptoint");
    }
    if features.bulk_memory {
        enabled.push("bulk-memory");
    }
    if features.bulk_memory_opt {
        enabled.push("bulk-memory-opt");
    }
    if features.call_indirect_overlong {
        enabled.push("call-indirect-overlong");
    }

    if enabled.is_empty() {
        "none".to_owned()
    } else {
        enabled.join(", ")
    }
}
