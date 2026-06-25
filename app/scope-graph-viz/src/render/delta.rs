use std::collections::BTreeMap;

use scope_graph::{Node, ScopeGraph, resolve::resolve_reference};

use super::format::{edge_kind, edge_no, format_def, node_no, node_signature, path_text};

#[derive(Default, Clone)]
pub struct Snapshot {
    nodes: BTreeMap<u32, String>,
    edges: BTreeMap<u32, String>,
    references: BTreeMap<String, String>,
}

impl Snapshot {
    pub fn from_graph(graph: &ScopeGraph) -> Self {
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

        Self {
            nodes,
            edges,
            references,
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
            };
        };

        let (node_status, node_added, node_changed, node_removed) =
            map_delta(&previous.nodes, &current.nodes);
        let (edge_status, edge_added, edge_changed, edge_removed) =
            map_delta(&previous.edges, &current.edges);
        let (_, reference_added, reference_changed, reference_removed) =
            map_delta(&previous.references, &current.references);

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
