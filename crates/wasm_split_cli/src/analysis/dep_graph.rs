use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Debug,
    ops::Range,
};

use anyhow::Context;
use wasmparser::RelocationType;

use crate::{analysis, helpers::ShiftRange, index::DataSegmentId, read::InputModule};
use crate::{
    index::{InputFuncId, SymbolIndex},
    read::linking::SymbolType,
};

#[derive(Debug, PartialEq, Eq, Hash, Copy, PartialOrd, Ord, Clone)]
pub enum DepNode {
    Function(InputFuncId),
    DataSymbol(DataSegmentId, SymbolIndex),
}

pub type DepGraph = HashMap<DepNode, HashSet<DepNode>>;

#[derive(Debug, Default)]
pub struct ReachabilityGraph {
    pub reachable: HashSet<DepNode>,
    pub parents: HashMap<DepNode, DepNode>,
}

pub trait SymbolTable {
    fn get_symbol_dep_node(&self, symbol_index: SymbolIndex) -> Option<DepNode>;
}

impl<'a> SymbolTable for InputModule<'a> {
    fn get_symbol_dep_node(&self, linking_index: SymbolIndex) -> Option<DepNode> {
        let (idx, sym_type) = self.linking.linking_symbols.original_indexes[linking_index];
        match sym_type {
            SymbolType::Func => Some(DepNode::Function(idx as InputFuncId)),
            SymbolType::DataDefined(segment_id) => {
                Some(DepNode::DataSymbol(segment_id, idx as InputFuncId))
            }
            SymbolType::DataUndefined => {
                log::error!(
                    "{idx} {} is undefined data symbol, dont have any location in data segment, what can we do with it?",
                    self.linking.linking_symbols.undefined_data[idx].name
                );
                None
            }
            _ => {
                //println!("{v:?} is not supported symol dep");
                None
            }
        }
    }
}

pub fn get_dependencies(
    module: &InputModule,
    info: &analysis::ModuleInfo,
) -> anyhow::Result<DepGraph> {
    let mut deps = DepGraph::new();
    let mut add_dep = |a: DepNode, linking_index: u32| {
        if let Some(target) = module.get_symbol_dep_node(linking_index as usize) {
            log::trace!("Adding dirrect deps {a:?} -> {target:?}");
            deps.entry(a).or_default().insert(target);
        };
    };

    if let Some(relocs) = module.relocs.get_section(module.code.section_index) {
        for entry in &relocs.entries {
            let func_index = find_function_containing_range(
                info,
                entry
                    .relocation_range()
                    .shift_right(module.code.starting_offset),
            )
            .with_context(|| format!("Invalid code relocation entry {entry:?}"))?;

            match entry.ty {
                RelocationType::TypeIndexLeb => {
                    continue;
                } //TODO: refine rest?
                _ => {}
            }
            add_dep(DepNode::Function(func_index), entry.index);
        }
    }

    if let Some(relocs) = module.relocs.get_section(module.data.section_index) {
        for entry in &relocs.entries {
            let (segment_index, symbol_index) = find_data_symbol_containing_range(
                info,
                entry
                    .relocation_range()
                    .shift_right(module.data.starting_offset),
            )
            .with_context(|| format!("Invalid data relocation entry {entry:?}"))?;
            add_dep(
                DepNode::DataSymbol(segment_index, symbol_index),
                entry.index,
            );
        }
    }
    Ok(deps)
}

impl ReachabilityGraph {
    pub fn find_reachable_deps(
        deps: &DepGraph,
        roots: &HashSet<DepNode>,
        exclude: &HashSet<DepNode>,
    ) -> ReachabilityGraph {
        let mut queue: VecDeque<DepNode> = roots.iter().copied().collect();
        let mut seen = HashSet::<DepNode>::new();
        // println!("exclude = {exclude:?}");
        // multiple parents?
        let mut parents = HashMap::<DepNode, DepNode>::new();
        while let Some(node) = queue.pop_front() {
            // println!("queue node: {node:?}");
            seen.insert(node);
            let Some(children) = deps.get(&node) else {
                continue;
            };
            for child in children {
                if seen.contains(&child) || exclude.contains(&child) {
                    continue;
                }
                parents.entry(*child).or_insert(node);
                queue.push_back(*child);
            }
        }
        ReachabilityGraph {
            reachable: seen,
            parents,
        }
    }
    pub fn print(&self, module_name: &str, module: &InputModule, info: &analysis::ModuleInfo) {
        Self::print_deps_inner(module_name, module, info, &self.reachable, &self.parents);
    }
    pub(crate) fn print_deps_inner(
        module_name: &str,
        module: &InputModule,
        info: &analysis::ModuleInfo,
        reachable: &HashSet<DepNode>,
        parents: &HashMap<DepNode, DepNode>,
    ) {
        let format_dep = |dep: &DepNode| match dep {
            DepNode::Function(index) => {
                let name = module.names.functions.get(*index);
                format!("func[{index}] <{name:?}>")
            }
            DepNode::DataSymbol(segment, idx) => {
                let symbol = module
                    .linking
                    .get_data_in_segment(*segment, *idx)
                    .expect("indexes should be valid")
                    .name;
                let segment = module.names.data_segments[segment];
                format!("data[{segment}:{idx}] <{symbol:?}>")
            }
        };

        println!("SPLIT: ============== {module_name}");
        let mut total_size: usize = 0;
        for dep in reachable.iter() {
            let size = match dep {
                DepNode::Function(index) => {
                    let size = index
                        .checked_sub(info.import_funcs_info.imported_funcs.len())
                        .map(|defined_index| {
                            module.code.section_payload.defined_funcs[defined_index]
                                .body
                                .range()
                                .len()
                        })
                        .unwrap_or_default();
                    size
                }
                DepNode::DataSymbol(segment, idx) => {
                    module.linking.linking_symbols.data_in_segments[*segment][*idx].size as usize
                }
            };

            total_size += size;

            println!("   {} size={size:?}", format_dep(dep));
            let mut node = dep;
            while let Some(parent) = parents.get(node) {
                println!("      <== {}", format_dep(parent));
                node = parent;
            }
        }
        println!("SPLIT: ============== {module_name}  : total size: {total_size}");
    }
}

struct NamedGraph<'a> {
    // Module name, None if main.
    module: Option<&'a str>,

    reachable: Vec<DepNode>,
}

struct SharedEntries<'a> {
    module_names: Vec<&'a str>,

    nodes: Vec<DepNode>,
}

fn find_function_containing_range(
    info: &analysis::ModuleInfo,
    range: Range<usize>,
) -> anyhow::Result<usize> {
    info.find_function_id_containing_range(range)
}

fn find_data_symbol_containing_range(
    info: &analysis::ModuleInfo,
    range: Range<usize>,
) -> anyhow::Result<(usize, usize)> {
    let sym = &info.find_data_symbol_containing_range(range)?;
    Ok((sym.segment_index, sym.symbol_index))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use lazy_static::lazy_static;

    use crate::{
        analysis::{
            self,
            dep_graph::{DepGraph, DepNode},
            testing,
        },
        read,
    };

    // checkout test-data/simple-graph crate at root (just keep wasm in case rustc changes)
    const WASM_FILE: &[u8] = include_bytes!("../../test-data/simple_graph.wasm");

    #[test]
    fn load_dep_graph() {
        let module = read::InputModule::parse(&WASM_FILE).unwrap();
        let info = analysis::ModuleInfo::new(&module).unwrap();
        let dep_graph = super::get_dependencies(&module, &info).unwrap();

        let format_dep = |dep: &DepNode| match dep {
            DepNode::Function(index) => {
                let name = module.names.functions.get(*index);
                format!("func[{index}] <{name:?}>")
            }
            DepNode::DataSymbol(segment, idx) => {
                let symbol = module.linking.linking_symbols.data_in_segments[segment][*idx].name;
                let segment = module.names.data_segments[segment];
                format!("data[{segment}:{idx}] <{symbol:?}>")
            }
        };

        for (node, deps) in dep_graph.iter() {
            println!("node: {node}", node = format_dep(node));
            for dep in deps {
                println!("  =>{dep}", dep = format_dep(dep));
            }
        }

        let no_inline_fn = info.find_function_id_by_name("no_inline_fn").unwrap();

        let deps = dep_graph.get(&DepNode::Function(no_inline_fn)).unwrap();
        let func_deps: Vec<_> = deps
            .iter()
            .filter(|dep| matches!(dep, DepNode::Function(_)))
            .collect();
        let data_deps: Vec<_> = deps
            .iter()
            .filter(|dep| matches!(dep, DepNode::DataSymbol(..)))
            .collect();

        assert_eq!(func_deps.len(), 3);
        // no_inline_fn is really inline data, but keep method call for "side effect"
        assert_eq!(data_deps.len(), 3);

        let indirrect_fn = info.find_function_id_by_name("indirrect_fn").unwrap();

        let deps = dep_graph.get(&DepNode::Function(indirrect_fn)).unwrap();
        assert_eq!(deps.len(), 1); // only dep on switchtable
        let swith_table = deps.iter().next().unwrap();
        assert!(matches!(swith_table, DepNode::DataSymbol(..)));
        let fns = dep_graph.get(swith_table).unwrap();

        assert_eq!(fns.len(), 3);
    }

    #[test]
    fn reachablity_graph() {
        let module = read::InputModule::parse(&WASM_FILE).unwrap();
        let info = analysis::ModuleInfo::new(&module).unwrap();
        let dep_graph = super::get_dependencies(&module, &info).unwrap();

        let no_inline_fn = info.find_function_id_by_name("no_inline_fn").unwrap();

        let reachability_graph = super::ReachabilityGraph::find_reachable_deps(
            &dep_graph,
            &HashSet::from([DepNode::Function(no_inline_fn)]),
            &HashSet::new(),
        );
        // no_inline_fn -> data1
        //              -> data2
        //              -> data3
        //              -> func1 -> data1
        //              -> func2 -> data2
        //              -> func3 -> data3
        reachability_graph.print("no_inline_fn", &module, &info);
        assert_eq!(reachability_graph.reachable.len(), 7); // root +  3 data + 3 funcs

        let indirrect_fn = info.find_function_id_by_name("indirrect_fn").unwrap();
        let reachability_graph = super::ReachabilityGraph::find_reachable_deps(
            &dep_graph,
            &HashSet::from([DepNode::Function(indirrect_fn)]),
            &HashSet::new(),
        );
        reachability_graph.print("indirrect_fn", &module, &info);
        // almost same count, but indirrect_fn has more deep graph and switchtable
        // indirrect_fn -> switchtable -> func1 -> data1
        //                             -> func2 -> data2
        //                             -> func3 -> data3
        assert_eq!(reachability_graph.reachable.len(), 8); // root + <switchtable> +  3 data + 3 funcs
    }

    lazy_static! {
        static ref GRAPH: DepGraph = testing::parse_deps(
            r#"
            F(1) -> D(2, 3) & F(4) -> D(5, 6) & F(7) -> D(8, 9)
            F(11) -> F(4) & F(12) 
            "#
        )
        .unwrap()
        .1;
    }
}
