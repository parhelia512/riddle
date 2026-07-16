// src/scope_graph/resolve.rs
use std::collections::HashSet;

use hir::{
    HirFile, Name,
    body::{Expr, ResolvedName},
};

use super::{DefRef, EdgeKind, Node, NodeId, RefOrigin, ScopeGraph};

/// Resolves one reference node and returns all candidate definitions.
///
/// Query rules:
/// - Try `Def` edges in the current scope first.
/// - If the current scope has a matching name, it shadows outer scopes even when the remaining
///   path fails to resolve.
/// - Only follow `Lex` / `Export` edges when the current scope has no matching definition.
pub fn resolve_reference(sg: &ScopeGraph, reference: NodeId) -> Vec<DefRef> {
    if !matches!(sg.nodes[reference], Node::Reference { .. }) {
        return vec![];
    }
    resolve_from(sg, reference, Vec::new(), &mut HashSet::new(), 64)
}

/// Resolves all expression references in `sg` and writes the selected result back to HIR.
///
/// If resolution returns multiple candidates, the first candidate in resolver traversal order is
/// selected. If no candidate is found, the expression is marked as unresolved and an E0050
/// diagnostic is emitted.
pub fn resolve_hir(hir: &mut HirFile, sg: &ScopeGraph) {
    for (nid, node) in sg.nodes.iter() {
        let Node::Reference { origin, .. } = node else {
            continue;
        };

        let candidates = resolve_reference(sg, nid);
        let resolved = candidates
            .first()
            .map(def_to_resolved_name)
            .unwrap_or(ResolvedName::Unresolved);

        // Only emit E0050 when genuinely unresolved (no candidates).
        // `def_to_resolved_name` may map some DefRef variants (PatternBinding,
        // UseAlias) to Unresolved internally; those are not user-visible errors.
        if candidates.is_empty() {
            let RefOrigin::Expr { body, expr } = origin;
            let path_text = match &hir.bodies[*body].exprs[*expr] {
                Expr::Path { path, .. } | Expr::Struct { path, .. } => path.display(),
                _ => String::new(),
            };
            let range = hir.bodies[*body]
                .source_map
                .expr_ranges
                .get(expr)
                .copied()
                .expect("lowered expression should have a source range");
            hir.bodies[*body].diagnostics.push(hir::body::Diagnostic {
                code: "E0050",
                severity: hir::body::Severity::Error,
                message: format!("unresolved name: `{}`", path_text),
                labels: vec![hir::body::SourceLabel {
                    range,
                    message: String::new(),
                    style: hir::body::LabelStyle::Primary,
                }],
                help: None,
                notes: Vec::new(),
            });
        }

        write_resolution(hir, *origin, resolved);
    }
}

fn write_resolution(hir: &mut HirFile, origin: RefOrigin, resolved_name: ResolvedName) {
    match origin {
        RefOrigin::Expr { body, expr } => match &mut hir.bodies[body].exprs[expr] {
            Expr::Path { resolved, .. } | Expr::Struct { resolved, .. } => {
                *resolved = Some(resolved_name);
            }
            _ => {}
        },
    }
}

fn def_to_resolved_name(def: &DefRef) -> ResolvedName {
    match def {
        DefRef::Function(fid) => ResolvedName::Function(*fid),
        DefRef::Struct(sid) => ResolvedName::Struct(*sid),
        DefRef::Enum(eid) => ResolvedName::Enum(*eid),
        DefRef::Trait(tid) => ResolvedName::Trait(*tid),
        DefRef::Const(cid) => ResolvedName::Const(*cid),
        DefRef::TypeAlias(tid) => ResolvedName::TypeAlias(*tid),
        DefRef::Module { id, .. } => ResolvedName::Module(*id),
        DefRef::Local { stmt } => ResolvedName::Local(*stmt),
        DefRef::PatternBinding { .. } => ResolvedName::Unresolved,
        DefRef::Param { index, .. } => ResolvedName::Param(*index),
        DefRef::LambdaParam { lambda, index, .. } => ResolvedName::LambdaParam {
            lambda: *lambda,
            index: *index,
        },
        DefRef::ConstParam { .. } => ResolvedName::Unresolved,
        DefRef::UseAlias { .. } => ResolvedName::Unresolved,
        DefRef::EnumVariant { enum_id, index } => ResolvedName::EnumVariant(*enum_id, *index),
    }
}

fn resolve_from(
    sg: &ScopeGraph,
    node: NodeId,
    stack: Vec<Name>,
    visited: &mut HashSet<(NodeId, Vec<Name>)>,
    fuel: u32,
) -> Vec<DefRef> {
    if fuel == 0 {
        return vec![];
    }
    if !visited.insert((node, stack.clone())) {
        return vec![];
    }

    match &sg.nodes[node] {
        Node::Scope(_) => resolve_scope(sg, node, stack, visited, fuel),
        Node::PopSymbol { name, define } => resolve_pop(sg, name, define, stack, visited, fuel),
        Node::JumpToScope { target } => resolve_from(sg, *target, stack, visited, fuel - 1),
        Node::PushSymbol { name } => {
            let mut next_stack = stack;
            next_stack.push(name.clone());
            resolve_out_edges(sg, node, next_stack, visited, fuel)
        }
        Node::Reference { .. } => resolve_out_edges(sg, node, stack, visited, fuel),
        Node::Tombstone => vec![],
    }
}

fn resolve_scope(
    sg: &ScopeGraph,
    scope: NodeId,
    stack: Vec<Name>,
    visited: &mut HashSet<(NodeId, Vec<Name>)>,
    fuel: u32,
) -> Vec<DefRef> {
    let Some(out) = sg.out_edges.get(&scope) else {
        return vec![];
    };

    let wanted = stack.last();
    let mut matching_def_seen = false;
    let mut results = Vec::new();

    let mut eids = out.clone();
    eids.sort_by_key(|eid| -(sg.edges[*eid].precedence as i32));

    for eid in &eids {
        let edge = sg.edges[*eid];
        if edge.kind != EdgeKind::Def {
            continue;
        }
        let Node::PopSymbol { name, .. } = &sg.nodes[edge.to] else {
            continue;
        };
        if Some(name) != wanted {
            continue;
        }

        matching_def_seen = true;
        results.extend(resolve_from(sg, edge.to, stack.clone(), visited, fuel - 1));
    }

    if matching_def_seen {
        return results;
    }

    for eid in eids {
        let edge = sg.edges[eid];
        match edge.kind {
            EdgeKind::Lex | EdgeKind::Export => {
                results.extend(resolve_from(sg, edge.to, stack.clone(), visited, fuel - 1));
            }
            EdgeKind::Def => {}
        }
    }

    results
}

fn resolve_pop(
    sg: &ScopeGraph,
    name: &Name,
    define: &DefRef,
    stack: Vec<Name>,
    visited: &mut HashSet<(NodeId, Vec<Name>)>,
    fuel: u32,
) -> Vec<DefRef> {
    if stack.last() != Some(name) {
        return vec![];
    }

    let mut remaining = stack;
    remaining.pop();

    match define {
        DefRef::UseAlias { rewrite_to, anchor } => {
            // `use foo::bar as baz; baz::qux` rewrites to `foo::bar::qux`.
            // The stack bottom stores the rightmost path segments, so keep the remaining segments
            // first and then push `[bar, foo]`; the next lookup will match `foo` first.
            remaining.extend(rewrite_to.iter().rev().cloned());
            resolve_from(sg, *anchor, remaining, visited, fuel - 1)
        }
        DefRef::PatternBinding { .. } => {
            if remaining.is_empty() {
                vec![define.clone()]
            } else {
                vec![]
            }
        }
        DefRef::Module { enter, .. } => {
            if remaining.is_empty() {
                vec![define.clone()]
            } else {
                resolve_from(sg, *enter, remaining, visited, fuel - 1)
            }
        }
        DefRef::Struct(sid) => {
            if remaining.is_empty() {
                vec![define.clone()]
            } else if let Some(scope) = sg.impl_scopes_by_struct.get(sid) {
                resolve_from(sg, *scope, remaining, visited, fuel - 1)
            } else {
                vec![]
            }
        }
        DefRef::Enum(eid) => {
            if remaining.is_empty() {
                vec![define.clone()]
            } else if let Some(scope) = sg.variant_scopes_by_enum.get(eid) {
                resolve_from(sg, *scope, remaining, visited, fuel - 1)
            } else {
                vec![]
            }
        }
        _ => {
            if remaining.is_empty() {
                vec![define.clone()]
            } else {
                vec![]
            }
        }
    }
}

fn resolve_out_edges(
    sg: &ScopeGraph,
    node: NodeId,
    stack: Vec<Name>,
    visited: &mut HashSet<(NodeId, Vec<Name>)>,
    fuel: u32,
) -> Vec<DefRef> {
    let Some(out) = sg.out_edges.get(&node) else {
        return vec![];
    };

    let mut results = Vec::new();
    let mut eids = out.clone();
    eids.sort_by_key(|eid| -(sg.edges[*eid].precedence as i32));

    for eid in eids {
        let edge = sg.edges[eid];
        results.extend(resolve_from(sg, edge.to, stack.clone(), visited, fuel - 1));
    }

    results
}
