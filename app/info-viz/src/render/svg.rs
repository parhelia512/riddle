use hir::{HirFile, body::Expr};
use scope_graph::{Node, RefOrigin, ScopeGraph, resolve::resolve_reference};
use type_checker::TypeCheckResult;

use crate::html::escape;

use super::{
    delta::{ChangeStatus, Delta},
    format::{
        edge_kind, edge_no, format_def, node_class, node_label, node_no, node_signature, path_text,
        resolved_name_text, wrap_label,
    },
    layout::{Layout, NODE_H, NODE_W},
};

pub fn render_svg(
    hir: &HirFile,
    graph: &ScopeGraph,
    type_result: &TypeCheckResult,
    layout: &Layout,
    delta: &Delta,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "<svg class=\"scope-svg\" viewBox=\"0 0 {:.0} {:.0}\" role=\"img\">",
        layout.width, layout.height
    ));
    out.push_str(
        "<defs>\
         <marker id=\"arrow\" markerWidth=\"10\" markerHeight=\"8\" refX=\"9\" refY=\"4\" orient=\"auto\" markerUnits=\"strokeWidth\">\
         <path d=\"M0,0 L10,4 L0,8 z\" />\
         </marker>\
         </defs>",
    );
    out.push_str("<g id=\"graphViewport\">");
    out.push_str("<g class=\"edges\">");

    for (id, edge) in graph.edges.iter() {
        let Some((x1, y1)) = layout.positions.get(&node_no(edge.from)).copied() else {
            continue;
        };
        let Some((x2, y2)) = layout.positions.get(&node_no(edge.to)).copied() else {
            continue;
        };
        let status = delta
            .edge_status
            .get(&edge_no(id))
            .copied()
            .unwrap_or(ChangeStatus::Stable);
        let (sx, sy, tx, ty) = edge_points(x1, y1, x2, y2);
        let bend = if tx >= sx { 90.0 } else { -90.0 };
        let path = format!(
            "M {:.1} {:.1} C {:.1} {:.1}, {:.1} {:.1}, {:.1} {:.1}",
            sx,
            sy,
            sx + bend,
            sy,
            tx - bend,
            ty,
            tx,
            ty
        );
        let mid_x = (sx + tx) / 2.0;
        let mid_y = (sy + ty) / 2.0 - 6.0;
        out.push_str(&format!(
            "<path class=\"edge edge-{} {}\" d=\"{}\" marker-end=\"url(#arrow)\"><title>{}</title></path>",
            edge_kind(edge.kind).to_ascii_lowercase(),
            status.class(),
            path,
            escape(&format!(
                "e{} {} n{} -> n{} precedence {}",
                edge_no(id),
                edge_kind(edge.kind),
                node_no(edge.from),
                node_no(edge.to),
                edge.precedence
            ))
        ));
        out.push_str(&format!(
            "<text class=\"edge-label\" x=\"{:.1}\" y=\"{:.1}\">{}</text>",
            mid_x,
            mid_y,
            escape(edge_kind(edge.kind))
        ));
    }

    out.push_str("</g><g class=\"nodes\">");
    for (id, node) in graph.nodes.iter() {
        let no = node_no(id);
        let Some((x, y)) = layout.positions.get(&no).copied() else {
            continue;
        };
        let status = delta
            .node_status
            .get(&no)
            .copied()
            .unwrap_or(ChangeStatus::Stable);
        let label = semantic_node_label(hir, graph, type_result, id, node);
        let title = semantic_node_title(hir, graph, type_result, id, node);
        let class = node_class(node);
        out.push_str(&format!(
            "<g class=\"node {} {}\" transform=\"translate({:.1} {:.1})\">",
            class,
            status.class(),
            x,
            y
        ));
        out.push_str(&format!(
            "<title>{}</title><rect width=\"{NODE_W}\" height=\"{NODE_H}\" rx=\"6\" />",
            escape(&title)
        ));
        out.push_str(&format!(
            "<text x=\"12\" y=\"18\"><tspan class=\"node-id\">n{}</tspan>",
            no
        ));
        for (line_idx, line) in wrap_label(&label, 24).iter().take(3).enumerate() {
            out.push_str(&format!(
                "<tspan x=\"12\" dy=\"{}\">{}</tspan>",
                if line_idx == 0 { 18 } else { 15 },
                escape(line)
            ));
        }
        out.push_str("</text></g>");
    }
    out.push_str("</g></g></svg>");
    out
}

fn semantic_node_label(
    hir: &HirFile,
    graph: &ScopeGraph,
    type_result: &TypeCheckResult,
    id: scope_graph::NodeId,
    node: &Node,
) -> String {
    match node {
        Node::Reference {
            segments, origin, ..
        } => {
            let mut lines = vec![format!("ref {}", path_text(segments))];
            if let Some(ty) = reference_type(hir, type_result, *origin) {
                lines.push(format!("type {ty}"));
            }
            if let Some(symbol) = reference_resolved_symbol(hir, *origin) {
                lines.push(symbol);
            } else {
                let defs = resolve_reference(graph, id)
                    .iter()
                    .map(format_def)
                    .collect::<Vec<_>>();
                lines.push(if defs.is_empty() {
                    "unresolved".to_string()
                } else {
                    defs.join(", ")
                });
            }
            lines.join("\n")
        }
        _ => node_label(node),
    }
}

fn semantic_node_title(
    hir: &HirFile,
    graph: &ScopeGraph,
    type_result: &TypeCheckResult,
    id: scope_graph::NodeId,
    node: &Node,
) -> String {
    let mut title = node_signature(id, node);
    if let Node::Reference { origin, .. } = node {
        if let Some(ty) = reference_type(hir, type_result, *origin) {
            title.push_str(&format!("\ntype: {ty}"));
        }
        if let Some(symbol) = reference_resolved_symbol(hir, *origin) {
            title.push_str(&format!("\nsymbol: {symbol}"));
        }
        let defs = resolve_reference(graph, id)
            .iter()
            .map(format_def)
            .collect::<Vec<_>>()
            .join(", ");
        title.push_str(&format!(
            "\nscope candidates: {}",
            if defs.is_empty() { "unresolved" } else { &defs }
        ));
    }
    title
}

fn reference_type(
    hir: &HirFile,
    type_result: &TypeCheckResult,
    origin: RefOrigin,
) -> Option<String> {
    match origin {
        RefOrigin::Expr { body, expr } => type_result
            .expr_types
            .get(&(body, expr))
            .map(|ty| ty.display(hir)),
    }
}

fn reference_resolved_symbol(hir: &HirFile, origin: RefOrigin) -> Option<String> {
    match origin {
        RefOrigin::Expr { body, expr } => match &hir.bodies[body].exprs[expr] {
            Expr::Path { resolved, .. } | Expr::Struct { resolved, .. } => {
                Some(resolved_name_text(hir, resolved.as_ref()))
            }
            _ => None,
        },
    }
}

fn edge_points(x1: f32, y1: f32, x2: f32, y2: f32) -> (f32, f32, f32, f32) {
    let cy1 = y1 + NODE_H / 2.0;
    let cy2 = y2 + NODE_H / 2.0;
    if x2 >= x1 {
        (x1 + NODE_W, cy1, x2, cy2)
    } else {
        (x1, cy1, x2 + NODE_W, cy2)
    }
}
