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

    pub fn canonical_ids(self) -> &'static [SectionId] {
        match self {
            SectionKind::Types => &[1],
            SectionKind::Imports => &[2],
            SectionKind::Functions => &[3, 10],
            SectionKind::Tables => &[4],
            SectionKind::Memories => &[5],
            SectionKind::Globals => &[6],
            SectionKind::Exports => &[7],
            SectionKind::Start => &[8],
            SectionKind::Elements => &[9],
            SectionKind::Data => &[11, 12],
            SectionKind::Tags => &[13],
        }
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

    pub fn contains_section_id(self, section_id: SectionId) -> bool {
        self.canonical_ids().contains(&section_id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SectionDetailMode {
    Raw,
    StructuredShort,
    StructuredDetailed,
}

impl SectionDetailMode {
    pub const ALL: [SectionDetailMode; 3] = [
        SectionDetailMode::Raw,
        SectionDetailMode::StructuredShort,
        SectionDetailMode::StructuredDetailed,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SectionDetailMode::Raw => "Raw",
            SectionDetailMode::StructuredShort => "Structured short",
            SectionDetailMode::StructuredDetailed => "Structured detailed",
        }
    }

    pub fn next(self) -> Self {
        match self {
            SectionDetailMode::Raw => SectionDetailMode::StructuredShort,
            SectionDetailMode::StructuredShort => SectionDetailMode::StructuredDetailed,
            SectionDetailMode::StructuredDetailed => SectionDetailMode::Raw,
        }
    }

    pub fn tab_index(self) -> usize {
        match self {
            SectionDetailMode::Raw => 0,
            SectionDetailMode::StructuredShort => 1,
            SectionDetailMode::StructuredDetailed => 2,
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
