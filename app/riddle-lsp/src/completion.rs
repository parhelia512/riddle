use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use frontend::syntax_kind::SyntaxKind;
use hir::body::{BodyId, Expr, Stmt};
use hir::item_tree::{
    HirFunction, HirTypeRef, HirUseTree, HirUseTreeKind, StructId, TopLevelItem, Visibility,
};
use lsp_types::{CompletionItem, CompletionItemKind, CompletionItemLabelDetails};
use riddlec::{
    pipeline::{CompileOptions, CompileResult},
    proc_macro::{ProcMacroKind, STANDARD_DERIVE_MACROS, STANDARD_FUNCTION_MACROS},
};
use rowan::{TextRange, TextSize};
use scope_graph::resolve::{exported_definitions, resolve_path_at_reference, visible_definitions};
use scope_graph::{DefRef, Node, NodeId, RefOrigin, ScopeGraph};

use crate::{
    server::Document,
    session::AnalysisSessions,
    text::{is_identifier_continue, offset_for_position},
};

const COMPLETION_MARKER: &str = "__riddle_completion";
const COMPLETION_KEYWORDS: &[&str] = &[
    "let", "fun", "struct", "if", "else", "while", "break", "continue", "return", "as", "self",
    "mod", "use", "mut", "pub", "super", "crate", "enum", "trait", "impl", "match", "const",
    "type", "extern", "unsafe", "safe", "for", "in", "where", "move", "true", "false",
];
pub(crate) const BUILTIN_TYPES: &[&str] = &[
    "bool", "char", "str", "i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize",
    "f32", "f64",
];

pub(crate) fn completion_trigger_characters() -> Vec<String> {
    vec![".".into(), ":".into()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionContext {
    General,
    Type,
    Member,
    Associated,
}

struct CompletionSite {
    start: usize,
    end: usize,
    prefix: String,
    context: CompletionContext,
    macro_kind: Option<ProcMacroKind>,
}

pub fn completion_items_for_document(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document>,
    position: lsp_types::Position,
    compile_options: CompileOptions,
    sessions: &AnalysisSessions,
    fallback_sessions: &AnalysisSessions,
    cancelled: impl Fn() -> bool,
) -> std::result::Result<Option<Vec<CompletionItem>>, String> {
    if cancelled() {
        return Ok(None);
    }
    let document = docs
        .get(uri)
        .ok_or_else(|| "document is not open".to_string())?;
    let site = completion_site(&document.text, position)
        .ok_or_else(|| "completion position is outside the document".to_string())?;
    let mut marked = marked_completion_source(&document.text, &site);

    let mut overlays = docs
        .iter()
        .filter_map(|(uri, document)| {
            uri.to_file_path()
                .ok()
                .map(|path| (path, document.text.clone()))
        })
        .collect::<HashMap<_, _>>();
    if let Ok(path) = uri.to_file_path() {
        overlays.insert(path.clone(), marked.clone());
        if let Some(root) = clue::find_project_root(&path) {
            let session = sessions.project(&root);
            let mut session = session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if cancelled() {
                return Ok(None);
            }
            let mut analyze = |overlays: &HashMap<PathBuf, String>| {
                if site.context == CompletionContext::Member {
                    clue::check_project_with_session_cancellable(
                        &root,
                        overlays,
                        compile_options,
                        &mut session,
                        &cancelled,
                    )
                } else {
                    clue::resolve_project_with_session_cancellable(
                        &root,
                        overlays,
                        compile_options,
                        &mut session,
                        &cancelled,
                    )
                }
            };
            let Some(mut analysis) = analyze(&overlays).map_err(|error| error.to_string())? else {
                return Ok(None);
            };
            if analysis.result.hir.is_none() && completion_needs_semicolon(&marked, &site) {
                marked.insert(site.start + COMPLETION_MARKER.len(), ';');
                overlays.insert(path.clone(), marked.clone());
                let Some(recovered) = analyze(&overlays).map_err(|error| error.to_string())? else {
                    return Ok(None);
                };
                analysis = recovered;
            }
            if cancelled() {
                return Ok(None);
            }
            // If the marker wasn't resolved as a scope-graph reference (e.g. the
            // cursor is at the start of a fresh statement), re-analyse the original
            // source to recover global-scope completions.  Use `fallback_sessions`
            // so the two IncrementalParsers never thrash each other's cache.
            let fallback_result = if matches!(
                site.context,
                CompletionContext::General | CompletionContext::Type
            ) && analysis
                .result
                .hir
                .as_ref()
                .zip(analysis.result.scope_graph.as_ref())
                .is_some_and(|(_, graph)| completion_marker_reference(graph).is_none())
            {
                let fb_session = fallback_sessions.project(&root);
                let mut fb_session = fb_session.lock().unwrap_or_else(|p| p.into_inner());
                // Restore the original (non-marked) overlay so the fallback
                // session sees the unmodified source.
                let mut fb_overlays = overlays.clone();
                fb_overlays.insert(path.clone(), document.text.clone());
                clue::resolve_project_with_session_cancellable(
                    &root,
                    &fb_overlays,
                    compile_options,
                    &mut fb_session,
                    &cancelled,
                )
                .ok()
                .flatten()
            } else {
                None
            };
            if cancelled() {
                return Ok(None);
            }
            let mut items = completion_items_from_result(
                &analysis.result,
                &site,
                fallback_result.as_ref().and_then(|a| a.result.hir.as_ref()),
            );
            collect_standard_macro_completions(&site, &mut items);
            collect_macro_completions(&analysis, &path, &site, &mut items);
            items.sort_by(|left, right| left.label.cmp(&right.label));
            items.dedup_by(|left, right| left.label == right.label && left.kind == right.kind);
            return Ok(Some(items));
        }
    }

    let session = sessions.standalone(uri);
    let mut session = session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cancelled() {
        return Ok(None);
    }
    let result = if site.context == CompletionContext::Member {
        session.check_with_options_cancellable(&marked, compile_options, &cancelled)
    } else {
        session.resolve_with_options_cancellable(&marked, compile_options, &cancelled)
    };
    let Some(mut result) = result else {
        return Ok(None);
    };
    if result.hir.is_none() && completion_needs_semicolon(&marked, &site) {
        marked.insert(site.start + COMPLETION_MARKER.len(), ';');
        let recovered = if site.context == CompletionContext::Member {
            session.check_with_options_cancellable(&marked, compile_options, &cancelled)
        } else {
            session.resolve_with_options_cancellable(&marked, compile_options, &cancelled)
        };
        let Some(recovered) = recovered else {
            return Ok(None);
        };
        result = recovered;
    }
    if cancelled() {
        return Ok(None);
    }
    // Fallback: if the marker wasn't resolved as a reference in the scope graph
    // (e.g. typing at the start of a statement), re-analyse the original source
    // so that global definitions are still offered.  Use `fallback_sessions` so
    // the two IncrementalParsers never thrash each other's cache.
    let fallback_result = if matches!(
        site.context,
        CompletionContext::General | CompletionContext::Type
    ) && result
        .hir
        .as_ref()
        .zip(result.scope_graph.as_ref())
        .is_some_and(|(_, graph)| completion_marker_reference(graph).is_none())
    {
        let fb_session = fallback_sessions.standalone(uri);
        let mut fb_session = fb_session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fb_session.resolve_with_options_cancellable(&document.text, compile_options, &cancelled)
    } else {
        None
    };
    if cancelled() {
        return Ok(None);
    }
    let mut items = completion_items_from_result(
        &result,
        &site,
        fallback_result.as_ref().and_then(|r| r.hir.as_ref()),
    );
    collect_standard_macro_completions(&site, &mut items);
    items.sort_by(|left, right| left.label.cmp(&right.label));
    items.dedup_by(|left, right| left.label == right.label && left.kind == right.kind);
    Ok(Some(items))
}

#[cfg(feature = "test-support")]
pub fn completion_items_for_source(
    source: &str,
    position: lsp_types::Position,
    compile_options: CompileOptions,
) -> Vec<CompletionItem> {
    let Some(site) = completion_site(source, position) else {
        return Vec::new();
    };
    let mut analyzed_source = marked_completion_source(source, &site);
    let mut resolved = riddlec::pipeline::resolve_with_options(&analyzed_source, compile_options);
    let marker_end = site.start + COMPLETION_MARKER.len();
    if resolved.hir.is_none() && !analyzed_source[marker_end..].trim_start().starts_with(';') {
        analyzed_source.insert(marker_end, ';');
        resolved = riddlec::pipeline::resolve_with_options(&analyzed_source, compile_options);
    }
    let fallback = (matches!(
        site.context,
        CompletionContext::General | CompletionContext::Type
    ) && resolved
        .hir
        .as_ref()
        .zip(resolved.scope_graph.as_ref())
        .is_some_and(|(_, graph)| completion_marker_reference(graph).is_none()))
    .then(|| riddlec::pipeline::resolve_with_options(source, compile_options));
    let mut items = completion_items_from_result(
        &resolved,
        &site,
        fallback.as_ref().and_then(|result| result.hir.as_ref()),
    );
    collect_standard_macro_completions(&site, &mut items);
    items
}

fn completion_site(source: &str, position: lsp_types::Position) -> Option<CompletionSite> {
    let offset = offset_for_position(source, position)?;
    let start = identifier_start(source, offset);
    let end = identifier_end(source, offset);
    let before = &source[..start];
    let context = if before.ends_with('.') {
        CompletionContext::Member
    } else if before.ends_with("::") {
        CompletionContext::Associated
    } else if is_type_position(source, start, end) {
        CompletionContext::Type
    } else {
        CompletionContext::General
    };
    Some(CompletionSite {
        start,
        end,
        prefix: source[start..offset].into(),
        context,
        macro_kind: macro_completion_kind(source, start, end),
    })
}

fn macro_completion_kind(source: &str, start: usize, end: usize) -> Option<ProcMacroKind> {
    let mut marked = source.to_string();
    marked.replace_range(start..end, COMPLETION_MARKER);
    let tokens = frontend::lexer::lex(&marked);
    let (events, tokens, errors, parsed_source) =
        frontend::parser::Parser::new(&marked, tokens).parse();
    let parse = frontend::tree_builder::build_tree(events, tokens, parsed_source, errors);
    let marker_range = TextRange::new(
        TextSize::from(start as u32),
        TextSize::from((start + COMPLETION_MARKER.len()) as u32),
    );
    let marker = parse
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.text_range() == marker_range)?;
    for node in marker.parent_ancestors() {
        match node.kind() {
            SyntaxKind::MacroCall => return Some(ProcMacroKind::FunctionLike),
            SyntaxKind::Attribute => {
                let compact = node
                    .text()
                    .to_string()
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect::<String>();
                return Some(if compact.starts_with("#[derive(") {
                    ProcMacroKind::Derive
                } else {
                    ProcMacroKind::Attribute
                });
            }
            _ => {}
        }
    }
    None
}

fn collect_macro_completions(
    analysis: &clue::ProjectAnalysis,
    path: &Path,
    site: &CompletionSite,
    items: &mut Vec<CompletionItem>,
) {
    let Some(kind) = site.macro_kind else {
        return;
    };
    let path = crate::text::normalized_path(path.to_owned());
    for occurrence in analysis
        .macro_occurrences
        .iter()
        .filter(|occurrence| occurrence.is_declaration && occurrence.kind == kind)
    {
        let range = TextRange::new(
            (occurrence.range.start as u32).into(),
            (occurrence.range.end as u32).into(),
        );
        let Some(mapped) = analysis.macro_source_map.map_range(range) else {
            continue;
        };
        if crate::text::normalized_path(mapped.path.to_owned()) != path
            || !occurrence
                .name
                .to_lowercase()
                .starts_with(&site.prefix.to_lowercase())
        {
            continue;
        }
        let detail = match kind {
            ProcMacroKind::Derive => "derive proc macro",
            ProcMacroKind::Attribute => "attribute proc macro",
            ProcMacroKind::FunctionLike => "function-like proc macro",
        };
        items.push(completion_item(
            &occurrence.name,
            CompletionItemKind::FUNCTION,
            Some(detail.into()),
        ));
    }
}

fn collect_standard_macro_completions(site: &CompletionSite, items: &mut Vec<CompletionItem>) {
    match site.macro_kind {
        Some(ProcMacroKind::FunctionLike) => {
            for name in STANDARD_FUNCTION_MACROS {
                if name.to_lowercase().starts_with(&site.prefix.to_lowercase()) {
                    items.push(completion_item(
                        name,
                        CompletionItemKind::FUNCTION,
                        Some("standard macro".into()),
                    ));
                }
            }
        }
        Some(ProcMacroKind::Derive) => {
            for name in STANDARD_DERIVE_MACROS {
                if name.to_lowercase().starts_with(&site.prefix.to_lowercase()) {
                    items.push(completion_item(
                        name,
                        CompletionItemKind::FUNCTION,
                        Some("standard derive macro".into()),
                    ));
                }
            }
        }
        _ => {}
    }
}

fn is_type_position(source: &str, start: usize, end: usize) -> bool {
    let mut marked = source.to_string();
    marked.replace_range(start..end, COMPLETION_MARKER);
    let tokens = frontend::lexer::lex(&marked);
    let (events, tokens, errors, parsed_source) =
        frontend::parser::Parser::new(&marked, tokens).parse();
    let parse = frontend::tree_builder::build_tree(events, tokens, parsed_source, errors);
    let marker_range = TextRange::new(
        TextSize::from(start as u32),
        TextSize::from((start + COMPLETION_MARKER.len()) as u32),
    );
    let Some(marker) = parse
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.text_range() == marker_range)
    else {
        return false;
    };

    let mut parent = marker.parent();
    while let Some(node) = parent {
        if node.kind() == SyntaxKind::NamedType {
            return true;
        }
        parent = node.parent();
    }
    false
}

fn marked_completion_source(source: &str, site: &CompletionSite) -> String {
    let mut marked = source.to_string();
    marked.replace_range(site.start..site.end, COMPLETION_MARKER);
    marked
}

fn completion_needs_semicolon(marked: &str, site: &CompletionSite) -> bool {
    !marked[site.start + COMPLETION_MARKER.len()..]
        .trim_start()
        .starts_with(';')
}

fn completion_items_from_result(
    resolved: &CompileResult,
    site: &CompletionSite,
    fallback_hir: Option<&hir::HirFile>,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    if site.context == CompletionContext::General {
        items.extend(COMPLETION_KEYWORDS.iter().map(|keyword| {
            completion_item(keyword, CompletionItemKind::KEYWORD, Some("keyword".into()))
        }));
    }
    if matches!(
        site.context,
        CompletionContext::General | CompletionContext::Type
    ) {
        items.extend(BUILTIN_TYPES.iter().map(|ty| {
            completion_item(
                ty,
                CompletionItemKind::TYPE_PARAMETER,
                Some("builtin type".into()),
            )
        }));
    }

    if let (Some(hir), Some(scope_graph)) = (resolved.hir.as_ref(), resolved.scope_graph.as_ref()) {
        let marker = completion_marker_reference(scope_graph);
        match site.context {
            CompletionContext::General => {
                if let Some(marker) = marker {
                    collect_visible_completions(
                        hir,
                        scope_graph,
                        marker.reference,
                        marker.body,
                        &mut items,
                    );
                } else {
                    collect_global_completions(fallback_hir.unwrap_or(hir), &mut items);
                }
            }
            CompletionContext::Type => {
                collect_type_completions(hir, scope_graph, marker, fallback_hir, &mut items);
            }
            CompletionContext::Member => {
                let checked_types;
                let types = if resolved.type_result.expr_types.is_empty() {
                    checked_types = type_checker::check_hir(hir);
                    &checked_types
                } else {
                    &resolved.type_result
                };
                collect_member_completions(hir, types, &mut items);
            }
            CompletionContext::Associated => {
                if let Some(marker) = marker {
                    collect_resolved_associated_completions(
                        hir,
                        scope_graph,
                        marker.reference,
                        marker.body,
                        marker.segments,
                        &mut items,
                    );
                }
            }
        }
    }

    let normalized_prefix = site.prefix.to_lowercase();
    items.retain(|item| item.label.to_lowercase().starts_with(&normalized_prefix));
    items.sort_by(|left, right| left.label.cmp(&right.label));
    items.dedup_by(|left, right| left.label == right.label && left.kind == right.kind);
    items
}

#[derive(Clone, Copy)]
struct CompletionMarker<'a> {
    reference: NodeId,
    body: Option<BodyId>,
    origin: RefOrigin,
    segments: &'a [hir::Name],
}

fn completion_marker_reference(scope_graph: &ScopeGraph) -> Option<CompletionMarker<'_>> {
    scope_graph.nodes.iter().find_map(|(reference, node)| {
        let Node::Reference {
            segments, origin, ..
        } = node
        else {
            return None;
        };
        (segments
            .last()
            .is_some_and(|name| name.0 == COMPLETION_MARKER))
        .then_some(CompletionMarker {
            reference,
            body: match origin {
                RefOrigin::Expr { body, .. } => Some(*body),
                RefOrigin::Type { .. } => None,
            },
            origin: *origin,
            segments,
        })
    })
}

fn collect_visible_completions(
    hir: &hir::HirFile,
    scope_graph: &ScopeGraph,
    reference: NodeId,
    body: Option<BodyId>,
    out: &mut Vec<CompletionItem>,
) {
    for (name, definition) in visible_definitions(scope_graph, reference) {
        push_definition_completion(hir, body, &name.0, &definition, out);
    }
}

fn push_definition_completion(
    hir: &hir::HirFile,
    body: Option<BodyId>,
    label: &str,
    definition: &DefRef,
    out: &mut Vec<CompletionItem>,
) {
    match definition {
        DefRef::Function(id) => out.push(function_completion_named(
            &hir.item_tree.functions[*id],
            CompletionItemKind::FUNCTION,
            label,
        )),
        DefRef::Struct(id) => out.push(completion_item(
            label,
            CompletionItemKind::STRUCT,
            Some(format!("struct {}", hir.item_tree.structs[*id].name.0)),
        )),
        DefRef::Enum(id) => out.push(completion_item(
            label,
            CompletionItemKind::ENUM,
            Some(format!("enum {}", hir.item_tree.enums[*id].name.0)),
        )),
        DefRef::Trait(id) => out.push(completion_item(
            label,
            CompletionItemKind::INTERFACE,
            Some(format!("trait {}", hir.item_tree.traits[*id].name.0)),
        )),
        DefRef::Const(id) => out.push(completion_item(
            label,
            CompletionItemKind::CONSTANT,
            Some(hir.item_tree.consts[*id].ty.display()),
        )),
        DefRef::TypeAlias(id) => out.push(completion_item(
            label,
            CompletionItemKind::TYPE_PARAMETER,
            hir.item_tree.type_aliases[*id]
                .ty
                .as_ref()
                .map(HirTypeRef::display),
        )),
        DefRef::Module { .. } => out.push(completion_item(
            label,
            CompletionItemKind::MODULE,
            Some(format!("mod {label}")),
        )),
        DefRef::PatternBinding { id, .. } if body.is_some() => {
            // A `let` records its annotation on the statement; other patterns
            // (match arms, `for`) only get their type from inference.
            let ty = let_stmt_of_pattern(&hir.bodies[body.unwrap()], id.pattern)
                .filter(|ty| **ty != HirTypeRef::Unknown)
                .map(HirTypeRef::display);
            out.push(completion_item(label, CompletionItemKind::VARIABLE, ty));
        }
        DefRef::PatternBinding { .. } => {}
        DefRef::Param { fn_id, index } => {
            let param = &hir.item_tree.functions[*fn_id].params[*index];
            out.push(completion_item(
                label,
                CompletionItemKind::VARIABLE,
                Some(param.ty.display()),
            ));
        }
        DefRef::LambdaParam {
            body_id,
            lambda,
            index,
        } => {
            if let Expr::Lambda { params, .. } = &hir.bodies[*body_id].exprs[*lambda] {
                out.push(completion_item(
                    label,
                    CompletionItemKind::VARIABLE,
                    Some(params[*index].ty.display()),
                ));
            }
        }
        DefRef::ConstParam { .. } => out.push(completion_item(
            label,
            CompletionItemKind::TYPE_PARAMETER,
            Some("const parameter".into()),
        )),
        DefRef::UseAlias { .. } => out.push(completion_item(
            label,
            CompletionItemKind::REFERENCE,
            Some(format!("use {label}")),
        )),
        DefRef::EnumVariant { enum_id, index } => {
            let enumeration = &hir.item_tree.enums[*enum_id];
            let variant = &enumeration.variants[*index];
            out.push(completion_item(
                label,
                CompletionItemKind::ENUM_MEMBER,
                Some(format!("{}::{}", enumeration.name.0, variant.name.0)),
            ));
        }
    }
}

/// The type annotation of the `let` whose pattern is `pat`, if any.
fn let_stmt_of_pattern(body: &hir::body::Body, pat: hir::body::PatId) -> Option<&HirTypeRef> {
    body.stmts.iter().find_map(|(_, stmt)| match stmt {
        Stmt::Let {
            pat: let_pat, ty, ..
        } if *let_pat == pat => Some(ty),
        _ => None,
    })
}

fn collect_resolved_associated_completions(
    hir: &hir::HirFile,
    scope_graph: &ScopeGraph,
    reference: NodeId,
    body: Option<BodyId>,
    marker_segments: &[hir::Name],
    out: &mut Vec<CompletionItem>,
) {
    let qualifier = &marker_segments[..marker_segments.len().saturating_sub(1)];
    for definition in resolve_path_at_reference(scope_graph, reference, qualifier) {
        let definitions = match definition {
            DefRef::Module { enter, .. } => exported_definitions(scope_graph, enter),
            DefRef::Struct(id) => scope_graph
                .impl_scopes_by_struct
                .get(&id)
                .map(|scope| exported_definitions(scope_graph, *scope))
                .unwrap_or_default(),
            DefRef::Enum(id) => scope_graph
                .variant_scopes_by_enum
                .get(&id)
                .map(|scope| exported_definitions(scope_graph, *scope))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for (name, associated) in definitions {
            if let DefRef::Function(id) = &associated
                && !function_is_visible(hir, body, *id)
            {
                continue;
            }
            if let DefRef::Function(id) = &associated
                && hir.item_tree.functions[*id]
                    .params
                    .first()
                    .is_some_and(|param| param.name.0 == "self")
            {
                continue;
            }
            push_definition_completion(hir, body, &name.0, &associated, out);
        }
    }
}

fn collect_global_completions(hir: &hir::HirFile, out: &mut Vec<CompletionItem>) {
    for item in &hir.item_tree.top_level {
        push_top_level_item(hir, *item, true, out);
    }
    for (_, use_item) in hir.item_tree.uses.iter() {
        if use_item.visibility.is_public() {
            push_use_tree(&use_item.tree, out);
        }
    }
}

fn collect_type_completions(
    hir: &hir::HirFile,
    scope_graph: &ScopeGraph,
    marker: Option<CompletionMarker<'_>>,
    fallback_hir: Option<&hir::HirFile>,
    out: &mut Vec<CompletionItem>,
) {
    let mut candidates = Vec::new();
    if let Some(marker) = marker {
        collect_visible_completions(
            hir,
            scope_graph,
            marker.reference,
            marker.body,
            &mut candidates,
        );
        collect_generic_type_completions(hir, marker, &mut candidates);
    } else {
        collect_global_completions(fallback_hir.unwrap_or(hir), &mut candidates);
    }
    out.extend(candidates.into_iter().filter(|item| {
        matches!(
            item.kind,
            Some(
                CompletionItemKind::STRUCT
                    | CompletionItemKind::ENUM
                    | CompletionItemKind::INTERFACE
                    | CompletionItemKind::TYPE_PARAMETER
                    | CompletionItemKind::MODULE
                    | CompletionItemKind::REFERENCE
            )
        )
    }));
}

fn collect_generic_type_completions(
    hir: &hir::HirFile,
    marker: CompletionMarker<'_>,
    out: &mut Vec<CompletionItem>,
) {
    let RefOrigin::Type { range } = marker.origin else {
        return;
    };
    for (function_id, function) in hir.item_tree.functions.iter() {
        let contains_marker = function
            .params
            .iter()
            .any(|param| type_ref_contains_range(&param.ty, range))
            || function
                .ret_type
                .as_ref()
                .is_some_and(|ty| type_ref_contains_range(ty, range))
            || function.generic_bounds.iter().any(|bound| {
                type_ref_contains_range(&bound.target_ty, range)
                    || type_ref_contains_range(&bound.trait_ty, range)
            });
        if !contains_marker {
            continue;
        }
        let impl_generics = hir.item_tree.impls.iter().find_map(|(_, implementation)| {
            implementation
                .methods
                .contains(&function_id)
                .then_some(implementation.generics.as_slice())
        });
        for generic in impl_generics
            .into_iter()
            .flatten()
            .chain(function.generics.iter())
        {
            out.push(completion_item(
                &generic.0,
                CompletionItemKind::TYPE_PARAMETER,
                Some("type parameter".into()),
            ));
        }
        return;
    }
}

fn type_ref_contains_range(ty: &HirTypeRef, range: TextRange) -> bool {
    match ty {
        HirTypeRef::Named(path) => {
            path.range == range
                || path
                    .type_args
                    .iter()
                    .any(|arg| type_ref_contains_range(arg, range))
        }
        HirTypeRef::Ref(inner, _)
        | HirTypeRef::Ptr { inner, .. }
        | HirTypeRef::Slice(inner)
        | HirTypeRef::Array(inner, _) => type_ref_contains_range(inner, range),
        HirTypeRef::Tuple(elements) => elements
            .iter()
            .any(|element| type_ref_contains_range(element, range)),
        HirTypeRef::ImplTrait {
            trait_ty, callable, ..
        } => {
            type_ref_contains_range(trait_ty, range)
                || callable.as_ref().is_some_and(|signature| {
                    signature
                        .params
                        .iter()
                        .any(|param| type_ref_contains_range(param, range))
                        || type_ref_contains_range(&signature.ret, range)
                })
        }
        HirTypeRef::Never | HirTypeRef::Const(_) | HirTypeRef::Unknown | HirTypeRef::Error => false,
    }
}

fn collect_member_completions(
    hir: &hir::HirFile,
    types: &type_checker::TypeCheckResult,
    out: &mut Vec<CompletionItem>,
) {
    let receiver = hir.bodies.iter().find_map(|(body_id, body)| {
        body.exprs.iter().find_map(|(_, expr)| {
            let Expr::FieldAccess { base, field } = expr else {
                return None;
            };
            (field.0 == COMPLETION_MARKER)
                .then(|| {
                    types
                        .expr_types
                        .get(&(body_id, *base))
                        .map(|receiver| (body_id, receiver))
                })
                .flatten()
        })
    });
    let Some((body_id, receiver)) = receiver else {
        return;
    };

    if let Some(struct_id) = receiver_struct_id(receiver) {
        let struct_item = &hir.item_tree.structs[struct_id];
        for field in &struct_item.fields {
            if !type_checker::struct_field_is_visible(hir, body_id, struct_id, &field.visibility) {
                continue;
            }
            out.push(completion_item(
                &field.name.0,
                CompletionItemKind::FIELD,
                Some(field.ty.display()),
            ));
        }
    }

    for (_, impl_item) in hir.item_tree.impls.iter() {
        if !type_ref_matches_type(hir, &impl_item.generics, &impl_item.self_ty, receiver) {
            continue;
        }
        for function_id in &impl_item.methods {
            let function = &hir.item_tree.functions[*function_id];
            if function
                .params
                .first()
                .is_some_and(|param| param.name.0 == "self")
                && (function_is_visible(hir, Some(body_id), *function_id)
                    || impl_item.trait_ty.is_some())
            {
                out.push(function_completion(function, CompletionItemKind::METHOD));
            }
        }
    }
}

fn push_top_level_item(
    hir: &hir::HirFile,
    item: TopLevelItem,
    allow_private_user_item: bool,
    out: &mut Vec<CompletionItem>,
) {
    match item {
        TopLevelItem::Function(id) => {
            let item = &hir.item_tree.functions[id];
            if visible_for_completion(
                hir,
                &item.visibility,
                item.name_range,
                allow_private_user_item,
            ) {
                out.push(function_completion(item, CompletionItemKind::FUNCTION));
            }
        }
        TopLevelItem::Struct(id) => {
            let item = &hir.item_tree.structs[id];
            if visible_for_completion(
                hir,
                &item.visibility,
                item.name_range,
                allow_private_user_item,
            ) {
                out.push(completion_item(
                    &item.name.0,
                    CompletionItemKind::STRUCT,
                    Some(format!("struct {}", item.name.0)),
                ));
            }
        }
        TopLevelItem::Module(id) => {
            let item = &hir.item_tree.modules[id];
            if allow_private_user_item || item.visibility.is_public() {
                out.push(completion_item(
                    &item.name.0,
                    CompletionItemKind::MODULE,
                    Some(format!("mod {}", item.name.0)),
                ));
            }
        }
        TopLevelItem::Enum(id) => {
            let item = &hir.item_tree.enums[id];
            if visible_for_completion(
                hir,
                &item.visibility,
                item.name_range,
                allow_private_user_item,
            ) {
                out.push(completion_item(
                    &item.name.0,
                    CompletionItemKind::ENUM,
                    Some(format!("enum {}", item.name.0)),
                ));
            }
        }
        TopLevelItem::Trait(id) => {
            let item = &hir.item_tree.traits[id];
            if allow_private_user_item || item.visibility.is_public() {
                out.push(completion_item(
                    &item.name.0,
                    CompletionItemKind::INTERFACE,
                    Some(format!("trait {}", item.name.0)),
                ));
            }
        }
        TopLevelItem::Const(id) => {
            let item = &hir.item_tree.consts[id];
            if visible_for_completion(
                hir,
                &item.visibility,
                item.name_range,
                allow_private_user_item,
            ) {
                out.push(completion_item(
                    &item.name.0,
                    CompletionItemKind::CONSTANT,
                    Some(item.ty.display()),
                ));
            }
        }
        TopLevelItem::TypeAlias(id) => {
            let item = &hir.item_tree.type_aliases[id];
            if visible_for_completion(
                hir,
                &item.visibility,
                item.name_range,
                allow_private_user_item,
            ) {
                out.push(completion_item(
                    &item.name.0,
                    CompletionItemKind::TYPE_PARAMETER,
                    item.ty.as_ref().map(HirTypeRef::display),
                ));
            }
        }
        TopLevelItem::Use(_) | TopLevelItem::Impl(_) => {}
    }
}

fn push_use_tree(tree: &HirUseTree, out: &mut Vec<CompletionItem>) {
    match &tree.kind {
        HirUseTreeKind::Simple { alias } => {
            let name = alias
                .as_ref()
                .or_else(|| tree.prefix.segments.last())
                .map(|name| name.0.as_str());
            if let Some(name) = name {
                out.push(completion_item(
                    name,
                    CompletionItemKind::REFERENCE,
                    Some(format!("use {}", tree.prefix.display())),
                ));
            }
        }
        HirUseTreeKind::List(items) => {
            for item in items {
                push_use_tree(item, out);
            }
        }
        HirUseTreeKind::Glob => {}
    }
}

fn function_completion(function: &HirFunction, kind: CompletionItemKind) -> CompletionItem {
    function_completion_named(function, kind, &function.name.0)
}

fn function_completion_named(
    function: &HirFunction,
    kind: CompletionItemKind,
    label: &str,
) -> CompletionItem {
    let params = function
        .params
        .iter()
        .map(|param| {
            if param.name.0 == "self" {
                match &param.ty {
                    HirTypeRef::Ref(_, true) => "&mut self".into(),
                    HirTypeRef::Ref(_, false) => "&self".into(),
                    _ => "self".into(),
                }
            } else {
                format!("{}: {}", param.name.0, param.ty.display())
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let ret = function
        .ret_type
        .as_ref()
        .map(HirTypeRef::display)
        .unwrap_or_else(|| "()".into());
    let mut item = completion_item(
        label,
        kind,
        Some(format!("fun {}({params}) -> {ret}", function.name.0)),
    );
    item.label_details = Some(CompletionItemLabelDetails {
        detail: Some(format!("({params})")),
        description: Some(ret),
    });
    item.insert_text = Some(label.into());
    item
}

fn completion_item(
    label: &str,
    kind: CompletionItemKind,
    detail: Option<String>,
) -> CompletionItem {
    CompletionItem {
        label: label.into(),
        kind: Some(kind),
        detail,
        ..CompletionItem::default()
    }
}

fn visible_for_completion(
    hir: &hir::HirFile,
    visibility: &Visibility,
    range: TextRange,
    allow_private_user_item: bool,
) -> bool {
    visibility.is_public() || (allow_private_user_item && hir.package_for_range(range).is_some())
}

fn function_is_visible(
    hir: &hir::HirFile,
    body: Option<BodyId>,
    function_id: hir::item_tree::FunctionId,
) -> bool {
    let function = &hir.item_tree.functions[function_id];
    function.visibility.is_public()
        || hir.item_tree.impls.iter().any(|(_, implementation)| {
            implementation.trait_ty.is_some() && implementation.methods.contains(&function_id)
        })
        || body.is_some_and(|body| {
            type_checker::method_is_visible(hir, body, function_id, &function.visibility)
        })
}

fn receiver_struct_id(ty: &type_checker::Type) -> Option<StructId> {
    match ty {
        type_checker::Type::Struct(id, _) => Some(*id),
        type_checker::Type::Ref(inner, _) | type_checker::Type::Ptr { inner, .. } => {
            receiver_struct_id(inner)
        }
        _ => None,
    }
}

fn type_ref_matches_type(
    hir: &hir::HirFile,
    generics: &[hir::Name],
    expected: &HirTypeRef,
    actual: &type_checker::Type,
) -> bool {
    match (expected, actual) {
        (HirTypeRef::Ref(expected, _), type_checker::Type::Ref(actual, _))
        | (HirTypeRef::Ref(expected, _), type_checker::Type::Ptr { inner: actual, .. }) => {
            type_ref_matches_type(hir, generics, expected, actual)
        }
        (
            HirTypeRef::Ptr {
                inner: expected, ..
            },
            type_checker::Type::Ptr { inner: actual, .. },
        ) => type_ref_matches_type(hir, generics, expected, actual),
        (HirTypeRef::Array(expected, _), type_checker::Type::Array(actual, _)) => {
            type_ref_matches_type(hir, generics, expected, actual)
        }
        (HirTypeRef::Slice(expected), type_checker::Type::Slice(actual)) => {
            type_ref_matches_type(hir, generics, expected, actual)
        }
        (expected @ HirTypeRef::Slice(_), type_checker::Type::Ref(actual, _))
        | (expected @ HirTypeRef::Slice(_), type_checker::Type::Ptr { inner: actual, .. }) => {
            type_ref_matches_type(hir, generics, expected, actual)
        }
        (HirTypeRef::Named(_), type_checker::Type::Ref(actual, _))
        | (HirTypeRef::Named(_), type_checker::Type::Ptr { inner: actual, .. }) => {
            type_ref_matches_type(hir, generics, expected, actual)
        }
        (HirTypeRef::Named(path), _)
            if path.segments.len() == 1
                && path.type_args.is_empty()
                && generics.contains(&path.segments[0]) =>
        {
            true
        }
        (HirTypeRef::Named(_), _) => type_ref_name(expected).is_some_and(|expected| {
            let actual = actual.display(hir);
            actual.split('<').next() == Some(expected)
        }),
        _ => false,
    }
}

fn type_ref_name(ty: &HirTypeRef) -> Option<&str> {
    match ty {
        HirTypeRef::Named(path) => path.segments.last().map(|name| name.0.as_str()),
        HirTypeRef::Ref(inner, _) | HirTypeRef::Ptr { inner, .. } => type_ref_name(inner),
        _ => None,
    }
}

fn identifier_start(source: &str, offset: usize) -> usize {
    source[..offset]
        .char_indices()
        .rev()
        .find(|(_, ch)| !is_identifier_continue(*ch))
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0)
}

fn identifier_end(source: &str, offset: usize) -> usize {
    source[offset..]
        .char_indices()
        .find(|(_, ch)| !is_identifier_continue(*ch))
        .map(|(index, _)| offset + index)
        .unwrap_or(source.len())
}
