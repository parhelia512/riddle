mod delta;
mod format;
mod layout;
mod svg;

pub use delta::{Delta, Snapshot};

use scope_graph::{Node, ScopeGraph, resolve::resolve_reference};

use crate::{app::ReparseStatus, html::escape};

use self::{
    format::{format_def, node_no, path_text, reference_count},
    layout::Layout,
    svg::render_svg,
};

pub fn render_graph(
    graph: &ScopeGraph,
    delta: &Delta,
    reparse: &ReparseStatus,
    parse_errors: &[String],
) -> String {
    let layout = Layout::new(graph);
    let mut out = String::new();

    out.push_str("<div class=\"summary\">");
    out.push_str(&format!(
        "<span class=\"badge {}\">{}</span>",
        reparse.class(),
        escape(&reparse.label())
    ));
    out.push_str(&metric("nodes", graph.nodes.iter().count()));
    out.push_str(&metric("edges", graph.edges.iter().count()));
    out.push_str(&metric("fragments", graph.fragments.iter().count()));
    out.push_str(&metric("references", reference_count(graph)));
    if !parse_errors.is_empty() {
        out.push_str(&format!(
            "<span class=\"badge bad\">{} parse errors</span>",
            parse_errors.len()
        ));
    }
    out.push_str("</div>");

    out.push_str("<div class=\"split-output\">");
    out.push_str("<div class=\"graph-shell\">");
    out.push_str(&render_svg(graph, &layout, delta));
    out.push_str("</div>");
    out.push_str("<aside class=\"side-panel\">");
    out.push_str(&render_delta(delta));
    out.push_str(&render_references(graph));
    if !parse_errors.is_empty() {
        out.push_str("<section><h2>Parse Errors</h2><ul class=\"compact-list\">");
        for error in parse_errors {
            out.push_str("<li>");
            out.push_str(&escape(error));
            out.push_str("</li>");
        }
        out.push_str("</ul></section>");
    }
    out.push_str("</aside>");
    out.push_str("</div>");

    out
}

pub fn render_error(message: &str, parse_errors: &[String]) -> String {
    let mut out = String::new();
    out.push_str("<div class=\"summary\"><span class=\"badge bad\">render failed</span></div>");
    out.push_str("<div class=\"error-panel\"><h2>Error</h2><p>");
    out.push_str(&escape(message));
    out.push_str("</p>");
    if !parse_errors.is_empty() {
        out.push_str("<ul>");
        for error in parse_errors {
            out.push_str("<li>");
            out.push_str(&escape(error));
            out.push_str("</li>");
        }
        out.push_str("</ul>");
    }
    out.push_str("</div>");
    out
}

fn metric(name: &str, value: usize) -> String {
    format!(
        "<span class=\"metric\"><b>{}</b><small>{}</small></span>",
        value, name
    )
}

fn render_delta(delta: &Delta) -> String {
    format!(
        "<section><h2>Delta</h2>\
         <div class=\"delta-grid\">\
         <span>nodes</span><b class=\"added\">+{}</b><b class=\"changed\">~{}</b><b class=\"removed\">-{}</b>\
         <span>edges</span><b class=\"added\">+{}</b><b class=\"changed\">~{}</b><b class=\"removed\">-{}</b>\
         <span>refs</span><b class=\"added\">+{}</b><b class=\"changed\">~{}</b><b class=\"removed\">-{}</b>\
         </div></section>",
        delta.node_added,
        delta.node_changed,
        delta.node_removed,
        delta.edge_added,
        delta.edge_changed,
        delta.edge_removed,
        delta.reference_added,
        delta.reference_changed,
        delta.reference_removed
    )
}

fn render_references(graph: &ScopeGraph) -> String {
    let mut refs = Vec::new();
    for (id, node) in graph.nodes.iter() {
        let Node::Reference {
            segments, anchor, ..
        } = node
        else {
            continue;
        };
        let defs = resolve_reference(graph, id)
            .iter()
            .map(format_def)
            .collect::<Vec<_>>();
        refs.push((
            format!("n{}", node_no(id)),
            path_text(segments),
            format!("n{}", node_no(*anchor)),
            if defs.is_empty() {
                "unresolved".into()
            } else {
                defs.join(", ")
            },
        ));
    }

    let mut out = String::from("<section><h2>References</h2>");
    if refs.is_empty() {
        out.push_str("<p class=\"muted\">No expression references.</p></section>");
        return out;
    }

    out.push_str("<table><thead><tr><th>node</th><th>path</th><th>anchor</th><th>resolves to</th></tr></thead><tbody>");
    for (node, path, anchor, defs) in refs {
        out.push_str("<tr><td>");
        out.push_str(&escape(&node));
        out.push_str("</td><td>");
        out.push_str(&escape(&path));
        out.push_str("</td><td>");
        out.push_str(&escape(&anchor));
        out.push_str("</td><td>");
        out.push_str(&escape(&defs));
        out.push_str("</td></tr>");
    }
    out.push_str("</tbody></table></section>");
    out
}
