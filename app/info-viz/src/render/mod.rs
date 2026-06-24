mod delta;
mod format;
mod layout;
mod svg;

pub use delta::{Delta, Snapshot};

use hir::{
    HirFile,
    body::{Expr, Pattern, Stmt},
    item_tree::TopLevelItem,
};
use scope_graph::{Node, ScopeGraph, resolve::resolve_reference};
use type_checker::TypeCheckResult;

use crate::{app::ReparseStatus, html::escape};

use self::{
    format::{
        body_no, expr_label, expr_no, format_def, function_signature, item_symbol_signature,
        node_no, path_text, reference_count, resolved_name_text, stmt_label, type_ref_text,
        use_tree_text,
    },
    layout::Layout,
    svg::render_svg,
};

pub fn render_semantics(
    hir: &HirFile,
    graph: &ScopeGraph,
    type_result: &TypeCheckResult,
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
    out.push_str(&metric("symbols", symbol_count(hir)));
    out.push_str(&metric("typed exprs", type_result.expr_types.len()));
    if !parse_errors.is_empty() {
        out.push_str(&format!(
            "<span class=\"badge bad\">{} parse errors</span>",
            parse_errors.len()
        ));
    }
    if !type_result.diagnostics.is_empty() {
        out.push_str(&format!(
            "<span class=\"badge bad\">{} type diagnostics</span>",
            type_result.diagnostics.len()
        ));
    }
    out.push_str("</div>");

    out.push_str("<div class=\"split-output\">");
    out.push_str("<div class=\"graph-shell\">");
    out.push_str(&render_svg(hir, graph, type_result, &layout, delta));
    out.push_str("</div>");
    out.push_str("<aside class=\"side-panel\">");
    out.push_str(&render_delta(delta));
    out.push_str(&render_diagnostics(hir, type_result, parse_errors));
    out.push_str(&render_symbols(hir, type_result));
    out.push_str(&render_references(graph));
    out.push_str(&render_expression_types(hir, type_result));
    out.push_str(&render_body_messages(hir));
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

fn collapsible_section(title: &str, open: bool, body: &str) -> String {
    format!(
        "<details class=\"data-section\"{}><summary><span>{}</span></summary><div class=\"section-body\">{}</div></details>",
        if open { " open" } else { "" },
        escape(title),
        body
    )
}

fn render_delta(delta: &Delta) -> String {
    let body = format!(
        "<div class=\"delta-grid\">\
         <span></span><small>new</small><small>changed</small><small>gone</small>\
         <span>nodes</span><b class=\"added\">+{}</b><b class=\"changed\">~{}</b><b class=\"removed\">-{}</b>\
         <span>edges</span><b class=\"added\">+{}</b><b class=\"changed\">~{}</b><b class=\"removed\">-{}</b>\
         <span>refs</span><b class=\"added\">+{}</b><b class=\"changed\">~{}</b><b class=\"removed\">-{}</b>\
         <span>symbols</span><b class=\"added\">+{}</b><b class=\"changed\">~{}</b><b class=\"removed\">-{}</b>\
         <span>types</span><b class=\"added\">+{}</b><b class=\"changed\">~{}</b><b class=\"removed\">-{}</b>\
         <span>diagnostics</span><b class=\"added\">+{}</b><b class=\"changed\">~{}</b><b class=\"removed\">-{}</b>\
         </div>",
        delta.node_added,
        delta.node_changed,
        delta.node_removed,
        delta.edge_added,
        delta.edge_changed,
        delta.edge_removed,
        delta.reference_added,
        delta.reference_changed,
        delta.reference_removed,
        delta.symbol_added,
        delta.symbol_changed,
        delta.symbol_removed,
        delta.type_added,
        delta.type_changed,
        delta.type_removed,
        delta.diagnostic_added,
        delta.diagnostic_changed,
        delta.diagnostic_removed
    );
    collapsible_section("Delta", true, &body)
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

    let mut body = String::new();
    if refs.is_empty() {
        body.push_str("<p class=\"muted\">No expression references.</p>");
        return collapsible_section("References", false, &body);
    }

    body.push_str("<table><thead><tr><th>node</th><th>path</th><th>anchor</th><th>resolves to</th></tr></thead><tbody>");
    for (node, path, anchor, defs) in refs {
        body.push_str("<tr><td>");
        body.push_str(&escape(&node));
        body.push_str("</td><td>");
        body.push_str(&escape(&path));
        body.push_str("</td><td>");
        body.push_str(&escape(&anchor));
        body.push_str("</td><td>");
        body.push_str(&escape(&defs));
        body.push_str("</td></tr>");
    }
    body.push_str("</tbody></table>");
    collapsible_section("References", false, &body)
}

fn render_diagnostics(
    hir: &HirFile,
    type_result: &TypeCheckResult,
    parse_errors: &[String],
) -> String {
    let mut html = String::new();
    if parse_errors.is_empty()
        && type_result.diagnostics.is_empty()
        && hir
            .bodies
            .iter()
            .all(|(_, body)| body.diagnostics.is_empty())
    {
        html.push_str("<p class=\"muted\">No parser, lowering, or type diagnostics.</p>");
        return collapsible_section("Messages", true, &html);
    }

    html.push_str("<ul class=\"message-list\">");
    for error in parse_errors {
        html.push_str("<li><b class=\"removed\">parse</b><span>");
        html.push_str(&escape(error));
        html.push_str("</span></li>");
    }
    for (body_id, body) in hir.bodies.iter() {
        for diagnostic in &body.diagnostics {
            html.push_str("<li><b class=\"changed\">lower</b><span>");
            html.push_str(&escape(&format!(
                "body b{}: {}",
                body_no(body_id),
                diagnostic.message
            )));
            html.push_str("</span></li>");
        }
    }
    for diagnostic in &type_result.diagnostics {
        html.push_str("<li><b class=\"removed\">type</b><span>");
        html.push_str(&escape(&diagnostic.message));
        html.push_str("</span></li>");
    }
    html.push_str("</ul>");
    collapsible_section("Messages", true, &html)
}

fn render_symbols(hir: &HirFile, type_result: &TypeCheckResult) -> String {
    let mut rows = Vec::new();
    collect_symbol_rows(hir, &hir.item_tree.top_level, "crate", &mut rows);

    for (fid, function) in hir.item_tree.functions.iter() {
        let Some(body_id) = hir.function_bodies.get(&fid).copied() else {
            continue;
        };
        let body = &hir.bodies[body_id];
        for (idx, param) in function.params.iter().enumerate() {
            rows.push((
                format!("param #{}", idx),
                format!("{}::{}", function.name.0, param.name.0),
                type_ref_text(&param.ty),
                format!("{fid:?}"),
            ));
        }
        for (stmt_id, stmt) in body.stmts.iter() {
            let Stmt::Let { name, ty, init } = stmt else {
                continue;
            };
            let ty_text = type_ref_text(ty);
            let inferred = init
                .and_then(|expr| type_result_type(hir, body_id, expr, Some(type_result)))
                .unwrap_or_default();
            rows.push((
                "local".to_string(),
                format!("{}::{}", function.name.0, name.0),
                if inferred.is_empty() || inferred == ty_text || ty_text != "_" {
                    ty_text
                } else {
                    inferred
                },
                format!("{stmt_id:?}"),
            ));
        }
    }

    for (body_id, body) in hir.bodies.iter() {
        for (pat_id, pat) in body.pats.iter() {
            let Pattern::Binding { name } = pat else {
                continue;
            };
            rows.push((
                "pattern binding".to_string(),
                name.0.clone(),
                "_".to_string(),
                format!("b{} {pat_id:?}", body_no(body_id)),
            ));
        }
    }

    let mut html = String::new();
    if rows.is_empty() {
        html.push_str("<p class=\"muted\">No symbols.</p>");
        return collapsible_section("Symbols", false, &html);
    }

    html.push_str("<table><thead><tr><th>kind</th><th>name</th><th>type/detail</th><th>id</th></tr></thead><tbody>");
    for (kind, name, ty, id) in rows {
        html.push_str("<tr><td>");
        html.push_str(&escape(&kind));
        html.push_str("</td><td>");
        html.push_str(&escape(&name));
        html.push_str("</td><td>");
        html.push_str(&escape(&ty));
        html.push_str("</td><td>");
        html.push_str(&escape(&id));
        html.push_str("</td></tr>");
    }
    html.push_str("</tbody></table>");
    collapsible_section("Symbols", false, &html)
}

fn collect_symbol_rows(
    hir: &HirFile,
    items: &[TopLevelItem],
    module_path: &str,
    rows: &mut Vec<(String, String, String, String)>,
) {
    for item in items {
        match item {
            TopLevelItem::Function(fid) => {
                let function = &hir.item_tree.functions[*fid];
                rows.push((
                    "function".to_string(),
                    format!("{module_path}::{}", function.name.0),
                    function_signature(function),
                    format!("{fid:?}"),
                ));
            }
            TopLevelItem::Struct(sid) => {
                let strukt = &hir.item_tree.structs[*sid];
                rows.push((
                    "struct".to_string(),
                    format!("{module_path}::{}", strukt.name.0),
                    item_symbol_signature(hir, item, module_path),
                    format!("{sid:?}"),
                ));
            }
            TopLevelItem::Enum(eid) => {
                let enm = &hir.item_tree.enums[*eid];
                rows.push((
                    "enum".to_string(),
                    format!("{module_path}::{}", enm.name.0),
                    item_symbol_signature(hir, item, module_path),
                    format!("{eid:?}"),
                ));
            }
            TopLevelItem::Trait(tid) => {
                let tr = &hir.item_tree.traits[*tid];
                rows.push((
                    "trait".to_string(),
                    format!("{module_path}::{}", tr.name.0),
                    item_symbol_signature(hir, item, module_path),
                    format!("{tid:?}"),
                ));
            }
            TopLevelItem::Const(cid) => {
                let konst = &hir.item_tree.consts[*cid];
                rows.push((
                    "const".to_string(),
                    format!("{module_path}::{}", konst.name.0),
                    item_symbol_signature(hir, item, module_path),
                    format!("{cid:?}"),
                ));
            }
            TopLevelItem::TypeAlias(tid) => {
                let alias = &hir.item_tree.type_aliases[*tid];
                rows.push((
                    "type alias".to_string(),
                    format!("{module_path}::{}", alias.name.0),
                    item_symbol_signature(hir, item, module_path),
                    format!("{tid:?}"),
                ));
            }
            TopLevelItem::Module(mid) => {
                let module = &hir.item_tree.modules[*mid];
                let path = format!("{module_path}::{}", module.name.0);
                rows.push((
                    "module".to_string(),
                    path.clone(),
                    if module.items.is_some() {
                        "inline".to_string()
                    } else {
                        "external".to_string()
                    },
                    format!("{mid:?}"),
                ));
                if let Some(children) = &module.items {
                    collect_symbol_rows(hir, children, &path, rows);
                }
            }
            TopLevelItem::Use(uid) => {
                let import = &hir.item_tree.uses[*uid];
                rows.push((
                    "use".to_string(),
                    module_path.to_string(),
                    use_tree_text(&import.tree),
                    format!("{uid:?}"),
                ));
            }
            TopLevelItem::Impl(iid) => {
                rows.push((
                    "impl".to_string(),
                    module_path.to_string(),
                    item_symbol_signature(hir, item, module_path),
                    format!("{iid:?}"),
                ));
            }
        }
    }
}

fn render_expression_types(hir: &HirFile, type_result: &TypeCheckResult) -> String {
    let mut html = String::new();
    let mut rows = Vec::new();
    for (fid, function) in hir.item_tree.functions.iter() {
        let Some(body_id) = hir.function_bodies.get(&fid).copied() else {
            continue;
        };
        let body = &hir.bodies[body_id];
        for (expr_id, expr) in body.exprs.iter() {
            let ty = type_result
                .expr_types
                .get(&(body_id, expr_id))
                .map(|ty| ty.display(hir))
                .unwrap_or_else(|| "<not checked>".to_string());
            let symbol = match expr {
                Expr::Path { resolved, .. } | Expr::Struct { resolved, .. } => {
                    resolved_name_text(hir, resolved.as_ref())
                }
                _ => String::new(),
            };
            rows.push((
                format!("b{} e{}", body_no(body_id), expr_no(expr_id)),
                function.name.0.clone(),
                expr_label(body, expr_id),
                ty,
                symbol,
            ));
        }
    }

    if rows.is_empty() {
        html.push_str("<p class=\"muted\">No checked expressions.</p>");
        return collapsible_section("Expression Types", false, &html);
    }

    html.push_str("<table><thead><tr><th>expr</th><th>function</th><th>source</th><th>type</th><th>symbol</th></tr></thead><tbody>");
    for (id, function, expr, ty, symbol) in rows {
        html.push_str("<tr><td>");
        html.push_str(&escape(&id));
        html.push_str("</td><td>");
        html.push_str(&escape(&function));
        html.push_str("</td><td>");
        html.push_str(&escape(&expr));
        html.push_str("</td><td>");
        html.push_str(&escape(&ty));
        html.push_str("</td><td>");
        html.push_str(&escape(&symbol));
        html.push_str("</td></tr>");
    }
    html.push_str("</tbody></table>");
    collapsible_section("Expression Types", false, &html)
}

fn render_body_messages(hir: &HirFile) -> String {
    let mut html = String::new();
    let mut rows = Vec::new();
    for (fid, function) in hir.item_tree.functions.iter() {
        let Some(body_id) = hir.function_bodies.get(&fid).copied() else {
            continue;
        };
        let body = &hir.bodies[body_id];
        for (stmt_id, _) in body.stmts.iter() {
            rows.push((
                format!("b{} {stmt_id:?}", body_no(body_id)),
                function.name.0.clone(),
                stmt_label(hir, body, stmt_id),
            ));
        }
    }

    if rows.is_empty() {
        html.push_str("<p class=\"muted\">No statements.</p>");
        return collapsible_section("Statements", false, &html);
    }

    html.push_str(
        "<table><thead><tr><th>stmt</th><th>function</th><th>message</th></tr></thead><tbody>",
    );
    for (id, function, message) in rows {
        html.push_str("<tr><td>");
        html.push_str(&escape(&id));
        html.push_str("</td><td>");
        html.push_str(&escape(&function));
        html.push_str("</td><td>");
        html.push_str(&escape(&message));
        html.push_str("</td></tr>");
    }
    html.push_str("</tbody></table>");
    collapsible_section("Statements", false, &html)
}

fn type_result_type(
    hir: &HirFile,
    body_id: hir::body::BodyId,
    expr: hir::body::ExprId,
    type_result: Option<&TypeCheckResult>,
) -> Option<String> {
    type_result?
        .expr_types
        .get(&(body_id, expr))
        .map(|ty| ty.display(hir))
}

fn symbol_count(hir: &HirFile) -> usize {
    let mut count = count_items(hir, &hir.item_tree.top_level);
    for (_, function) in hir.item_tree.functions.iter() {
        count += function.params.len();
    }
    for (_, body) in hir.bodies.iter() {
        count += body
            .stmts
            .iter()
            .filter(|(_, stmt)| matches!(stmt, Stmt::Let { .. }))
            .count();
        count += body
            .pats
            .iter()
            .filter(|(_, pat)| matches!(pat, Pattern::Binding { .. }))
            .count();
    }
    count
}

fn count_items(hir: &HirFile, items: &[TopLevelItem]) -> usize {
    items
        .iter()
        .map(|item| match item {
            TopLevelItem::Module(mid) => {
                1 + hir.item_tree.modules[*mid]
                    .items
                    .as_ref()
                    .map(|children| count_items(hir, children))
                    .unwrap_or(0)
            }
            _ => 1,
        })
        .sum()
}
