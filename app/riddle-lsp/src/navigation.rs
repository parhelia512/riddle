use std::collections::HashMap;

use frontend::syntax_kind::SyntaxKind;
use hir::{
    HirFile, Name,
    body::{BodyId, Expr, Pattern, ResolvedName},
    item_tree::{FunctionId, HirFunction, HirImpl, HirTypeRef, TraitId},
};
use lsp_types::{
    GotoDefinitionResponse, Hover, HoverContents, Location, MarkupContent, MarkupKind, Position,
};
use riddlec::pipeline::CompileOptions;
use rowan::{TextRange, TextSize};
use scope_graph::{DefRef, Node, RefOrigin, ScopeGraph, resolve::resolve_path_at_reference};
use type_checker::{Type, TypeCheckResult};

use crate::{
    analysis::{AnalysisDepth, DocumentAnalysis, analyze_document},
    completion::BUILTIN_TYPES,
    server::Document,
    session::AnalysisSessions,
    text::{LineIndex, offset_for_position},
};

struct Symbol {
    origin: TextRange,
    detail: String,
    definition: Option<TextRange>,
    implementations: Vec<TextRange>,
}

pub fn hover_for_document(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document>,
    position: Position,
    options: CompileOptions,
    sessions: &AnalysisSessions,
) -> Result<Option<Hover>, String> {
    let document = docs
        .get(uri)
        .ok_or_else(|| "document is not open".to_string())?;
    let analysis = analyze_document(uri, docs, options, sessions, AnalysisDepth::Check)?;
    Ok(hover_from_analysis(document, &analysis, position))
}

pub fn definition_for_document(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document>,
    position: Position,
    options: CompileOptions,
    sessions: &AnalysisSessions,
) -> Result<Option<GotoDefinitionResponse>, String> {
    let document = docs
        .get(uri)
        .ok_or_else(|| "document is not open".to_string())?;
    let analysis = analyze_document(uri, docs, options, sessions, AnalysisDepth::Check)?;
    Ok(definition_from_analysis(uri, document, &analysis, position))
}

pub fn implementation_for_document(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document>,
    position: Position,
    options: CompileOptions,
    sessions: &AnalysisSessions,
) -> Result<Option<GotoDefinitionResponse>, String> {
    let document = docs
        .get(uri)
        .ok_or_else(|| "document is not open".to_string())?;
    let analysis = analyze_document(uri, docs, options, sessions, AnalysisDepth::Check)?;
    Ok(implementation_from_analysis(
        uri, document, &analysis, position,
    ))
}

#[cfg(feature = "test-support")]
pub fn hover_for_source(
    source: &str,
    position: Position,
    options: CompileOptions,
) -> Option<Hover> {
    let document = Document {
        text: source.into(),
        version: Some(1),
    };
    let analysis = standalone_analysis(source, options);
    hover_from_analysis(&document, &analysis, position)
}

#[cfg(feature = "test-support")]
pub fn definition_for_source(
    source: &str,
    position: Position,
    options: CompileOptions,
) -> Option<GotoDefinitionResponse> {
    let uri = lsp_types::Url::parse("file:///riddle-navigation.rid").unwrap();
    let document = Document {
        text: source.into(),
        version: Some(1),
    };
    let analysis = standalone_analysis(source, options);
    definition_from_analysis(&uri, &document, &analysis, position)
}

#[cfg(feature = "test-support")]
pub fn implementation_for_source(
    source: &str,
    position: Position,
    options: CompileOptions,
) -> Option<GotoDefinitionResponse> {
    let uri = lsp_types::Url::parse("file:///riddle-navigation.rid").unwrap();
    let document = Document {
        text: source.into(),
        version: Some(1),
    };
    let analysis = standalone_analysis(source, options);
    implementation_from_analysis(&uri, &document, &analysis, position)
}

#[cfg(feature = "test-support")]
fn standalone_analysis(source: &str, options: CompileOptions) -> DocumentAnalysis {
    DocumentAnalysis {
        result: riddlec::pipeline::check_with_options(source, options),
        source: source.into(),
        source_map: None,
        path: None,
    }
}

fn hover_from_analysis(
    document: &Document,
    analysis: &DocumentAnalysis,
    position: Position,
) -> Option<Hover> {
    let symbol = symbol_at(&document.text, analysis, position)?;
    let range = LineIndex::new(&document.text).range(&document.text, symbol.origin)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("```riddle\n{}\n```", symbol.detail),
        }),
        range: Some(range),
    })
}

fn definition_from_analysis(
    uri: &lsp_types::Url,
    document: &Document,
    analysis: &DocumentAnalysis,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let symbol = symbol_at(&document.text, analysis, position)?;
    let location = location_for_range(uri, analysis, symbol.definition?)?;
    Some(GotoDefinitionResponse::Scalar(location))
}

fn implementation_from_analysis(
    uri: &lsp_types::Url,
    document: &Document,
    analysis: &DocumentAnalysis,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let symbol = symbol_at(&document.text, analysis, position)?;
    let mut locations = symbol
        .implementations
        .into_iter()
        .filter_map(|range| location_for_range(uri, analysis, range))
        .collect::<Vec<_>>();
    locations.sort_by(|left, right| {
        left.uri
            .as_str()
            .cmp(right.uri.as_str())
            .then_with(|| left.range.start.line.cmp(&right.range.start.line))
            .then_with(|| left.range.start.character.cmp(&right.range.start.character))
    });
    locations.dedup();
    (!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations))
}

fn location_for_range(
    current_uri: &lsp_types::Url,
    analysis: &DocumentAnalysis,
    range: TextRange,
) -> Option<Location> {
    if let Some(source_map) = &analysis.source_map {
        let mapped = source_map.map_range(range)?;
        return Some(Location::new(
            lsp_types::Url::from_file_path(mapped.path).ok()?,
            LineIndex::new(mapped.source).range(mapped.source, mapped.range)?,
        ));
    }
    Some(Location::new(
        current_uri.clone(),
        LineIndex::new(&analysis.source).range(&analysis.source, range)?,
    ))
}

fn symbol_at(
    document_source: &str,
    analysis: &DocumentAnalysis,
    position: Position,
) -> Option<Symbol> {
    let offset = offset_for_position(document_source, position)?;
    let origin = identifier_range_at(document_source, offset)?;
    let hir = analysis.result.hir.as_ref()?;
    let graph = analysis.result.scope_graph.as_ref()?;

    method_or_field_symbol(hir, &analysis.result.type_result, analysis, origin)
        .or_else(|| reference_symbol(hir, graph, &analysis.result.type_result, analysis, origin))
        .or_else(|| declaration_symbol(hir, &analysis.result.type_result, analysis, origin))
        .or_else(|| inferred_expression_symbol(hir, &analysis.result.type_result, analysis, origin))
        .or_else(|| {
            let text = &document_source[origin];
            BUILTIN_TYPES.contains(&text).then(|| Symbol {
                origin,
                detail: format!("builtin type {text}"),
                definition: None,
                implementations: Vec::new(),
            })
        })
}

fn method_or_field_symbol(
    hir: &HirFile,
    types: &TypeCheckResult,
    analysis: &DocumentAnalysis,
    origin: TextRange,
) -> Option<Symbol> {
    for (body_id, body) in hir.bodies.iter() {
        for (expr_id, expr) in body.exprs.iter() {
            let Expr::FieldAccess { base, field } = expr else {
                continue;
            };
            let Some(range) = body.source_map.expr_ranges.get(&expr_id).copied() else {
                continue;
            };
            let Some(field_range) = last_identifier_range(&analysis.source, range) else {
                continue;
            };
            if analysis.local_range(field_range) != Some(origin) {
                continue;
            }

            if let Some(call) = types.trait_method_calls.get(&(body_id, expr_id)) {
                return trait_method_symbol(
                    hir,
                    types,
                    body_id,
                    expr_id,
                    call.trait_id,
                    &call.method,
                    origin,
                );
            }
            if let Some(Type::FunctionItem { function, .. }) =
                types.expr_types.get(&(body_id, expr_id))
            {
                return Some(function_symbol(hir, *function, origin));
            }

            let Some(receiver) = types.expr_types.get(&(body_id, *base)) else {
                continue;
            };
            let Some(struct_id) = receiver_struct_id(receiver) else {
                continue;
            };
            let strukt = &hir.item_tree.structs[struct_id];
            let Some(field) = strukt
                .fields
                .iter()
                .find(|candidate| candidate.name == *field)
            else {
                continue;
            };
            return Some(Symbol {
                origin,
                detail: format!("field {}: {}", field.name.0, field.ty.display()),
                definition: Some(field.name_range),
                implementations: Vec::new(),
            });
        }
    }
    None
}

fn reference_symbol(
    hir: &HirFile,
    graph: &ScopeGraph,
    types: &TypeCheckResult,
    analysis: &DocumentAnalysis,
    origin: TextRange,
) -> Option<Symbol> {
    for (reference, node) in graph.nodes.iter() {
        let Node::Reference {
            segments,
            origin: reference_origin,
            ..
        } = node
        else {
            continue;
        };
        let Some(path_range) = reference_path_range(hir, *reference_origin) else {
            continue;
        };
        let segment_ranges = path_segment_ranges(&analysis.source, path_range, segments);
        let Some(index) = segment_ranges
            .iter()
            .position(|range| analysis.local_range(*range) == Some(origin))
        else {
            continue;
        };
        let body = match reference_origin {
            RefOrigin::Expr { body, .. } => Some(*body),
            RefOrigin::Type { .. } => None,
        };
        return resolve_path_at_reference(graph, reference, &segments[..=index])
            .into_iter()
            .find_map(|definition| {
                symbol_for_definition(hir, types, body, definition, origin, &analysis.source)
            });
    }
    None
}

fn declaration_symbol(
    hir: &HirFile,
    types: &TypeCheckResult,
    analysis: &DocumentAnalysis,
    origin: TextRange,
) -> Option<Symbol> {
    for (trait_id, tr) in hir.item_tree.traits.iter() {
        if analysis.local_range(tr.name_range) == Some(origin) {
            return Some(trait_symbol(hir, trait_id, origin));
        }
        for method in &tr.methods {
            if analysis.local_range(method.name_range) == Some(origin) {
                return Some(trait_method_declaration_symbol(
                    hir, trait_id, method, origin,
                ));
            }
        }
    }
    for (function_id, function) in hir.item_tree.functions.iter() {
        if analysis.local_range(function.name_range) == Some(origin) {
            return Some(function_symbol(hir, function_id, origin));
        }
        for parameter in &function.params {
            if parameter.name.0 != "self"
                && analysis.local_range(parameter.name_range) == Some(origin)
            {
                return Some(Symbol {
                    origin,
                    detail: format!("parameter {}: {}", parameter.name.0, parameter.ty.display()),
                    definition: Some(parameter.name_range),
                    implementations: Vec::new(),
                });
            }
        }
    }
    for (_, strukt) in hir.item_tree.structs.iter() {
        if analysis.local_range(strukt.name_range) == Some(origin) {
            return Some(Symbol {
                origin,
                detail: format_nominal("struct", &strukt.name, &strukt.generics),
                definition: Some(strukt.name_range),
                implementations: Vec::new(),
            });
        }
        for field in &strukt.fields {
            if analysis.local_range(field.name_range) == Some(origin) {
                return Some(Symbol {
                    origin,
                    detail: format!("field {}: {}", field.name.0, field.ty.display()),
                    definition: Some(field.name_range),
                    implementations: Vec::new(),
                });
            }
        }
    }
    for (_, enumeration) in hir.item_tree.enums.iter() {
        if analysis.local_range(enumeration.name_range) == Some(origin) {
            return Some(Symbol {
                origin,
                detail: format_nominal("enum", &enumeration.name, &enumeration.generics),
                definition: Some(enumeration.name_range),
                implementations: Vec::new(),
            });
        }
        for variant in &enumeration.variants {
            if analysis.local_range(variant.name_range) == Some(origin) {
                return Some(Symbol {
                    origin,
                    detail: format!("variant {}::{}", enumeration.name.0, variant.name.0),
                    definition: Some(variant.name_range),
                    implementations: Vec::new(),
                });
            }
        }
    }
    for (_, konst) in hir.item_tree.consts.iter() {
        if analysis.local_range(konst.name_range) == Some(origin) {
            return Some(Symbol {
                origin,
                detail: format!("const {}: {}", konst.name.0, konst.ty.display()),
                definition: Some(konst.name_range),
                implementations: Vec::new(),
            });
        }
    }
    for (_, alias) in hir.item_tree.type_aliases.iter() {
        if analysis.local_range(alias.name_range) == Some(origin) {
            return Some(Symbol {
                origin,
                detail: alias
                    .ty
                    .as_ref()
                    .map(|ty| format!("type {} = {}", alias.name.0, ty.display()))
                    .unwrap_or_else(|| format!("type {}", alias.name.0)),
                definition: Some(alias.name_range),
                implementations: Vec::new(),
            });
        }
    }
    for (_, module) in hir.item_tree.modules.iter() {
        if analysis.local_range(module.name_range) == Some(origin) {
            return Some(Symbol {
                origin,
                detail: format!("mod {}", module.name.0),
                definition: Some(module.name_range),
                implementations: Vec::new(),
            });
        }
    }
    for (body_id, body) in hir.bodies.iter() {
        for (pat_id, pattern) in body.pats.iter() {
            let Pattern::Binding { name, .. } = pattern else {
                continue;
            };
            let Some(pattern_range) = body.source_map.pat_ranges.get(&pat_id).copied() else {
                continue;
            };
            let Some(name_range) =
                identifier_named_in_range(&analysis.source, pattern_range, &name.0)
            else {
                continue;
            };
            if analysis.local_range(name_range) == Some(origin) {
                let ty = types
                    .pattern_binding_types
                    .iter()
                    .find_map(|((candidate_body, binding), ty)| {
                        (*candidate_body == body_id && binding.pattern == pat_id)
                            .then_some(ty.display(hir))
                    })
                    .unwrap_or_else(|| "_".into());
                return Some(Symbol {
                    origin,
                    detail: format!("let {}: {ty}", name.0),
                    definition: Some(name_range),
                    implementations: Vec::new(),
                });
            }
        }
        for (_, expr) in body.exprs.iter() {
            let Expr::Lambda { params, .. } = expr else {
                continue;
            };
            for parameter in params {
                if parameter
                    .name_range
                    .is_some_and(|range| analysis.local_range(range) == Some(origin))
                {
                    return Some(Symbol {
                        origin,
                        detail: format!(
                            "parameter {}: {}",
                            parameter.name.0,
                            parameter.ty.display()
                        ),
                        definition: parameter.name_range,
                        implementations: Vec::new(),
                    });
                }
            }
        }
    }
    None
}

fn inferred_expression_symbol(
    hir: &HirFile,
    types: &TypeCheckResult,
    analysis: &DocumentAnalysis,
    origin: TextRange,
) -> Option<Symbol> {
    let (_, ty) = types
        .expr_types
        .iter()
        .filter_map(|((body_id, expr_id), ty)| {
            let range = hir.bodies[*body_id].source_map.expr_ranges.get(expr_id)?;
            let local = analysis.local_range(*range)?;
            (local.start() <= origin.start() && origin.end() <= local.end())
                .then_some((local.len(), ty))
        })
        .min_by_key(|(length, _)| *length)?;
    Some(Symbol {
        origin,
        detail: ty.display(hir),
        definition: None,
        implementations: Vec::new(),
    })
}

fn symbol_for_definition(
    hir: &HirFile,
    types: &TypeCheckResult,
    body: Option<BodyId>,
    definition: DefRef,
    origin: TextRange,
    source: &str,
) -> Option<Symbol> {
    match definition {
        DefRef::Function(function) => Some(function_symbol(hir, function, origin)),
        DefRef::Struct(id) => {
            let item = &hir.item_tree.structs[id];
            Some(Symbol {
                origin,
                detail: format_nominal("struct", &item.name, &item.generics),
                definition: Some(item.name_range),
                implementations: Vec::new(),
            })
        }
        DefRef::Enum(id) => {
            let item = &hir.item_tree.enums[id];
            Some(Symbol {
                origin,
                detail: format_nominal("enum", &item.name, &item.generics),
                definition: Some(item.name_range),
                implementations: Vec::new(),
            })
        }
        DefRef::Trait(id) => Some(trait_symbol(hir, id, origin)),
        DefRef::Const(id) => {
            let item = &hir.item_tree.consts[id];
            Some(Symbol {
                origin,
                detail: format!("const {}: {}", item.name.0, item.ty.display()),
                definition: Some(item.name_range),
                implementations: Vec::new(),
            })
        }
        DefRef::TypeAlias(id) => {
            let item = &hir.item_tree.type_aliases[id];
            Some(Symbol {
                origin,
                detail: item
                    .ty
                    .as_ref()
                    .map(|ty| format!("type {} = {}", item.name.0, ty.display()))
                    .unwrap_or_else(|| format!("type {}", item.name.0)),
                definition: Some(item.name_range),
                implementations: Vec::new(),
            })
        }
        DefRef::Module { id, .. } => {
            let item = &hir.item_tree.modules[id];
            Some(Symbol {
                origin,
                detail: format!("mod {}", item.name.0),
                definition: Some(item.name_range),
                implementations: Vec::new(),
            })
        }
        DefRef::PatternBinding { name, id } => {
            let body = body?;
            let pattern_range = hir.bodies[body]
                .source_map
                .pat_ranges
                .get(&id.pattern)
                .copied()?;
            let name_range = identifier_named_in_range(source, pattern_range, &name.0)?;
            let ty = types
                .pattern_binding_types
                .get(&(body, id))
                .map(|ty| ty.display(hir))
                .unwrap_or_else(|| "_".into());
            Some(Symbol {
                origin,
                detail: format!("let {}: {ty}", name.0),
                definition: Some(name_range),
                implementations: Vec::new(),
            })
        }
        DefRef::Param { fn_id, index } => {
            let parameter = &hir.item_tree.functions[fn_id].params[index];
            Some(Symbol {
                origin,
                detail: format!("parameter {}: {}", parameter.name.0, parameter.ty.display()),
                definition: Some(parameter.name_range),
                implementations: Vec::new(),
            })
        }
        DefRef::LambdaParam {
            body_id,
            lambda,
            index,
        } => {
            let Expr::Lambda { params, .. } = &hir.bodies[body_id].exprs[lambda] else {
                return None;
            };
            let parameter = params.get(index)?;
            Some(Symbol {
                origin,
                detail: format!("parameter {}: {}", parameter.name.0, parameter.ty.display()),
                definition: parameter.name_range,
                implementations: Vec::new(),
            })
        }
        DefRef::ConstParam { name } => Some(Symbol {
            origin,
            detail: format!("const parameter {}", name.0),
            definition: None,
            implementations: Vec::new(),
        }),
        DefRef::EnumVariant { enum_id, index } => {
            let enumeration = &hir.item_tree.enums[enum_id];
            let variant = enumeration.variants.get(index)?;
            Some(Symbol {
                origin,
                detail: format!("variant {}::{}", enumeration.name.0, variant.name.0),
                definition: Some(variant.name_range),
                implementations: Vec::new(),
            })
        }
        DefRef::UseAlias { .. } => None,
    }
}

fn function_symbol(hir: &HirFile, function_id: FunctionId, origin: TextRange) -> Symbol {
    let function = &hir.item_tree.functions[function_id];
    if let Some((trait_id, _)) = trait_impl_for_function(hir, function_id) {
        let definition = trait_method(hir, trait_id, &function.name.0)
            .map(|method| method.name_range)
            .or(Some(function.name_range));
        return Symbol {
            origin,
            detail: format_function(function),
            definition,
            implementations: trait_method_implementations(hir, trait_id, &function.name.0),
        };
    }
    Symbol {
        origin,
        detail: format_function(function),
        definition: Some(function.name_range),
        implementations: Vec::new(),
    }
}

fn trait_symbol(hir: &HirFile, trait_id: TraitId, origin: TextRange) -> Symbol {
    let tr = &hir.item_tree.traits[trait_id];
    Symbol {
        origin,
        detail: format_nominal("trait", &tr.name, &tr.generics),
        definition: Some(tr.name_range),
        implementations: hir
            .item_tree
            .impls
            .iter()
            .filter_map(|(_, implementation)| {
                (trait_id_for_impl(hir, implementation) == Some(trait_id))
                    .then_some(implementation.self_ty_range)
            })
            .collect(),
    }
}

fn trait_method_symbol(
    hir: &HirFile,
    types: &TypeCheckResult,
    body_id: BodyId,
    expr_id: hir::body::ExprId,
    trait_id: TraitId,
    method_name: &str,
    origin: TextRange,
) -> Option<Symbol> {
    let method = trait_method(hir, trait_id, method_name)?;
    let actual = match types.expr_types.get(&(body_id, expr_id)) {
        Some(Type::FunctionItem { function, .. })
            if trait_impl_for_function(hir, *function)
                .is_some_and(|(candidate, _)| candidate == trait_id) =>
        {
            vec![hir.item_tree.functions[*function].name_range]
        }
        _ => trait_method_implementations(hir, trait_id, method_name),
    };
    Some(Symbol {
        origin,
        detail: format_function(method),
        definition: Some(method.name_range),
        implementations: actual,
    })
}

fn trait_method_declaration_symbol(
    hir: &HirFile,
    trait_id: TraitId,
    method: &HirFunction,
    origin: TextRange,
) -> Symbol {
    Symbol {
        origin,
        detail: format_function(method),
        definition: Some(method.name_range),
        implementations: trait_method_implementations(hir, trait_id, &method.name.0),
    }
}

fn trait_method<'a>(hir: &'a HirFile, trait_id: TraitId, name: &str) -> Option<&'a HirFunction> {
    hir.item_tree.traits[trait_id]
        .methods
        .iter()
        .find(|method| method.name.0 == name)
}

fn trait_method_implementations(hir: &HirFile, trait_id: TraitId, name: &str) -> Vec<TextRange> {
    hir.item_tree
        .impls
        .iter()
        .filter(|(_, implementation)| trait_id_for_impl(hir, implementation) == Some(trait_id))
        .flat_map(|(_, implementation)| implementation.methods.iter().copied())
        .filter_map(|function| {
            let function = &hir.item_tree.functions[function];
            (function.name.0 == name).then_some(function.name_range)
        })
        .collect()
}

fn trait_impl_for_function(hir: &HirFile, function: FunctionId) -> Option<(TraitId, &HirImpl)> {
    hir.item_tree.impls.iter().find_map(|(_, implementation)| {
        if !implementation.methods.contains(&function) {
            return None;
        }
        trait_id_for_impl(hir, implementation).map(|trait_id| (trait_id, implementation))
    })
}

fn trait_id_for_impl(hir: &HirFile, implementation: &HirImpl) -> Option<TraitId> {
    let HirTypeRef::Named(path) = implementation.trait_ty.as_ref()? else {
        return None;
    };
    match hir.type_resolutions.get(&path.range) {
        Some(ResolvedName::Trait(trait_id)) => Some(*trait_id),
        _ => None,
    }
}

fn reference_path_range(hir: &HirFile, origin: RefOrigin) -> Option<TextRange> {
    match origin {
        RefOrigin::Type { range } => Some(range),
        RefOrigin::Expr { body, expr } => match &hir.bodies[body].exprs[expr] {
            Expr::Path { path, .. } | Expr::Struct { path, .. } => Some(path.range),
            _ => None,
        },
    }
}

fn path_segment_ranges(source: &str, range: TextRange, segments: &[Name]) -> Vec<TextRange> {
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    let Some(text) = source.get(start..end) else {
        return Vec::new();
    };
    let tokens = frontend::lexer::lex(text);
    let mut ranges = Vec::with_capacity(segments.len());
    let mut next = 0;
    for segment in segments {
        let Some((index, token)) = tokens.iter().enumerate().skip(next).find(|(_, token)| {
            token.kind == SyntaxKind::Ident && token.text(text) == segment.0.as_str()
        }) else {
            return Vec::new();
        };
        ranges.push(TextRange::new(
            TextSize::from((start + token.span.start) as u32),
            TextSize::from((start + token.span.end) as u32),
        ));
        next = index + 1;
    }
    ranges
}

fn identifier_range_at(source: &str, offset: usize) -> Option<TextRange> {
    frontend::lexer::lex(source)
        .into_iter()
        .find(|token| {
            token.kind == SyntaxKind::Ident
                && token.span.start <= offset
                && offset <= token.span.end
        })
        .map(|token| {
            TextRange::new(
                TextSize::from(token.span.start as u32),
                TextSize::from(token.span.end as u32),
            )
        })
}

fn identifier_named_in_range(source: &str, range: TextRange, name: &str) -> Option<TextRange> {
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    let text = source.get(start..end)?;
    frontend::lexer::lex(text)
        .into_iter()
        .find(|token| token.kind == SyntaxKind::Ident && token.text(text) == name)
        .map(|token| {
            TextRange::new(
                TextSize::from((start + token.span.start) as u32),
                TextSize::from((start + token.span.end) as u32),
            )
        })
}

fn last_identifier_range(source: &str, range: TextRange) -> Option<TextRange> {
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    let text = source.get(start..end)?;
    frontend::lexer::lex(text)
        .into_iter()
        .rev()
        .find(|token| token.kind == SyntaxKind::Ident)
        .map(|token| {
            TextRange::new(
                TextSize::from((start + token.span.start) as u32),
                TextSize::from((start + token.span.end) as u32),
            )
        })
}

fn receiver_struct_id(ty: &Type) -> Option<hir::item_tree::StructId> {
    match ty {
        Type::Struct(id, _) => Some(*id),
        Type::Ref(inner, _) | Type::Ptr { inner, .. } => receiver_struct_id(inner),
        _ => None,
    }
}

fn format_function(function: &HirFunction) -> String {
    let visibility = if function.visibility.is_public() {
        "pub "
    } else {
        ""
    };
    let safety = if function.is_unsafe { "unsafe " } else { "" };
    let generics = if function.generics.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            function
                .generics
                .iter()
                .map(|name| name.0.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let params = function
        .params
        .iter()
        .map(|parameter| {
            if parameter.name.0 == "self" {
                match &parameter.ty {
                    HirTypeRef::Ref(_, true) => "&mut self".into(),
                    HirTypeRef::Ref(_, false) => "&self".into(),
                    _ => "self".into(),
                }
            } else {
                format!("{}: {}", parameter.name.0, parameter.ty.display())
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let ret = function
        .ret_type
        .as_ref()
        .map(HirTypeRef::display)
        .unwrap_or_else(|| "()".into());
    format!(
        "{visibility}{safety}fun {}{generics}({params}) -> {ret}",
        function.name.0
    )
}

fn format_nominal(kind: &str, name: &Name, generics: &[Name]) -> String {
    if generics.is_empty() {
        return format!("{kind} {}", name.0);
    }
    format!(
        "{kind} {}<{}>",
        name.0,
        generics
            .iter()
            .map(|generic| generic.0.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}
