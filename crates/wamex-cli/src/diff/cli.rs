//! Find a difference between two wasm modules.
//!

use std::collections::BTreeMap;

use colored::Colorize;
use similar::{ChangeTag, TextDiff};
use wasmparser::{Data, Global};

use crate::{
    analysis::{
        self,
        symbols::{DiffEntry, DiffResult},
    },
    index::{Id, IdVec, Indexed},
    read::code::FunctionWithBody,
};
pub struct Compare<'any, 'src> {
    left: &'any analysis::ModuleInfo<'src>,
    right: &'any analysis::ModuleInfo<'src>,
    structural: bool,
}

impl<'any, 'src> Compare<'any, 'src> {
    pub fn new(
        left: &'any analysis::ModuleInfo<'src>,
        right: &'any analysis::ModuleInfo<'src>,
        structural: bool,
    ) -> Self {
        Self {
            left,
            right,
            structural,
        }
    }

    /// Compare two modules and return a list of differences.
    pub fn print_diff(&self) -> Result<(), anyhow::Error> {
        macro_rules! print_hex_diff {
            ($id:expr, $left: expr, $right: expr) => {
                let num_imports = 27;
                let raw_id = $id.as_raw_index() + num_imports;
                let id = crate::index::Id::from_index(raw_id);
                log::info!("name: {}", self.left.wasm.names.functions[id]);
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
            ($id:expr, $left: expr, $right: expr) => {
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
                let res = Self::compare_vec(&self.left.wasm.$($path).+, &self.right.wasm.$($path).+);
                if res.is_empty() {
                    log::info!("No differences in {} found", stringify!($($path).+));
                }
                for (id, err) in res {
                    let left = self.left.wasm.$($path).+.get(id);
                    let right = self.right.wasm.$($path).+.get(id);

                    log::error!(
                        "Section {} differ at index: {} - {}",
                        stringify!($($path).+),
                        id,
                        err
                    );

                    $v!(id, left, right);
                }
            };
        }
        // TODO: Handle exports/imports index changes

        print_compare_section!(types);
        print_compare_section!(imports);
        print_compare_section!(exports);
        print_compare_section!(tables);
        print_compare_section!(elements);
        print_compare_section!(tags);
        print_compare_section!(globals);
        print_compare_section!(memories);

        if self.structural {
            let differ = crate::analysis::symbols::Differ::new(self.left, self.right);
            let map = differ.symbol_map();
            let diff_result = differ.build_diff(&map);
            differ.debug_diff(&diff_result);
            // TODO: rebuild structure using graph

            self.print_changed_modules(&differ, &diff_result)?;
            // let left_structure = Self::module_structure_from_module(self.left);
            // let right_structure = Self::module_structure_from_module(self.right);
            // let diff = left_structure.structure.diff(&right_structure.structure);

            // diff.debug();
            // Self::print_node_tree_changes(&left_structure, &right_structure, &diff);
            // Self::print_cascade_of_changes(&left_structure, &right_structure, &diff);
        } else {
            self.print_compare_data();

            // self.left.code.defined_funcs.get(0).unwrap().
            print_compare_section!(print_hex_diff, code.defined_funcs);
        }

        // data
        // custom sections (names, linking, relocations, target_features, ...)

        Ok(())
    }

    // Print info about split modules - which modules were changed.
    // Mark changed modules and print what caused the change.
    fn print_changed_modules(
        &self,
        differ: &crate::analysis::symbols::Differ<
            &'any analysis::ModuleInfo<'src>,
            &'any analysis::ModuleInfo<'src>,
        >,
        diff_result: &crate::analysis::symbols::DiffResult,
    ) -> Result<(), anyhow::Error> {
        let split_points = crate::analysis::split_point::find_split_points(
            &self.right,
            crate::SplitPointExtractor::Wamex,
        )?;
        let dep_graph = crate::analysis::dep_graph::get_dependencies(&self.right)?;
        let wbg_closures = crate::analysis::split_point::wbg_closures(&self.right, &dep_graph);
        let split_program_info = crate::analysis::split_point::compute_split_modules(
            &self.right,
            &dep_graph,
            &split_points,
            &wbg_closures,
        )?;

        let changed_deps = diff_result
            .entries()
            .filter_map(|entry| {
                let right = match entry {
                    DiffEntry::Added { right } => right,
                    DiffEntry::Replaced { right, .. } => right,
                    _ => {
                        return None;
                    }
                };
                Some((*right, entry.clone()))
            })
            .collect::<BTreeMap<_, _>>();

        let mut changed_modules = BTreeMap::new();
        for (module_name, split_deps) in split_program_info.output_modules.iter() {
            let changed_defined_symbols: DiffResult = split_deps
                .defined_symbols
                .iter()
                .filter_map(|sym_id| changed_deps.get(sym_id))
                .copied()
                .collect();
            if changed_defined_symbols.is_empty() {
                continue;
            }
            changed_modules.insert(module_name.clone(), changed_defined_symbols);
        }

        for (name, structure) in changed_modules.iter() {
            log::warn!("Changed module: {}", name);
            differ.debug_diff(&structure);
        }
        Ok(())
    }

    // // During diff calculation one changed node cause all its parents to be marked as changed.
    // // This function will print tree of changes
    // //  - starting from root nodes (the ones that are exported)
    // //  - going to bottom-most (leaf that actually was changed)
    // fn print_node_tree_changes(
    //     old_structure: &crate::diff::symbols_map::ModuleStructure,
    //     new_structure: &crate::diff::symbols_map::ModuleStructure,
    //     diff: &crate::diff::symbols_map::DiffResult,
    // ) {
    //     let mut added_nodes = BTreeSet::new();
    //     let mut removed_nodes = BTreeSet::new();

    //     for change in diff.all_changes() {
    //         if let Some(node) = &change.new {
    //             added_nodes.insert(node.node_id());
    //         };
    //         if let Some(node) = &change.old {
    //             removed_nodes.insert(node.node_id());
    //         };
    //     }
    //     let mut added_roots = BTreeSet::new();
    //     for node in added_nodes.iter() {
    //         // if no roots parents in changes - this is root of diff
    //         if !new_structure.structure.nodes[node]
    //             .parents
    //             .iter()
    //             .any(|p| added_nodes.contains(p))
    //         {
    //             added_roots.insert(*node);
    //         }
    //     }

    //     for id in added_roots.iter() {
    //         let node = &new_structure.module_nodes.get(*id).unwrap();
    //         log::info!("Root {} => {:?}", id, node);
    //     }
    //     let mut tree_builder = ptree::TreeBuilder::new("Changes tree".to_string());

    //     let tree = &mut tree_builder;
    //     for root in added_roots.iter() {
    //         Self::build_child_tree(root, &new_structure.structure, &added_nodes, tree);
    //     }
    //     // ptree::print_tree(&tree.build()).unwrap();

    //     log::warn!("Added nodes: {added_nodes:?}");
    //     log::warn!("Removed nodes: {removed_nodes:?}");
    //     log::warn!("Added roots: {added_roots:?}");
    // }

    // // Print tree of changes
    // // - starting from leafs (the one that was really changed)
    // // - going to top-most (should be root nodes - one that exported to world)
    // fn print_cascade_of_changes(
    //     old_structure: &crate::diff::symbols_map::ModuleStructure,
    //     new_structure: &crate::diff::symbols_map::ModuleStructure,
    //     diff: &crate::diff::symbols_map::DiffResult,
    // ) {
    //     let mut added_nodes = BTreeSet::new();
    //     let mut removed_nodes = BTreeSet::new();

    //     for change in diff.all_changes() {
    //         if let Some(node) = &change.new {
    //             added_nodes.insert(node.node_id());
    //         };
    //         if let Some(node) = &change.old {
    //             removed_nodes.insert(node.node_id());
    //         };
    //     }

    //     let mut leafs = BTreeSet::new();

    //     for node in added_nodes.iter() {
    //         if !new_structure.structure.nodes[node]
    //             .children
    //             .iter()
    //             .any(|c| match c {
    //                 crate::diff::symbols_map::NodeMarker::Lazy { node: child_id, .. } => {
    //                     added_nodes.contains(child_id)
    //                 }
    //                 _ => false,
    //             })
    //         {
    //             leafs.insert(*node);
    //         }
    //     }

    //     let mut tree_builder = ptree::TreeBuilder::new("Cascade of changes".to_string());

    //     let tree = &mut tree_builder;
    //     for leaf in leafs.iter() {
    //         Self::build_parent_tree(leaf, &new_structure.structure, &added_nodes, tree);
    //     }
    //     // ptree::print_tree(&tree.build()).unwrap();

    //     log::warn!("Added nodes: {added_nodes:?}");
    //     log::warn!("Added leafs: {leafs:?}");
    // }

    // fn build_child_tree(
    //     node_id: &GraphNode,
    //     structure: &crate::diff::symbols_map::Structure,
    //     changed_nodes: &BTreeSet<GraphNode>,
    //     tree: &mut ptree::TreeBuilder,
    // ) {
    //     let mut used_childs = BTreeSet::new();
    //     let node = &structure.nodes[node_id];
    //     let name = node.signature().display_signature();
    //     tree.begin_child(name.clone());
    //     for child in node.children.iter() {
    //         let crate::diff::symbols_map::NodeMarker::Lazy { node: child_id, .. } = child else {
    //             continue;
    //         };
    //         if !changed_nodes.contains(child_id) || used_childs.contains(child_id) {
    //             continue;
    //         }
    //         used_childs.insert(child_id);
    //         Self::build_child_tree(child_id, structure, changed_nodes, tree);
    //     }
    //     tree.end_child();
    // }

    // fn build_parent_tree(
    //     node_id: &GraphNode,
    //     structure: &crate::diff::symbols_map::Structure,
    //     changed_nodes: &BTreeSet<GraphNode>,
    //     tree: &mut ptree::TreeBuilder,
    // ) {
    //     let mut used_parents = BTreeSet::new();
    //     let node = &structure.nodes[node_id];
    //     let name = node.signature().display_signature();
    //     tree.begin_child(name.clone());
    //     for parent in node.parents.iter() {
    //         if !changed_nodes.contains(parent) || used_parents.contains(parent) {
    //             continue;
    //         }
    //         used_parents.insert(parent);
    //         Self::build_parent_tree(parent, structure, changed_nodes, tree);
    //     }
    //     tree.end_child();
    // }

    // fn module_structure_from_module(
    //     module: &read::InputModule<'src>,
    // ) -> crate::diff::symbols_map::ModuleStructure {
    //     let info = crate::analysis::ModuleInfo::new(module).unwrap();

    //     crate::metadata_ext::_build_module_structure(&info)
    // }
    fn print_compare_data(&self) {
        let mut errors = Vec::new();
        let left = &self.left.wasm.data.data_segments;
        let right = &self.right.wasm.data.data_segments;
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
        let res = errors;

        let process = |left: Option<&Data<'_>>, right: Option<&Data<'_>>| {
            if let (Some(left), Some(right)) = (left, right) {
                let left = hex::encode(left.data);
                let right = hex::encode(right.data);
                print_inline_diff(&left, &right);
            }
        };
        if res.is_empty() {
            log::info!("No differences in {} found", stringify!(data.data_segments));
        }
        for (id, err) in res {
            let left = self.left.wasm.data.data_segments.get(id);
            let right = self.right.wasm.data.data_segments.get(id);

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
        left: &IdVec<Type>,
        right: &IdVec<Type>,
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
            ) => {
                let offset_expr = const_expr_to_string(offset_expr);
                let other_offset_expr = const_expr_to_string(other_offset_expr);
                table_index == other_table_index && offset_expr == other_offset_expr
            }
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
            ) => {
                // TODO: compare items
                funcs.range().len() == other_funcs.range().len()
            }
            (
                wasmparser::ElementItems::Expressions(exprs, reader),
                wasmparser::ElementItems::Expressions(other_exprs, other_reader),
            ) => exprs == other_exprs && reader.range().len() == other_reader.range().len(),
            _ => false,
        };
        if !items_same {
            return Err(anyhow::anyhow!("Mismatched Element items"));
        }
        Ok(())
    }
    fn debug(&self) -> String {
        let kind_type = match &self.kind {
            wasmparser::ElementKind::Active {
                offset_expr,
                table_index,
            } => format!(
                "Active {{ offset_expr: {:?}, table_index: {:?} }}",
                const_expr_to_string(offset_expr),
                table_index
            ),
            wasmparser::ElementKind::Passive => "Passive".into(),
            wasmparser::ElementKind::Declared => "Declared".into(),
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

fn const_expr_to_string(expr: &wasmparser::ConstExpr<'_>) -> String {
    let mut ops = expr.get_operators_reader();
    let mut result = String::new();
    while !ops.is_end_then_eof() {
        if let Ok(op) = ops.read() {
            result.push_str(&format!("{:?} ", op));
        } else {
            break;
        }
    }
    result
}
