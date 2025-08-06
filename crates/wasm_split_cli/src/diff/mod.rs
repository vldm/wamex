//! Find a difference between two wasm modules.
//!

use colored::Colorize;
use similar::{ChangeTag, TextDiff};
use wasmparser::{Data, Global};

use crate::{
    analysis,
    index::{Id, IdVec, Indexed},
    read::{self, code::FunctionWithBody},
};

pub struct Compare<'any, 'src> {
    left: &'any read::InputModule<'src>,
    right: &'any read::InputModule<'src>,
}

impl<'any, 'src> Compare<'any, 'src> {
    pub fn new(left: &'any read::InputModule<'src>, right: &'any read::InputModule<'src>) -> Self {
        Self { left, right }
    }

    /// Compare two modules and return a list of differences.
    pub fn print_diff(&self) -> Result<(), anyhow::Error> {
        macro_rules! print_hex_diff {
            ($left: expr, $right: expr) => {
                match ($left, $right) {
                    (Some(left), Some(right)) => {
                        let left = hex::encode(left.body.as_bytes());
                        let right = hex::encode(right.body.as_bytes());
                        print_inline_diff(&left, &right);
                    }
                    _ => {}
                }
            };
        }
        macro_rules! print_elements {
            ($left: expr, $right: expr) => {
                if let Some(elem) = $left {
                    log::info!("Left item {}", elem.debug(),);
                }
                if let Some(elem) = $right {
                    log::info!("Right item {}", elem.debug(),);
                }
            };
        }
        macro_rules! print_compare_section {

            ($($path:ident).+) => {
                print_compare_section!(print_elements, $($path).+);
            };
            ($v: ident, $($path:ident).+) => {
                let res = Self::compare_vec(&self.left.$($path).+, &self.right.$($path).+);
                if res.is_empty() {
                    log::info!("No differences in {} found", stringify!($($path).+));
                }
                for (id, err) in res {
                    let left = self.left.$($path).+.get(id);
                    let right = self.right.$($path).+.get(id);

                    log::error!(
                        "Section {} differ at index: {} - {}",
                        stringify!($($path).+),
                        id,
                        err
                    );

                    $v!(left, right);
                }
            };
        }
        print_compare_section!(types);
        print_compare_section!(imports);
        print_compare_section!(exports);
        print_compare_section!(tables);
        print_compare_section!(elements);
        print_compare_section!(tags);
        print_compare_section!(globals);
        print_compare_section!(memories);

        self.print_compare_data();
        // self.left.code.defined_funcs.get(0).unwrap().body.as_bytes()
        print_compare_section!(print_hex_diff, code.defined_funcs);
        // data
        // custom sections (names, linking, relocations, target_features, ...)

        Ok(())
    }
    fn print_compare_data(&self) {
        let mut errors = Vec::new();
        let left = &self.left.data.data_segments;
        let right = &self.right.data.data_segments;
        let mut left_iter = left.iter().enumerate();
        let mut right_iter = right.iter().enumerate();
        for ((left_id, left), (_, right)) in (&mut left_iter).zip(&mut right_iter) {
            if let Err(e) = left.compare(right).map_err(|err| (left_id, err)) {
                errors.push(e);
            };
        }
        // If one of the iterators is longer, we have extra items in one of the sections.
        for (right_id, right) in right_iter {
            errors.push((
                right_id,
                anyhow::anyhow!("Extra item in right: {:?}", right.debug()),
            ));
        }
        for (left_id, left) in left_iter {
            errors.push((
                left_id,
                anyhow::anyhow!("Extra item in left: {:?}", left.debug()),
            ));
        }
        let res = errors;

        let process = |left: Option<&Data<'_>>, right: Option<&Data<'_>>| match (left, right) {
            (Some(left), Some(right)) => {
                let left = hex::encode(left.data);
                let right = hex::encode(right.data);
                print_inline_diff(&left, &right);
            }
            _ => {}
        };
        if res.is_empty() {
            log::info!("No differences in {} found", stringify!(data.data_segments));
        }
        for (id, err) in res {
            let left = self.left.data.data_segments.get(id);
            let right = self.right.data.data_segments.get(id);

            process(left, right);
            log::error!(
                "Section {} differ at index: {} - {}",
                stringify!(data.data_segments),
                id,
                err
            );
        }
    }

    fn compare_vec<Type>(
        left: &IdVec<Type, Id<<Type as Indexed>::StaticTypeTagForIndex>>,
        right: &IdVec<Type, Id<<Type as Indexed>::StaticTypeTagForIndex>>,
    ) -> Vec<(Id<<Type as Indexed>::StaticTypeTagForIndex>, anyhow::Error)>
    where
        Type: Indexed + DiffExt,
    {
        let mut errors = Vec::new();
        let mut left_iter = left.iter();
        let mut right_iter = right.iter();
        for ((left_id, left), (_, right)) in (&mut left_iter).zip(&mut right_iter) {
            if let Err(e) = left.compare(right).map_err(|err| (left_id, err)) {
                errors.push(e);
            };
        }
        // If one of the iterators is longer, we have extra items in one of the sections.
        for (right_id, right) in right_iter {
            errors.push((
                right_id,
                anyhow::anyhow!("Extra item in right: {:?}", right.debug()),
            ));
        }
        for (left_id, left) in left_iter {
            errors.push((
                left_id,
                anyhow::anyhow!("Extra item in left: {:?}", left.debug()),
            ));
        }
        errors
    }
}

fn print_inline_diff(a: &str, b: &str) {
    let diff = TextDiff::configure()
        .algorithm(similar::Algorithm::Myers)
        .diff_chars(a, b);

    for change in diff.iter_all_changes() {
        let change_str = change.as_str().unwrap();
        match change.tag() {
            ChangeTag::Equal => {
                // Print unchanged bytes
                print!("{}", change_str)
            }
            ChangeTag::Delete => {
                print!("{}", change_str.red())
            }
            ChangeTag::Insert => {
                print!("{}", change_str.green())
            }
        }
    }
    println!(); // Newline after the diff
}

trait DiffExt {
    fn compare(&self, other: &Self) -> Result<(), anyhow::Error>;
    fn debug(&self) -> String;
}

macro_rules! impl_cmp_from_eq {
    ( $($t:ty),* ) => {
        $(impl DiffExt for $t {
            fn compare(&self, other: &Self) -> Result<(), anyhow::Error> {
                if self == other {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("Mismatched {}: {:?} != {:?}", stringify!($t), self, other))
                }
            }
            fn debug(&self) -> String {
                format!("{:?}", self)
            }
        })*
    };
}

impl_cmp_from_eq!(
    wasmparser::Import<'_>,
    wasmparser::Export<'_>,
    wasmparser::FuncType,
    wasmparser::TagType,
    wasmparser::MemoryType
);

impl DiffExt for wasmparser::Data<'_> {
    fn compare(&self, other: &Self) -> Result<(), anyhow::Error> {
        let kind_eq = match (&self.kind, &other.kind) {
            (
                wasmparser::DataKind::Active {
                    memory_index,
                    offset_expr,
                },
                wasmparser::DataKind::Active {
                    memory_index: other_memory_index,
                    offset_expr: other_offset_expr,
                },
            ) => memory_index == other_memory_index && offset_expr == other_offset_expr,
            (wasmparser::DataKind::Passive, wasmparser::DataKind::Passive) => true,
            _ => false,
        };
        if !kind_eq {
            return Err(anyhow::anyhow!(
                "Mismatched Data kind: {:?} != {:?}",
                self.kind,
                other.kind
            ));
        }
        if self.data != other.data {
            return Err(anyhow::anyhow!("Mismatched Data"));
        }
        Ok(())
    }
    fn debug(&self) -> String {
        format!(
            "Data {{ data: {}, kind: {:?} }}",
            hex::encode(self.data),
            self.kind
        )
    }
}

impl DiffExt for FunctionWithBody<'_> {
    fn compare(&self, other: &Self) -> Result<(), anyhow::Error> {
        if self.body.as_bytes() == other.body.as_bytes() && self.type_id == other.type_id {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Mismatched Function"))
        }
    }
    fn debug(&self) -> String {
        format!(
            "FunctionWithBody {{ ty: {:?}, body: {} }}",
            self.type_id,
            hex::encode(self.body.as_bytes())
        )
    }
}

impl DiffExt for Global<'_> {
    fn compare(&self, other: &Self) -> Result<(), anyhow::Error> {
        if self.init_expr == other.init_expr && self.ty == other.ty {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Mismatched Global: {:?} != {:?}",
                self,
                other
            ))
        }
    }
    fn debug(&self) -> String {
        format!(
            "Global {{ ty: {:?}, init_expr: {:?} }}",
            self.ty, self.init_expr
        )
    }
}

impl DiffExt for wasmparser::Table<'_> {
    fn compare(&self, other: &Self) -> Result<(), anyhow::Error> {
        if self.ty != other.ty {
            return Err(anyhow::anyhow!(
                "Mismatched Table: {:?} != {:?}",
                self,
                other
            ));
        }
        let init_same = match (&self.init, &other.init) {
            (wasmparser::TableInit::RefNull, wasmparser::TableInit::RefNull) => true,
            (wasmparser::TableInit::Expr(expr1), wasmparser::TableInit::Expr(expr2)) => {
                expr1 == expr2
            }
            _ => false,
        };
        if !init_same {
            return Err(anyhow::anyhow!(
                "Mismatched Table init: {:?} != {:?}",
                self.init,
                other.init
            ));
        }
        Ok(())
    }
    fn debug(&self) -> String {
        format!("Table {{ ty: {:?}, init: {:?} }}", self.ty, self.init)
    }
}
impl DiffExt for wasmparser::Element<'_> {
    fn compare(&self, other: &Self) -> Result<(), anyhow::Error> {
        let kind_same = match (&self.kind, &other.kind) {
            (
                wasmparser::ElementKind::Active {
                    table_index,
                    offset_expr,
                },
                wasmparser::ElementKind::Active {
                    table_index: other_table_index,
                    offset_expr: other_offset_expr,
                },
            ) => table_index == other_table_index && offset_expr == other_offset_expr,
            (wasmparser::ElementKind::Passive, wasmparser::ElementKind::Passive)
            | (wasmparser::ElementKind::Declared, wasmparser::ElementKind::Declared) => true,
            _ => false,
        };
        if !kind_same {
            return Err(anyhow::anyhow!("Mismatched Element kind"));
        }
        let items_same = match (&self.items, &other.items) {
            (
                wasmparser::ElementItems::Functions(funcs),
                wasmparser::ElementItems::Functions(other_funcs),
            ) => funcs.range() == other_funcs.range(),
            (
                wasmparser::ElementItems::Expressions(exprs, reader),
                wasmparser::ElementItems::Expressions(other_exprs, other_reader),
            ) => exprs == other_exprs && reader.range() == other_reader.range(),
            _ => false,
        };
        if !items_same {
            return Err(anyhow::anyhow!("Mismatched Element items"));
        }
        Ok(())
    }
    fn debug(&self) -> String {
        let kind_type = match &self.kind {
            wasmparser::ElementKind::Active { .. } => "Active",
            wasmparser::ElementKind::Passive => "Passive",
            wasmparser::ElementKind::Declared => "Declared",
        };
        let items_type = match &self.items {
            wasmparser::ElementItems::Functions(_) => "Functions",
            wasmparser::ElementItems::Expressions(_, _) => "Expressions",
        };
        format!(
            "Element {{ kind: {:?}, items: {:?} }}",
            kind_type, items_type
        )
    }
}
