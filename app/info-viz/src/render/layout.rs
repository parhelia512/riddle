use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use scope_graph::ScopeGraph;

use super::format::node_no;

pub const NODE_W: f32 = 230.0;
pub const NODE_H: f32 = 78.0;

const RANK_GAP: f32 = 300.0;
const ROW_GAP: f32 = 116.0;

pub struct Layout {
    pub positions: BTreeMap<u32, (f32, f32)>,
    pub width: f32,
    pub height: f32,
}

impl Layout {
    pub fn new(graph: &ScopeGraph) -> Self {
        let mut adjacency: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut all_nodes = BTreeSet::new();

        for (id, _) in graph.nodes.iter() {
            all_nodes.insert(node_no(id));
            adjacency.entry(node_no(id)).or_default();
        }
        for (_, edge) in graph.edges.iter() {
            let from = node_no(edge.from);
            let to = node_no(edge.to);
            adjacency.entry(from).or_default().push(to);
            adjacency.entry(to).or_default().push(from);
        }

        let mut ranks = BTreeMap::new();
        let mut next_base_rank = 0usize;
        let roots = std::iter::once(node_no(graph.root))
            .chain(
                all_nodes
                    .iter()
                    .copied()
                    .filter(|id| *id != node_no(graph.root)),
            )
            .collect::<Vec<_>>();

        for root in roots {
            if ranks.contains_key(&root) {
                continue;
            }

            let mut queue = VecDeque::new();
            ranks.insert(root, next_base_rank);
            queue.push_back(root);
            let mut component_max_rank = next_base_rank;

            while let Some(node) = queue.pop_front() {
                let rank = ranks[&node];
                component_max_rank = component_max_rank.max(rank);
                let mut neighbors = adjacency.get(&node).cloned().unwrap_or_default();
                neighbors.sort_unstable();
                for next in neighbors {
                    if ranks.contains_key(&next) {
                        continue;
                    }
                    ranks.insert(next, rank + 1);
                    queue.push_back(next);
                }
            }

            next_base_rank = component_max_rank + 2;
        }

        let mut layers: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
        for (node, rank) in ranks {
            layers.entry(rank).or_default().push(node);
        }

        let mut positions = BTreeMap::new();
        let mut max_rank = 0usize;
        let mut max_rows = 0usize;
        for (rank, nodes) in &mut layers {
            nodes.sort_unstable();
            max_rank = max_rank.max(*rank);
            max_rows = max_rows.max(nodes.len());
            for (row, node) in nodes.iter().enumerate() {
                positions.insert(
                    *node,
                    (50.0 + *rank as f32 * RANK_GAP, 50.0 + row as f32 * ROW_GAP),
                );
            }
        }

        let width = (100.0 + max_rank as f32 * RANK_GAP + NODE_W + 80.0).max(900.0);
        let height = (100.0 + max_rows as f32 * ROW_GAP + NODE_H + 80.0).max(620.0);

        Self {
            positions,
            width,
            height,
        }
    }
}
