use std::collections::BTreeMap;

use hir::{
    HirFile,
    item_tree::{HirUseTreeKind, TopLevelItem},
};
use scope_graph::{Node, ScopeGraph, resolve::resolve_reference};
use type_checker::TypeCheckResult;

use super::format::{
    edge_kind, edge_no, expr_label, format_def, function_signature, item_symbol_signature, node_no,
    node_signature, path_text, type_ref_text,
};

#[derive(Default, Clone)]
pub struct Snapshot {
    nodes: BTreeMap<u32, String>,
    edges: BTreeMap<u32, String>,
    references: BTreeMap<String, String>,
    symbols: BTreeMap<String, String>,
    types: BTreeMap<String, String>,
    diagnostics: BTreeMap<String, String>,
}

impl Snapshot {
    pub fn from_semantics(
        hir: &HirFile,
        graph: &ScopeGraph,
        type_result: &TypeCheckResult,
    ) -> Self {
        let nodes = graph
            .nodes
            .iter()
            .map(|(id, node)| (node_no(id), node_signature(id, node)))
            .collect();
        let edges = graph
            .edges
            .iter()
            .map(|(id, edge)| {
                (
                    edge_no(id),
                    format!(
                        "{}:{}->{}:{}",
                        edge_kind(edge.kind),
                        node_no(edge.from),
                        node_no(edge.to),
                        edge.precedence
                    ),
                )
            })
            .collect();
        let references = graph
            .nodes
            .iter()
            .filter_map(|(id, node)| {
                let Node::Reference { segments, .. } = node else {
                    return None;
                };
                let defs = resolve_reference(graph, id)
                    .iter()
                    .map(format_def)
                    .collect::<Vec<_>>()
                    .join(", ");
                Some((
                    format!("n{}", node_no(id)),
                    format!("{} -> {}", path_text(segments), defs),
                ))
            })
            .collect();
        let mut symbols = BTreeMap::new();
        collect_item_symbols(hir, &hir.item_tree.top_level, "crate", &mut symbols);
        for (fid, function) in hir.item_tree.functions.iter() {
            if let Some(body_id) = hir.function_bodies.get(&fid).copied() {
                let body = &hir.bodies[body_id];
                for (idx, param) in function.params.iter().enumerate() {
                    symbols.insert(
                        format!("param:{fid:?}:{idx}"),
                        format!(
                            "{}::param {}: {}",
                            function.name.0,
                            param.name.0,
                            type_ref_text(&param.ty)
                        ),
                    );
                }
                for (stmt_id, stmt) in body.stmts.iter() {
                    if let hir::body::Stmt::Let { name, ty, init } = stmt {
                        let init_text = init
                            .map(|expr| expr_label(body, expr))
                            .unwrap_or_else(|| "<none>".to_string());
                        symbols.insert(
                            format!("local:{body_id:?}:{stmt_id:?}"),
                            format!(
                                "{}::local {}: {} = {}",
                                function.name.0,
                                name.0,
                                type_ref_text(ty),
                                init_text
                            ),
                        );
                    }
                }
            }
        }
        for (body_id, body) in hir.bodies.iter() {
            for (pat_id, pat) in body.pats.iter() {
                if let hir::body::Pattern::Binding { name } = pat {
                    symbols.insert(
                        format!("binding:{body_id:?}:{pat_id:?}"),
                        format!("pattern binding {}", name.0),
                    );
                }
            }
        }

        let types = type_result
            .expr_types
            .iter()
            .map(|((body, expr), ty)| {
                (
                    format!("{body:?}:{expr:?}"),
                    format!("{expr:?}: {}", ty.display(hir)),
                )
            })
            .collect();

        let diagnostics = type_result
            .diagnostics
            .iter()
            .enumerate()
            .map(|(idx, diagnostic)| {
                (
                    format!("type:{idx}"),
                    format!("type diagnostic: {}", diagnostic.message),
                )
            })
            .chain(hir.bodies.iter().flat_map(|(body_id, body)| {
                body.diagnostics
                    .iter()
                    .enumerate()
                    .map(move |(idx, diagnostic)| {
                        (
                            format!("lower:{body_id:?}:{idx}"),
                            format!("lowering diagnostic: {}", diagnostic.message),
                        )
                    })
            }))
            .collect();

        Self {
            nodes,
            edges,
            references,
            symbols,
            types,
            diagnostics,
        }
    }
}

pub struct Delta {
    pub node_status: BTreeMap<u32, ChangeStatus>,
    pub edge_status: BTreeMap<u32, ChangeStatus>,
    pub node_added: usize,
    pub node_changed: usize,
    pub node_removed: usize,
    pub edge_added: usize,
    pub edge_changed: usize,
    pub edge_removed: usize,
    pub reference_added: usize,
    pub reference_changed: usize,
    pub reference_removed: usize,
    pub symbol_added: usize,
    pub symbol_changed: usize,
    pub symbol_removed: usize,
    pub type_added: usize,
    pub type_changed: usize,
    pub type_removed: usize,
    pub diagnostic_added: usize,
    pub diagnostic_changed: usize,
    pub diagnostic_removed: usize,
}

impl Delta {
    pub fn new(previous: Option<&Snapshot>, current: &Snapshot) -> Self {
        let Some(previous) = previous else {
            return Self {
                node_status: current
                    .nodes
                    .keys()
                    .map(|id| (*id, ChangeStatus::Stable))
                    .collect(),
                edge_status: current
                    .edges
                    .keys()
                    .map(|id| (*id, ChangeStatus::Stable))
                    .collect(),
                node_added: 0,
                node_changed: 0,
                node_removed: 0,
                edge_added: 0,
                edge_changed: 0,
                edge_removed: 0,
                reference_added: 0,
                reference_changed: 0,
                reference_removed: 0,
                symbol_added: 0,
                symbol_changed: 0,
                symbol_removed: 0,
                type_added: 0,
                type_changed: 0,
                type_removed: 0,
                diagnostic_added: 0,
                diagnostic_changed: 0,
                diagnostic_removed: 0,
            };
        };

        let (node_status, node_added, node_changed, node_removed) =
            map_delta(&previous.nodes, &current.nodes);
        let (edge_status, edge_added, edge_changed, edge_removed) =
            map_delta(&previous.edges, &current.edges);
        let (_, reference_added, reference_changed, reference_removed) =
            map_delta(&previous.references, &current.references);
        let (_, symbol_added, symbol_changed, symbol_removed) =
            map_delta(&previous.symbols, &current.symbols);
        let (_, type_added, type_changed, type_removed) =
            map_delta(&previous.types, &current.types);
        let (_, diagnostic_added, diagnostic_changed, diagnostic_removed) =
            map_delta(&previous.diagnostics, &current.diagnostics);

        Self {
            node_status,
            edge_status,
            node_added,
            node_changed,
            node_removed,
            edge_added,
            edge_changed,
            edge_removed,
            reference_added,
            reference_changed,
            reference_removed,
            symbol_added,
            symbol_changed,
            symbol_removed,
            type_added,
            type_changed,
            type_removed,
            diagnostic_added,
            diagnostic_changed,
            diagnostic_removed,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChangeStatus {
    Stable,
    Added,
    Changed,
}

impl ChangeStatus {
    pub fn class(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Added => "added",
            Self::Changed => "changed",
        }
    }
}

fn map_delta<K>(
    previous: &BTreeMap<K, String>,
    current: &BTreeMap<K, String>,
) -> (BTreeMap<K, ChangeStatus>, usize, usize, usize)
where
    K: Clone + Ord,
{
    let mut statuses = BTreeMap::new();
    let mut added = 0;
    let mut changed = 0;

    for (id, value) in current {
        let status = match previous.get(id) {
            None => {
                added += 1;
                ChangeStatus::Added
            }
            Some(old) if old != value => {
                changed += 1;
                ChangeStatus::Changed
            }
            Some(_) => ChangeStatus::Stable,
        };
        statuses.insert(id.clone(), status);
    }

    let removed = previous
        .keys()
        .filter(|id| !current.contains_key(id))
        .count();

    (statuses, added, changed, removed)
}

fn collect_item_symbols(
    hir: &HirFile,
    items: &[TopLevelItem],
    module_path: &str,
    out: &mut BTreeMap<String, String>,
) {
    for item in items {
        match item {
            TopLevelItem::Function(id) => {
                let function = &hir.item_tree.functions[*id];
                out.insert(
                    format!("function:{id:?}"),
                    format!(
                        "{}::{} {}",
                        module_path,
                        function.name.0,
                        function_signature(function)
                    ),
                );
            }
            TopLevelItem::Struct(id) => {
                out.insert(
                    format!("struct:{id:?}"),
                    item_symbol_signature(hir, item, module_path),
                );
            }
            TopLevelItem::Enum(id) => {
                out.insert(
                    format!("enum:{id:?}"),
                    item_symbol_signature(hir, item, module_path),
                );
            }
            TopLevelItem::Trait(id) => {
                out.insert(
                    format!("trait:{id:?}"),
                    item_symbol_signature(hir, item, module_path),
                );
            }
            TopLevelItem::Const(id) => {
                out.insert(
                    format!("const:{id:?}"),
                    item_symbol_signature(hir, item, module_path),
                );
            }
            TopLevelItem::TypeAlias(id) => {
                out.insert(
                    format!("type_alias:{id:?}"),
                    item_symbol_signature(hir, item, module_path),
                );
            }
            TopLevelItem::Module(id) => {
                let module = &hir.item_tree.modules[*id];
                let next_path = format!("{module_path}::{}", module.name.0);
                out.insert(format!("module:{id:?}"), format!("module {next_path}"));
                if let Some(children) = &module.items {
                    collect_item_symbols(hir, children, &next_path, out);
                }
            }
            TopLevelItem::Use(id) => {
                let import = &hir.item_tree.uses[*id];
                let kind = match &import.tree.kind {
                    HirUseTreeKind::Simple { .. } => "use",
                    HirUseTreeKind::Glob => "glob use",
                    HirUseTreeKind::List(_) => "use list",
                };
                out.insert(
                    format!("use:{id:?}"),
                    format!("{module_path}::{kind} {}", import.tree.prefix.display()),
                );
            }
            TopLevelItem::Impl(id) => {
                let imp = &hir.item_tree.impls[*id];
                out.insert(
                    format!("impl:{id:?}"),
                    format!(
                        "{module_path}::impl {}{}",
                        type_ref_text(&imp.self_ty),
                        imp.trait_ty
                            .as_ref()
                            .map(|trait_ty| format!(" for {}", type_ref_text(trait_ty)))
                            .unwrap_or_default()
                    ),
                );
            }
        }
    }
}
