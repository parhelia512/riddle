use hir::item_tree::{HirVariantKind, TopLevelItem};
use lsp_types::{
    DocumentSymbol, FoldingRange, FoldingRangeKind, Location, SymbolInformation, SymbolKind,
};
use std::{collections::HashMap, hash::BuildHasher};
use syntax::SyntaxKind;

use crate::text::LineIndex;
use crate::{
    analysis::{AnalysisDepth, analyze_document_cancellable},
    navigation::source_uri,
    server::Document,
    session::AnalysisSessions,
};

#[must_use]
pub fn format_source(source: &str, tab_size: u32, insert_spaces: bool) -> String {
    let tokens = frontend::lexer::lex(source);
    let indent_unit = if insert_spaces {
        " ".repeat(tab_size.max(1) as usize)
    } else {
        "\t".into()
    };
    let significant = tokens
        .iter()
        .filter(|token| token.kind != SyntaxKind::Whitespace)
        .collect::<Vec<_>>();
    let mut formatter = SourceFormatter::new(indent_unit);
    for (index, token) in significant.iter().enumerate() {
        let next = significant.get(index + 1).map(|token| token.kind);
        formatter.push(token.kind, &source[token.span.clone()], next);
    }
    formatter.finish()
}

struct SourceFormatter {
    output: String,
    indent: usize,
    delimiters: Vec<SyntaxKind>,
    previous: Option<SyntaxKind>,
    indent_unit: String,
}

impl SourceFormatter {
    const fn new(indent_unit: String) -> Self {
        Self {
            output: String::new(),
            indent: 0,
            delimiters: Vec::new(),
            previous: None,
            indent_unit,
        }
    }

    fn push(&mut self, kind: SyntaxKind, text: &str, next: Option<SyntaxKind>) {
        match kind {
            SyntaxKind::LineComment => {
                write_indent(&mut self.output, self.indent, &self.indent_unit);
                if !self.output.ends_with([' ', '\n', '\t']) {
                    self.output.push(' ');
                }
                self.output.push_str(text.trim_end());
                newline(&mut self.output);
            }
            SyntaxKind::LBrace => {
                write_indent(&mut self.output, self.indent, &self.indent_unit);
                ensure_space(&mut self.output);
                self.output.push('{');
                self.delimiters.push(kind);
                self.indent += 1;
                newline(&mut self.output);
            }
            SyntaxKind::RBrace => {
                self.close_brace(next);
            }
            SyntaxKind::LParen | SyntaxKind::LBracket => {
                write_indent(&mut self.output, self.indent, &self.indent_unit);
                self.output.push_str(text);
                self.delimiters.push(kind);
            }
            SyntaxKind::RParen | SyntaxKind::RBracket => {
                trim_spaces(&mut self.output);
                self.output.push_str(text);
                self.delimiters.pop();
            }
            SyntaxKind::Semi => {
                trim_spaces(&mut self.output);
                self.output.push(';');
                newline(&mut self.output);
            }
            SyntaxKind::Comma => {
                trim_spaces(&mut self.output);
                self.output.push(',');
                if self.delimiters.last() == Some(&SyntaxKind::LBrace) {
                    newline(&mut self.output);
                } else {
                    self.output.push(' ');
                }
            }
            SyntaxKind::Colon => {
                trim_spaces(&mut self.output);
                self.output.push_str(": ");
            }
            SyntaxKind::Dot | SyntaxKind::ColonColon | SyntaxKind::Question => {
                trim_spaces(&mut self.output);
                self.output.push_str(text);
            }
            kind if is_spaced_operator(kind) => {
                write_indent(&mut self.output, self.indent, &self.indent_unit);
                ensure_space(&mut self.output);
                self.output.push_str(text);
                self.output.push(' ');
            }
            _ => {
                write_indent(&mut self.output, self.indent, &self.indent_unit);
                if needs_space(self.previous, kind, &self.output) {
                    ensure_space(&mut self.output);
                }
                self.output.push_str(text);
            }
        }
        self.previous = Some(kind);
    }

    fn close_brace(&mut self, next: Option<SyntaxKind>) {
        self.indent = self.indent.saturating_sub(1);
        if !self.output.ends_with('\n') {
            newline(&mut self.output);
        }
        write_indent(&mut self.output, self.indent, &self.indent_unit);
        self.output.push('}');
        if self.delimiters.last() == Some(&SyntaxKind::LBrace) {
            self.delimiters.pop();
        }
        if !matches!(
            next,
            Some(
                SyntaxKind::Else
                    | SyntaxKind::Semi
                    | SyntaxKind::Comma
                    | SyntaxKind::RParen
                    | SyntaxKind::RBracket
            )
        ) {
            newline(&mut self.output);
        }
    }

    fn finish(mut self) -> String {
        trim_spaces(&mut self.output);
        while self.output.ends_with("\n\n") {
            self.output.pop();
        }
        if !self.output.is_empty() && !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        self.output
    }
}

const fn is_spaced_operator(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Arrow
            | SyntaxKind::FatArrow
            | SyntaxKind::Eq
            | SyntaxKind::EqEq
            | SyntaxKind::BangEq
            | SyntaxKind::Less
            | SyntaxKind::LessEq
            | SyntaxKind::Greater
            | SyntaxKind::GreaterEq
            | SyntaxKind::Plus
            | SyntaxKind::Minus
            | SyntaxKind::Slash
            | SyntaxKind::Percent
            | SyntaxKind::Pipe
            | SyntaxKind::Caret
            | SyntaxKind::AmpAmp
            | SyntaxKind::PipePipe
            | SyntaxKind::PlusEq
            | SyntaxKind::MinusEq
            | SyntaxKind::StarEq
            | SyntaxKind::SlashEq
            | SyntaxKind::PercentEq
            | SyntaxKind::AmpEq
            | SyntaxKind::PipeEq
            | SyntaxKind::CaretEq
            | SyntaxKind::Shl
            | SyntaxKind::Shr
            | SyntaxKind::ShlEq
            | SyntaxKind::ShrEq
    )
}

fn needs_space(previous: Option<SyntaxKind>, current: SyntaxKind, output: &str) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    if output.ends_with([' ', '\n', '\t']) {
        return false;
    }
    !matches!(
        (previous, current),
        (
            SyntaxKind::LParen
                | SyntaxKind::LBracket
                | SyntaxKind::Dot
                | SyntaxKind::ColonColon
                | SyntaxKind::Hash
                | SyntaxKind::Bang,
            _
        ) | (
            _,
            SyntaxKind::LParen | SyntaxKind::LBracket | SyntaxKind::Bang
        )
    )
}

fn write_indent(output: &mut String, indent: usize, indent_unit: &str) {
    if output.is_empty() || output.ends_with('\n') {
        for _ in 0..indent {
            output.push_str(indent_unit);
        }
    }
}

fn ensure_space(output: &mut String) {
    if !output.is_empty() && !output.ends_with([' ', '\n', '\t']) {
        output.push(' ');
    }
}

fn trim_spaces(output: &mut String) {
    while output.ends_with([' ', '\t']) {
        output.pop();
    }
}

fn newline(output: &mut String) {
    trim_spaces(output);
    if !output.ends_with('\n') {
        output.push('\n');
    }
}

#[must_use]
pub fn folding_ranges(source: &str) -> Vec<FoldingRange> {
    let index = LineIndex::new(source);
    let mut stack = Vec::new();
    let mut ranges = Vec::new();
    for token in frontend::lexer::lex(source) {
        match token.kind {
            SyntaxKind::LBrace => stack.push(token.span.start),
            SyntaxKind::RBrace => {
                let Some(start) = stack.pop() else {
                    continue;
                };
                let start = index.position(source, start);
                let end = index.position(source, token.span.end.saturating_sub(1));
                if let (Some(start), Some(end)) = (start, end)
                    && start.line < end.line
                {
                    ranges.push(FoldingRange {
                        start_line: start.line,
                        start_character: Some(start.character),
                        end_line: end.line,
                        end_character: Some(end.character),
                        kind: Some(FoldingRangeKind::Region),
                        collapsed_text: None,
                    });
                }
            }
            _ => {}
        }
    }
    ranges.sort_by_key(|range| (range.start_line, range.start_character));
    ranges
}

#[cfg(feature = "test")]
#[must_use]
pub fn document_symbols_for_source(source: &str) -> Vec<DocumentSymbol> {
    let result = riddlec::pipeline::resolve_with_options(
        source,
        riddlec::pipeline::CompileOptions { use_std: false },
    );
    let Some(hir) = result.hir.as_ref() else {
        return Vec::new();
    };
    let index = LineIndex::new(source);
    symbols_for_items(hir, &hir.item_tree.top_level, &|range| {
        index.range(source, range)
    })
}

#[cfg(feature = "test")]
#[must_use]
/// Returns workspace symbols from standalone test source.
///
/// # Panics
///
/// Panics if the fixed test document URL cannot be parsed.
pub fn workspace_symbols_for_source(source: &str, query: &str) -> Vec<SymbolInformation> {
    let uri = lsp_types::Url::parse("untitled:riddle-workspace-symbols.rid").unwrap();
    let mut symbols = Vec::new();
    flatten_symbols(
        &uri,
        query,
        None,
        &document_symbols_for_source(source),
        &mut symbols,
    );
    symbols
}

#[allow(deprecated)]
#[cfg(feature = "test")]
fn flatten_symbols(
    uri: &lsp_types::Url,
    query: &str,
    container: Option<&str>,
    nested: &[DocumentSymbol],
    output: &mut Vec<SymbolInformation>,
) {
    for symbol in nested {
        if symbol.name.to_lowercase().contains(&query.to_lowercase()) {
            output.push(SymbolInformation {
                name: symbol.name.clone(),
                kind: symbol.kind,
                tags: symbol.tags.clone(),
                deprecated: None,
                location: Location::new(uri.clone(), symbol.selection_range),
                container_name: container.map(str::to_string),
            });
        }
        if let Some(children) = &symbol.children {
            flatten_symbols(uri, query, Some(&symbol.name), children, output);
        }
    }
}

fn symbols_for_items(
    hir: &hir::HirFile,
    items: &[TopLevelItem],
    range_for: &impl Fn(rowan::TextRange) -> Option<lsp_types::Range>,
) -> Vec<DocumentSymbol> {
    items
        .iter()
        .filter_map(|item| symbol_for_item(hir, *item, range_for))
        .collect()
}

fn symbol_for_item(
    hir: &hir::HirFile,
    item: TopLevelItem,
    range_for: &impl Fn(rowan::TextRange) -> Option<lsp_types::Range>,
) -> Option<DocumentSymbol> {
    match item {
        TopLevelItem::Function(id) => {
            let item = &hir.item_tree.functions[id];
            symbol(
                range_for,
                &item.name.0,
                SymbolKind::FUNCTION,
                item.name_range,
                None,
            )
        }
        TopLevelItem::Struct(id) => {
            let item = &hir.item_tree.structs[id];
            let children = symbols_for_fields(&item.fields, range_for);
            symbol(
                range_for,
                &item.name.0,
                SymbolKind::STRUCT,
                item.name_range,
                Some(children),
            )
        }
        TopLevelItem::Enum(id) => {
            let item = &hir.item_tree.enums[id];
            symbol(
                range_for,
                &item.name.0,
                SymbolKind::ENUM,
                item.name_range,
                Some(symbols_for_enum_children(item, range_for)),
            )
        }
        TopLevelItem::Trait(id) => {
            let item = &hir.item_tree.traits[id];
            symbol(
                range_for,
                &item.name.0,
                SymbolKind::INTERFACE,
                item.name_range,
                Some(symbols_for_methods(&item.methods, range_for)),
            )
        }
        TopLevelItem::Module(id) => {
            let item = &hir.item_tree.modules[id];
            let children = item
                .items
                .as_deref()
                .map(|items| symbols_for_items(hir, items, range_for));
            symbol(
                range_for,
                &item.name.0,
                SymbolKind::MODULE,
                item.name_range,
                children,
            )
        }
        TopLevelItem::Const(id) => {
            let item = &hir.item_tree.consts[id];
            symbol(
                range_for,
                &item.name.0,
                SymbolKind::CONSTANT,
                item.name_range,
                None,
            )
        }
        TopLevelItem::TypeAlias(id) => {
            let item = &hir.item_tree.type_aliases[id];
            symbol(
                range_for,
                &item.name.0,
                SymbolKind::TYPE_PARAMETER,
                item.name_range,
                None,
            )
        }
        TopLevelItem::Impl(_) | TopLevelItem::Use(_) => None,
    }
}

fn symbols_for_fields(
    fields: &[hir::item_tree::HirStructField],
    range_for: &impl Fn(rowan::TextRange) -> Option<lsp_types::Range>,
) -> Vec<DocumentSymbol> {
    fields
        .iter()
        .filter_map(|field| {
            symbol(
                range_for,
                &field.name.0,
                SymbolKind::FIELD,
                field.name_range,
                None,
            )
        })
        .collect()
}

fn symbols_for_enum_children(
    item: &hir::item_tree::HirEnum,
    range_for: &impl Fn(rowan::TextRange) -> Option<lsp_types::Range>,
) -> Vec<DocumentSymbol> {
    item.variants
        .iter()
        .filter_map(|variant| {
            let fields = match &variant.kind {
                HirVariantKind::Struct(fields) => symbols_for_fields(fields, range_for),
                _ => Vec::new(),
            };
            symbol(
                range_for,
                &variant.name.0,
                SymbolKind::ENUM_MEMBER,
                variant.name_range,
                Some(fields),
            )
        })
        .collect()
}

fn symbols_for_methods(
    methods: &[hir::item_tree::HirFunction],
    range_for: &impl Fn(rowan::TextRange) -> Option<lsp_types::Range>,
) -> Vec<DocumentSymbol> {
    methods
        .iter()
        .filter_map(|method| {
            symbol(
                range_for,
                &method.name.0,
                SymbolKind::METHOD,
                method.name_range,
                None,
            )
        })
        .collect()
}

#[allow(deprecated)]
fn symbol(
    range_for: &impl Fn(rowan::TextRange) -> Option<lsp_types::Range>,
    name: &str,
    kind: SymbolKind,
    range: rowan::TextRange,
    children: Option<Vec<DocumentSymbol>>,
) -> Option<DocumentSymbol> {
    let range = range_for(range)?;
    Some(DocumentSymbol {
        name: name.into(),
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children,
    })
}

pub fn document_symbols_for_document_cancellable<S: BuildHasher>(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document, S>,
    options: riddlec::pipeline::CompileOptions,
    sessions: &AnalysisSessions,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Vec<DocumentSymbol>>, String> {
    let document = docs
        .get(uri)
        .ok_or_else(|| "document is not open".to_string())?;
    let Some(analysis) = analyze_document_cancellable(
        uri,
        docs,
        options,
        sessions,
        AnalysisDepth::Resolve,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(hir) = analysis.result.hir.as_ref() else {
        return Ok(Some(Vec::new()));
    };
    let index = LineIndex::new(&document.text);
    Ok(Some(symbols_for_items(
        hir,
        &hir.item_tree.top_level,
        &|range| {
            analysis
                .local_range(range)
                .and_then(|range| index.range(&document.text, range))
        },
    )))
}

pub fn workspace_symbols_for_document_cancellable<S: BuildHasher>(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document, S>,
    query: &str,
    options: riddlec::pipeline::CompileOptions,
    sessions: &AnalysisSessions,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Vec<SymbolInformation>>, String> {
    let Some(analysis) = analyze_document_cancellable(
        uri,
        docs,
        options,
        sessions,
        AnalysisDepth::Resolve,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(hir) = analysis.result.hir.as_ref() else {
        return Ok(Some(Vec::new()));
    };
    let mut symbols = Vec::new();
    collect_workspace_items(
        uri,
        &analysis,
        hir,
        &hir.item_tree.top_level,
        &query.to_lowercase(),
        None,
        &mut symbols,
    );
    Ok(Some(symbols))
}

fn collect_workspace_items(
    current_uri: &lsp_types::Url,
    analysis: &crate::analysis::DocumentAnalysis,
    hir: &hir::HirFile,
    items: &[TopLevelItem],
    query: &str,
    container: Option<&str>,
    output: &mut Vec<SymbolInformation>,
) {
    for item in items {
        collect_workspace_item(current_uri, analysis, hir, *item, query, container, output);
    }
}

fn collect_workspace_item(
    current_uri: &lsp_types::Url,
    analysis: &crate::analysis::DocumentAnalysis,
    hir: &hir::HirFile,
    item: TopLevelItem,
    query: &str,
    container: Option<&str>,
    output: &mut Vec<SymbolInformation>,
) {
    match item {
        TopLevelItem::Function(id) => {
            let item = &hir.item_tree.functions[id];
            push_workspace_symbol(
                current_uri,
                analysis,
                &item.name.0,
                SymbolKind::FUNCTION,
                item.name_range,
                query,
                container,
                output,
            );
        }
        TopLevelItem::Struct(id) => {
            let item = &hir.item_tree.structs[id];
            push_workspace_symbol(
                current_uri,
                analysis,
                &item.name.0,
                SymbolKind::STRUCT,
                item.name_range,
                query,
                container,
                output,
            );
        }
        TopLevelItem::Enum(id) => collect_enum_workspace_items(
            current_uri,
            analysis,
            &hir.item_tree.enums[id],
            query,
            container,
            output,
        ),
        TopLevelItem::Trait(id) => collect_trait_workspace_items(
            current_uri,
            analysis,
            &hir.item_tree.traits[id],
            query,
            container,
            output,
        ),
        TopLevelItem::Module(id) => collect_module_workspace_items(
            current_uri,
            analysis,
            hir,
            &hir.item_tree.modules[id],
            query,
            container,
            output,
        ),
        TopLevelItem::Impl(id) => collect_impl_workspace_items(
            current_uri,
            analysis,
            hir,
            &hir.item_tree.impls[id],
            query,
            output,
        ),
        TopLevelItem::Const(id) => {
            let item = &hir.item_tree.consts[id];
            push_workspace_symbol(
                current_uri,
                analysis,
                &item.name.0,
                SymbolKind::CONSTANT,
                item.name_range,
                query,
                container,
                output,
            );
        }
        TopLevelItem::TypeAlias(id) => {
            let item = &hir.item_tree.type_aliases[id];
            push_workspace_symbol(
                current_uri,
                analysis,
                &item.name.0,
                SymbolKind::TYPE_PARAMETER,
                item.name_range,
                query,
                container,
                output,
            );
        }
        TopLevelItem::Use(_) => {}
    }
}

fn collect_enum_workspace_items(
    current_uri: &lsp_types::Url,
    analysis: &crate::analysis::DocumentAnalysis,
    item: &hir::item_tree::HirEnum,
    query: &str,
    container: Option<&str>,
    output: &mut Vec<SymbolInformation>,
) {
    push_workspace_symbol(
        current_uri,
        analysis,
        &item.name.0,
        SymbolKind::ENUM,
        item.name_range,
        query,
        container,
        output,
    );
    for variant in &item.variants {
        push_workspace_symbol(
            current_uri,
            analysis,
            &variant.name.0,
            SymbolKind::ENUM_MEMBER,
            variant.name_range,
            query,
            Some(&item.name.0),
            output,
        );
    }
}

fn collect_trait_workspace_items(
    current_uri: &lsp_types::Url,
    analysis: &crate::analysis::DocumentAnalysis,
    item: &hir::item_tree::HirTrait,
    query: &str,
    container: Option<&str>,
    output: &mut Vec<SymbolInformation>,
) {
    push_workspace_symbol(
        current_uri,
        analysis,
        &item.name.0,
        SymbolKind::INTERFACE,
        item.name_range,
        query,
        container,
        output,
    );
    for method in &item.methods {
        push_workspace_symbol(
            current_uri,
            analysis,
            &method.name.0,
            SymbolKind::METHOD,
            method.name_range,
            query,
            Some(&item.name.0),
            output,
        );
    }
}

fn collect_module_workspace_items(
    current_uri: &lsp_types::Url,
    analysis: &crate::analysis::DocumentAnalysis,
    hir: &hir::HirFile,
    item: &hir::item_tree::HirModule,
    query: &str,
    container: Option<&str>,
    output: &mut Vec<SymbolInformation>,
) {
    push_workspace_symbol(
        current_uri,
        analysis,
        &item.name.0,
        SymbolKind::MODULE,
        item.name_range,
        query,
        container,
        output,
    );
    if let Some(items) = &item.items {
        collect_workspace_items(
            current_uri,
            analysis,
            hir,
            items,
            query,
            Some(&item.name.0),
            output,
        );
    }
}

fn collect_impl_workspace_items(
    current_uri: &lsp_types::Url,
    analysis: &crate::analysis::DocumentAnalysis,
    hir: &hir::HirFile,
    item: &hir::item_tree::HirImpl,
    query: &str,
    output: &mut Vec<SymbolInformation>,
) {
    let container = format!("impl {}", item.self_ty.display());
    for method in &item.methods {
        let method = &hir.item_tree.functions[*method];
        push_workspace_symbol(
            current_uri,
            analysis,
            &method.name.0,
            SymbolKind::METHOD,
            method.name_range,
            query,
            Some(&container),
            output,
        );
    }
}

#[allow(deprecated, clippy::too_many_arguments)]
fn push_workspace_symbol(
    current_uri: &lsp_types::Url,
    analysis: &crate::analysis::DocumentAnalysis,
    name: &str,
    kind: SymbolKind,
    range: rowan::TextRange,
    query: &str,
    container: Option<&str>,
    output: &mut Vec<SymbolInformation>,
) {
    if !name.to_lowercase().contains(query) {
        return;
    }
    let location = if let Some(source_map) = &analysis.source_map {
        let Some(mapped) = source_map.map_range(range) else {
            return;
        };
        let Some(uri) = source_uri(current_uri, mapped.path) else {
            return;
        };
        let Some(range) = LineIndex::new(mapped.source).range(mapped.source, mapped.range) else {
            return;
        };
        Location::new(uri, range)
    } else {
        let Some(range) = LineIndex::new(&analysis.source).range(&analysis.source, range) else {
            return;
        };
        Location::new(current_uri.clone(), range)
    };
    output.push(SymbolInformation {
        name: name.into(),
        kind,
        tags: None,
        deprecated: None,
        location,
        container_name: container.map(str::to_string),
    });
}
