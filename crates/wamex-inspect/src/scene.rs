use wamex_object::{index::SectionId, layouts::DataSymbolRef, typed::FunctionRef};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SectionKind {
    Types,
    Imports,
    Functions,
    Tables,
    Memories,
    Globals,
    Exports,
    Start,
    Elements,
    Data,
    Tags,
}

impl SectionKind {
    pub const ALL: [SectionKind; 11] = [
        SectionKind::Types,
        SectionKind::Imports,
        SectionKind::Functions,
        SectionKind::Tables,
        SectionKind::Memories,
        SectionKind::Globals,
        SectionKind::Exports,
        SectionKind::Start,
        SectionKind::Elements,
        SectionKind::Data,
        SectionKind::Tags,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SectionKind::Types => "Types",
            SectionKind::Imports => "Imports",
            SectionKind::Functions => "Functions",
            SectionKind::Tables => "Tables",
            SectionKind::Memories => "Memories",
            SectionKind::Globals => "Globals",
            SectionKind::Exports => "Exports",
            SectionKind::Start => "Start",
            SectionKind::Elements => "Elements",
            SectionKind::Data => "Data",
            SectionKind::Tags => "Tags",
        }
    }

    pub fn canonical_ids(self) -> SectionId {
        let x = match self {
            SectionKind::Types => wasm_encoder::SectionId::Type,
            SectionKind::Imports => wasm_encoder::SectionId::Import,
            SectionKind::Functions => wasm_encoder::SectionId::Function,
            SectionKind::Tables => wasm_encoder::SectionId::Table,
            SectionKind::Memories => wasm_encoder::SectionId::Memory,
            SectionKind::Globals => wasm_encoder::SectionId::Global,
            SectionKind::Exports => wasm_encoder::SectionId::Export,
            SectionKind::Start => wasm_encoder::SectionId::Start,
            SectionKind::Elements => wasm_encoder::SectionId::Element,
            SectionKind::Data => wasm_encoder::SectionId::Data,
            SectionKind::Tags => wasm_encoder::SectionId::Tag,
        };
        x as SectionId
    }

    pub fn canonical_label(self) -> &'static str {
        match self {
            SectionKind::Types => "01",
            SectionKind::Imports => "02",
            SectionKind::Functions => "03/10",
            SectionKind::Tables => "04",
            SectionKind::Memories => "05",
            SectionKind::Globals => "06",
            SectionKind::Exports => "07",
            SectionKind::Start => "08",
            SectionKind::Elements => "09",
            SectionKind::Data => "11/12",
            SectionKind::Tags => "13",
        }
    }

    pub fn from_section_id(section_id: SectionId) -> Option<Self> {
        SectionKind::ALL
            .into_iter()
            .find(|kind| kind.is_raw_eq(section_id))
    }

    pub fn is_raw_eq(self, section_id: SectionId) -> bool {
        match self {
            SectionKind::Functions => {
                section_id == self.canonical_ids()
                    || section_id == wasm_encoder::SectionId::Code as SectionId
            }
            SectionKind::Data => {
                section_id == self.canonical_ids()
                    || section_id == wasm_encoder::SectionId::DataCount as SectionId
            }
            _ => self.canonical_ids() == section_id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SectionDetailMode {
    Raw,
    Structured,
}

impl SectionDetailMode {
    pub const ALL: [SectionDetailMode; 2] = [SectionDetailMode::Raw, SectionDetailMode::Structured];

    pub fn title(self) -> &'static str {
        match self {
            SectionDetailMode::Raw => "Raw",
            SectionDetailMode::Structured => "Structured",
        }
    }

    pub fn next(self) -> Self {
        match self {
            SectionDetailMode::Raw => SectionDetailMode::Structured,
            SectionDetailMode::Structured => SectionDetailMode::Raw,
        }
    }

    pub fn tab_index(self) -> usize {
        match self {
            SectionDetailMode::Raw => 0,
            SectionDetailMode::Structured => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OverallViewMode {
    Raw,
    Structural,
}

impl OverallViewMode {
    pub const ALL: [OverallViewMode; 2] = [OverallViewMode::Raw, OverallViewMode::Structural];

    pub fn title(self) -> &'static str {
        match self {
            OverallViewMode::Raw => "Raw",
            OverallViewMode::Structural => "Structural",
        }
    }

    pub fn next(self) -> Self {
        match self {
            OverallViewMode::Raw => OverallViewMode::Structural,
            OverallViewMode::Structural => OverallViewMode::Raw,
        }
    }

    pub fn tab_index(self) -> usize {
        match self {
            OverallViewMode::Raw => 0,
            OverallViewMode::Structural => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InspectTarget {
    Function(FunctionRef),
    Data(DataSymbolRef),
}

impl InspectTarget {
    pub fn title(self) -> String {
        match self {
            InspectTarget::Function(func) => format!("func {}", func.as_u32()),
            InspectTarget::Data(data) => format!("data {}", data.as_u32()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scene {
    OverallView,
    SectionDetail(SectionKind),
}

impl Scene {
    pub fn tab_index(&self) -> usize {
        match self {
            Scene::OverallView => 0,
            Scene::SectionDetail(_) => 1,
        }
    }

    pub fn tab_title(&self) -> &'static str {
        match self {
            Scene::OverallView => "Overall",
            Scene::SectionDetail(_) => "Section",
        }
    }

    pub fn header_title(&self) -> String {
        match self {
            Scene::OverallView => "Overall view".to_owned(),
            Scene::SectionDetail(kind) => {
                format!("Section: [{}] {}", kind.canonical_label(), kind.title())
            }
        }
    }
}
