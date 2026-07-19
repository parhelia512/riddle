use std::{
    collections::HashMap,
    env, process,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
mod diagnostics;

use diagnostics::{DiagnosticSessions, collect_workspace_diagnostics_cancellable};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use diagnostics::{
    collect_diagnostics, collect_document_diagnostics, collect_workspace_diagnostics,
    collect_workspace_diagnostics_with_sessions, position, range, to_lsp,
};
use frontend::syntax_kind::SyntaxKind;
use hir::body::{Expr, ResolvedName, Stmt};
use hir::item_tree::{
    FunctionId, HirFunction, HirTypeRef, HirUseTree, HirUseTreeKind, StructId, TopLevelItem,
    Visibility,
};
#[cfg(test)]
use lsp_types::Position;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    CompletionItem, CompletionItemKind, CompletionItemLabelDetails, CompletionOptions,
    CompletionParams, CompletionResponse, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, FileChangeType, FileEvent,
    InitializeParams, InitializeResult, InitializedParams, InlayHint, InlayHintKind,
    InlayHintLabel, InlayHintParams, MessageType, NumberOrString, OneOf, PositionEncodingKind,
    Range, SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensResult, SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
    TextDocumentContentChangeEvent, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit,
    WorkspaceEdit,
};
use riddlec::pipeline::CompileOptions;
use rowan::TextRange;
use tower_lsp::jsonrpc::Result;
use tower_lsp::{Client, LanguageServer, LspService, Server};

struct Backend {
    client: Client,
    docs: Arc<Mutex<HashMap<lsp_types::Url, Document>>>,
    published: Arc<Mutex<HashMap<lsp_types::Url, diagnostics::PublishedDiagnostics>>>,
    publish_gate: Arc<tokio::sync::Mutex<()>>,
    diagnostic_revision: Arc<AtomicU64>,
    diagnostic_sessions: Arc<Mutex<DiagnosticSessions>>,
    semantic_tokens: Arc<Mutex<HashMap<lsp_types::Url, CachedSemanticTokens>>>,
    compile_options: CompileOptions,
}

const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
struct Document {
    text: String,
    version: Option<i32>,
}

#[derive(Clone)]
struct CachedSemanticTokens {
    text: String,
    tokens: SemanticTokens,
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(PositionEncodingKind::UTF16),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                code_action_provider: Some(true.into()),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: Some(vec![".".into(), ":".into()]),
                    ..CompletionOptions::default()
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(SemanticTokensServerCapabilities::from(
                    SemanticTokensOptions {
                        legend: semantic_tokens_legend(),
                        full: Some(SemanticTokensFullOptions::Bool(true)),
                        ..SemanticTokensOptions::default()
                    },
                )),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "riddle-lsp".into(),
                version: Some(format!(
                    "{} ({})",
                    env!("CARGO_PKG_VERSION"),
                    riddlec::GIT_HASH
                )),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "riddle-lsp initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let doc = Document {
            text: params.text_document.text,
            version: Some(params.text_document.version),
        };
        self.docs.lock().unwrap().insert(uri, doc);
        self.schedule_diagnostics();
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some(text) = full_text(params.content_changes) else {
            return;
        };
        let doc = Document {
            text,
            version: Some(params.text_document.version),
        };
        self.docs.lock().unwrap().insert(uri, doc);
        self.schedule_diagnostics();
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.lock().unwrap().remove(&uri);
        self.semantic_tokens.lock().unwrap().remove(&uri);
        self.schedule_diagnostics();
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let Some(text) = self
            .docs
            .lock()
            .unwrap()
            .get(&uri)
            .map(|doc| doc.text.clone())
        else {
            return Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
                result_id: None,
                data: Vec::new(),
            })));
        };
        if let Some(cached) = self
            .semantic_tokens
            .lock()
            .unwrap()
            .get(&uri)
            .filter(|cached| cached.text == text)
        {
            return Ok(Some(SemanticTokensResult::Tokens(cached.tokens.clone())));
        }

        let analyzed_text = text.clone();
        let compile_options = self.compile_options;
        let default_library_source = is_standard_library_uri(&uri);
        let tokens = tokio::task::spawn_blocking(move || {
            semantic_tokens_for_source_with_options(
                &analyzed_text,
                compile_options,
                default_library_source,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let is_current = self
            .docs
            .lock()
            .unwrap()
            .get(&uri)
            .is_some_and(|document| document.text == text);
        if !is_current {
            return Ok(None);
        }
        self.semantic_tokens.lock().unwrap().insert(
            uri,
            CachedSemanticTokens {
                text,
                tokens: tokens.clone(),
            },
        );

        Ok(Some(SemanticTokensResult::Tokens(tokens)))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let Some(text) = self
            .docs
            .lock()
            .unwrap()
            .get(&uri)
            .map(|document| document.text.clone())
        else {
            return Ok(Some(Vec::new()));
        };
        let analyzed_text = text.clone();
        let hints = tokio::task::spawn_blocking(move || {
            inlay_hints_for_source(&analyzed_text, params.range)
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        if self
            .docs
            .lock()
            .unwrap()
            .get(&uri)
            .is_none_or(|document| document.text != text)
        {
            return Ok(None);
        }

        Ok(Some(hints))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let Some(text) = self
            .docs
            .lock()
            .unwrap()
            .get(&uri)
            .map(|document| document.text.clone())
        else {
            return Ok(Some(CompletionResponse::Array(Vec::new())));
        };
        let analyzed_text = text.clone();
        let compile_options = self.compile_options;
        let items = tokio::task::spawn_blocking(move || {
            completion_items_for_source(&analyzed_text, position, compile_options)
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        if self
            .docs
            .lock()
            .unwrap()
            .get(&uri)
            .is_none_or(|document| document.text != text)
        {
            return Ok(None);
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        Ok(Some(quick_fixes(
            &params.text_document.uri,
            &params.context.diagnostics,
        )))
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        if params.changes.iter().any(watched_change_resets_sessions) {
            *self.diagnostic_sessions.lock().unwrap() = DiagnosticSessions::default();
        }
        self.schedule_diagnostics();
    }
}

const MUTABLE_CLOSURE_BINDING_MESSAGE: &str =
    "cannot call a mutable closure through an immutable binding\nimmutable closure binding";

fn quick_fixes(uri: &lsp_types::Url, diagnostics: &[lsp_types::Diagnostic]) -> CodeActionResponse {
    diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.source.as_deref() == Some("riddle")
                && matches!(
                    &diagnostic.code,
                    Some(NumberOrString::String(code)) if code == "E0031"
                )
                && diagnostic
                    .message
                    .starts_with(MUTABLE_CLOSURE_BINDING_MESSAGE)
        })
        .map(|diagnostic| {
            let start = diagnostic.range.start;
            CodeActionOrCommand::CodeAction(CodeAction {
                title: "Add `mut` to closure binding".into(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(
                        uri.clone(),
                        vec![TextEdit::new(Range::new(start, start), "mut ".into())],
                    )])),
                    ..WorkspaceEdit::default()
                }),
                is_preferred: Some(true),
                ..CodeAction::default()
            })
        })
        .collect()
}

fn inlay_hints_for_source(source: &str, range: Range) -> Vec<InlayHint> {
    // ponytail: document-local type hints; reuse project analysis when cross-module hints matter.
    let resolved =
        riddlec::pipeline::resolve_with_options(source, CompileOptions { use_std: false });
    let Some(hir) = resolved.hir.as_ref() else {
        return Vec::new();
    };
    let type_result = type_checker::check_hir(hir);
    let mut hints = Vec::new();

    for (body_id, body) in hir.bodies.iter() {
        for (_, statement) in body.stmts.iter() {
            let Stmt::Let {
                name_range: Some(name_range),
                ty: HirTypeRef::Unknown,
                init: Some(init),
                ..
            } = statement
            else {
                continue;
            };
            if matches!(body.exprs[*init], Expr::Struct { .. }) {
                continue;
            }
            let Some(init_range) = body.source_map.expr_ranges.get(init).copied() else {
                continue;
            };
            if resolved
                .hir_diagnostics
                .iter()
                .chain(type_result.diagnostics.iter())
                .filter(|diagnostic| diagnostic.severity == type_checker::Severity::Error)
                .flat_map(|diagnostic| &diagnostic.labels)
                .any(|label| ranges_overlap(label.range, init_range))
            {
                continue;
            }
            let Some(ty) = type_result.expr_types.get(&(body_id, *init)) else {
                continue;
            };
            if matches!(
                ty,
                type_checker::Type::Unknown
                    | type_checker::Type::Error
                    | type_checker::Type::InferVar(_)
                    | type_checker::Type::Never
            ) {
                continue;
            }
            hints.push(InlayHint {
                position: diagnostics::position(source, usize::from(name_range.end())),
                label: InlayHintLabel::String(format!(": {}", ty.display(hir))),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: None,
                data: None,
            });
        }
    }

    hints.retain(|hint| {
        range.start.line <= hint.position.line && hint.position.line <= range.end.line
    });
    hints.sort_by_key(|hint| (hint.position.line, hint.position.character));
    hints
}

const COMPLETION_MARKER: &str = "__riddle_completion";
const COMPLETION_KEYWORDS: &[&str] = &[
    "let", "fun", "struct", "if", "else", "while", "break", "continue", "return", "as", "self",
    "mod", "use", "mut", "pub", "super", "crate", "enum", "trait", "impl", "match", "const",
    "type", "extern", "unsafe", "for", "in", "where", "true", "false",
];
const COMPLETION_BUILTIN_TYPES: &[&str] = &[
    "bool", "char", "str", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64",
    "u128", "usize", "f16", "f32", "f64", "f128",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionContext<'a> {
    General,
    Member,
    Associated(&'a str),
}

fn completion_items_for_source(
    source: &str,
    position: lsp_types::Position,
    compile_options: CompileOptions,
) -> Vec<CompletionItem> {
    let Some(offset) = offset_for_position(source, position) else {
        return Vec::new();
    };
    let start = identifier_start(source, offset);
    let end = identifier_end(source, offset);
    let prefix = &source[start..offset];
    let before = &source[..start];
    let context = if before.ends_with('.') {
        CompletionContext::Member
    } else if let Some(path) = before.strip_suffix("::") {
        CompletionContext::Associated(trailing_identifier(path))
    } else {
        CompletionContext::General
    };

    let mut analyzed_source = source.to_string();
    if context != CompletionContext::General {
        analyzed_source.replace_range(start..end, COMPLETION_MARKER);
    }
    let mut resolved = riddlec::pipeline::resolve_with_options(&analyzed_source, compile_options);
    let marker_end = start + COMPLETION_MARKER.len();
    if resolved.hir.is_none()
        && context != CompletionContext::General
        && !analyzed_source[marker_end..].trim_start().starts_with(';')
    {
        analyzed_source.insert(marker_end, ';');
        resolved = riddlec::pipeline::resolve_with_options(&analyzed_source, compile_options);
    }
    let mut items = Vec::new();

    if context == CompletionContext::General {
        items.extend(COMPLETION_KEYWORDS.iter().map(|keyword| {
            completion_item(keyword, CompletionItemKind::KEYWORD, Some("keyword".into()))
        }));
        items.extend(COMPLETION_BUILTIN_TYPES.iter().map(|ty| {
            completion_item(
                ty,
                CompletionItemKind::TYPE_PARAMETER,
                Some("builtin type".into()),
            )
        }));
    }

    if let Some(hir) = resolved.hir.as_ref() {
        match context {
            CompletionContext::General => {
                collect_global_completions(hir, source.len(), &mut items);
                collect_local_completions(hir, offset, &mut items);
            }
            CompletionContext::Member => {
                let types = type_checker::check_hir(hir);
                collect_member_completions(hir, &types, source.len(), &mut items);
            }
            CompletionContext::Associated(qualifier) if !qualifier.is_empty() => {
                collect_associated_completions(hir, qualifier, source.len(), &mut items);
            }
            CompletionContext::Associated(_) => {}
        }
    }

    items.retain(|item| item.label.starts_with(prefix));
    items.sort_by(|left, right| left.label.cmp(&right.label));
    items.dedup_by(|left, right| left.label == right.label);
    items
}

fn collect_global_completions(
    hir: &hir::HirFile,
    source_len: usize,
    out: &mut Vec<CompletionItem>,
) {
    for item in &hir.item_tree.top_level {
        push_top_level_item(hir, *item, source_len, true, out);
    }
    for (_, use_item) in hir.item_tree.uses.iter() {
        if use_item.visibility.is_public() {
            push_use_tree(&use_item.tree, out);
        }
    }
}

fn collect_local_completions(hir: &hir::HirFile, offset: usize, out: &mut Vec<CompletionItem>) {
    // ponytail: document-local scopes; use the project scope graph for cross-module completion.
    for (function_id, body_id) in &hir.function_bodies {
        let hir_body = &hir.bodies[*body_id];
        let Some(root_range) = hir_body.source_map.expr_ranges.get(&hir_body.root_block) else {
            continue;
        };
        if !range_contains_offset(*root_range, offset) {
            continue;
        }
        for param in &hir.item_tree.functions[*function_id].params {
            out.push(completion_item(
                &param.name.0,
                CompletionItemKind::VARIABLE,
                Some(param.ty.display()),
            ));
        }
        for (_, statement) in hir_body.stmts.iter() {
            if let Stmt::Let {
                name,
                name_range: Some(name_range),
                ty,
                ..
            } = statement
                && usize::from(name_range.start()) < offset
            {
                let detail = (ty != &HirTypeRef::Unknown).then(|| ty.display());
                out.push(completion_item(
                    &name.0,
                    CompletionItemKind::VARIABLE,
                    detail,
                ));
            }
        }
        for (_, expr) in hir_body.exprs.iter() {
            let Expr::Lambda {
                params,
                body: lambda_body,
                ..
            } = expr
            else {
                continue;
            };
            let Some(lambda_range) = hir_body.source_map.expr_ranges.get(lambda_body) else {
                continue;
            };
            if range_contains_offset(*lambda_range, offset) {
                for param in params {
                    out.push(completion_item(
                        &param.name.0,
                        CompletionItemKind::VARIABLE,
                        Some(param.ty.display()),
                    ));
                }
            }
        }
    }
}

fn collect_member_completions(
    hir: &hir::HirFile,
    types: &type_checker::TypeCheckResult,
    source_len: usize,
    out: &mut Vec<CompletionItem>,
) {
    let receiver = hir.bodies.iter().find_map(|(body_id, body)| {
        body.exprs.iter().find_map(|(_, expr)| {
            let Expr::FieldAccess { base, field } = expr else {
                return None;
            };
            (field.0 == COMPLETION_MARKER)
                .then(|| types.expr_types.get(&(body_id, *base)))
                .flatten()
        })
    });
    let Some(receiver) = receiver else {
        return;
    };

    if let Some(struct_id) = receiver_struct_id(receiver) {
        let struct_item = &hir.item_tree.structs[struct_id];
        if usize::from(struct_item.name_range.start()) < source_len {
            for field in &struct_item.fields {
                out.push(completion_item(
                    &field.name.0,
                    CompletionItemKind::FIELD,
                    Some(field.ty.display()),
                ));
            }
        }
    }

    for (_, impl_item) in hir.item_tree.impls.iter() {
        if !type_ref_matches_type(hir, &impl_item.self_ty, receiver) {
            continue;
        }
        for function_id in &impl_item.methods {
            let function = &hir.item_tree.functions[*function_id];
            if function
                .params
                .first()
                .is_some_and(|param| param.name.0 == "self")
                && (item_is_visible(function.visibility.clone(), function.name_range, source_len)
                    || impl_item.trait_ty.is_some())
            {
                out.push(function_completion(function, CompletionItemKind::METHOD));
            }
        }
    }
}

fn collect_associated_completions(
    hir: &hir::HirFile,
    qualifier: &str,
    source_len: usize,
    out: &mut Vec<CompletionItem>,
) {
    for (_, enum_item) in hir.item_tree.enums.iter() {
        if enum_item.name.0 == qualifier {
            for variant in &enum_item.variants {
                out.push(completion_item(
                    &variant.name.0,
                    CompletionItemKind::ENUM_MEMBER,
                    Some(format!("{}::{}", enum_item.name.0, variant.name.0)),
                ));
            }
        }
    }
    for (_, module) in hir.item_tree.modules.iter() {
        if module.name.0 != qualifier {
            continue;
        }
        for item in module.items.iter().flatten() {
            push_top_level_item(hir, *item, source_len, false, out);
            if let TopLevelItem::Use(use_id) = item {
                let use_item = &hir.item_tree.uses[*use_id];
                if use_item.visibility.is_public() {
                    push_use_tree(&use_item.tree, out);
                }
            }
        }
    }
    for (_, impl_item) in hir.item_tree.impls.iter() {
        if type_ref_name(&impl_item.self_ty) != Some(qualifier) {
            continue;
        }
        for function_id in &impl_item.methods {
            let function = &hir.item_tree.functions[*function_id];
            if function
                .params
                .first()
                .is_none_or(|param| param.name.0 != "self")
                && item_is_visible(function.visibility.clone(), function.name_range, source_len)
            {
                out.push(function_completion(function, CompletionItemKind::FUNCTION));
            }
        }
        for const_id in &impl_item.consts {
            let item = &hir.item_tree.consts[*const_id];
            if item_is_visible(item.visibility.clone(), item.name_range, source_len) {
                out.push(completion_item(
                    &item.name.0,
                    CompletionItemKind::CONSTANT,
                    Some(item.ty.display()),
                ));
            }
        }
        for type_alias_id in &impl_item.type_aliases {
            let item = &hir.item_tree.type_aliases[*type_alias_id];
            if item_is_visible(item.visibility.clone(), item.name_range, source_len) {
                out.push(completion_item(
                    &item.name.0,
                    CompletionItemKind::TYPE_PARAMETER,
                    item.ty.as_ref().map(HirTypeRef::display),
                ));
            }
        }
    }
}

fn push_top_level_item(
    hir: &hir::HirFile,
    item: TopLevelItem,
    source_len: usize,
    allow_private_user_item: bool,
    out: &mut Vec<CompletionItem>,
) {
    match item {
        TopLevelItem::Function(id) => {
            let item = &hir.item_tree.functions[id];
            if visible_for_completion(
                &item.visibility,
                item.name_range,
                source_len,
                allow_private_user_item,
            ) {
                out.push(function_completion(item, CompletionItemKind::FUNCTION));
            }
        }
        TopLevelItem::Struct(id) => {
            let item = &hir.item_tree.structs[id];
            if visible_for_completion(
                &item.visibility,
                item.name_range,
                source_len,
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
                &item.visibility,
                item.name_range,
                source_len,
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
                &item.visibility,
                item.name_range,
                source_len,
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
                &item.visibility,
                item.name_range,
                source_len,
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
        &function.name.0,
        kind,
        Some(format!("fun {}({params}) -> {ret}", function.name.0)),
    );
    item.label_details = Some(CompletionItemLabelDetails {
        detail: Some(format!("({params})")),
        description: Some(ret),
    });
    item.insert_text = Some(function.name.0.clone());
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
    visibility: &Visibility,
    range: TextRange,
    source_len: usize,
    allow_private_user_item: bool,
) -> bool {
    visibility.is_public() || (allow_private_user_item && usize::from(range.start()) < source_len)
}

fn item_is_visible(visibility: Visibility, range: TextRange, source_len: usize) -> bool {
    visibility.is_public() || usize::from(range.start()) < source_len
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
    expected: &HirTypeRef,
    actual: &type_checker::Type,
) -> bool {
    match (expected, actual) {
        (HirTypeRef::Ref(expected, _), type_checker::Type::Ref(actual, _))
        | (HirTypeRef::Ref(expected, _), type_checker::Type::Ptr { inner: actual, .. }) => {
            type_ref_matches_type(hir, expected, actual)
        }
        (
            HirTypeRef::Ptr {
                inner: expected, ..
            },
            type_checker::Type::Ptr { inner: actual, .. },
        ) => type_ref_matches_type(hir, expected, actual),
        (HirTypeRef::Array(expected, _), type_checker::Type::Array(actual, _)) => {
            type_ref_matches_type(hir, expected, actual)
        }
        (HirTypeRef::Named(_), type_checker::Type::Ref(actual, _))
        | (HirTypeRef::Named(_), type_checker::Type::Ptr { inner: actual, .. }) => {
            type_ref_matches_type(hir, expected, actual)
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

fn offset_for_position(source: &str, position: lsp_types::Position) -> Option<usize> {
    let mut line_start = 0;
    for _ in 0..position.line {
        line_start += source[line_start..].find('\n')? + 1;
    }
    let line_end = source[line_start..]
        .find('\n')
        .map(|offset| line_start + offset)
        .unwrap_or(source.len());
    let line = source[line_start..line_end]
        .strip_suffix('\r')
        .unwrap_or(&source[line_start..line_end]);
    let mut utf16_column = 0;
    for (byte, ch) in line.char_indices() {
        if utf16_column == position.character {
            return Some(line_start + byte);
        }
        utf16_column += ch.len_utf16() as u32;
        if utf16_column > position.character {
            return None;
        }
    }
    (utf16_column == position.character).then_some(line_start + line.len())
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

fn trailing_identifier(source: &str) -> &str {
    &source[identifier_start(source, source.len())..]
}

fn is_identifier_continue(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

fn range_contains_offset(range: TextRange, offset: usize) -> bool {
    usize::from(range.start()) <= offset && offset <= usize::from(range.end())
}

fn watched_change_resets_sessions(change: &FileEvent) -> bool {
    change.typ != FileChangeType::CHANGED
        || change
            .uri
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            == Some("Clue.toml")
}

impl Backend {
    fn schedule_diagnostics(&self) {
        let revision = self.diagnostic_revision.fetch_add(1, Ordering::SeqCst) + 1;
        let client = self.client.clone();
        let docs = Arc::clone(&self.docs);
        let published_state = Arc::clone(&self.published);
        let publish_gate = Arc::clone(&self.publish_gate);
        let diagnostic_revision = Arc::clone(&self.diagnostic_revision);
        let diagnostic_sessions = Arc::clone(&self.diagnostic_sessions);
        let compile_options = self.compile_options;

        tokio::spawn(async move {
            tokio::time::sleep(DIAGNOSTICS_DEBOUNCE).await;
            if diagnostic_revision.load(Ordering::SeqCst) != revision {
                return;
            }

            let docs = docs.lock().unwrap().clone();
            let analysis_revision = Arc::clone(&diagnostic_revision);
            let published = tokio::task::spawn_blocking(move || {
                let mut sessions = diagnostic_sessions
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if analysis_revision.load(Ordering::SeqCst) != revision {
                    return Ok(None);
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    collect_workspace_diagnostics_cancellable(
                        &docs,
                        compile_options,
                        &mut sessions,
                        || analysis_revision.load(Ordering::SeqCst) != revision,
                    )
                }));
                match result {
                    Ok(published) => Ok(published),
                    Err(_) => {
                        *sessions = DiagnosticSessions::default();
                        Err(())
                    }
                }
            })
            .await;
            let published = match published {
                Ok(Ok(Some(published))) => published,
                Ok(Ok(None)) => return,
                Ok(Err(())) | Err(_) => {
                    client
                        .log_message(MessageType::ERROR, "riddle-lsp analysis failed")
                        .await;
                    return;
                }
            };
            if diagnostic_revision.load(Ordering::SeqCst) != revision {
                return;
            }

            let _publish_guard = publish_gate.lock().await;
            if diagnostic_revision.load(Ordering::SeqCst) != revision {
                return;
            }
            publish_diagnostics(
                &client,
                &published_state,
                &diagnostic_revision,
                revision,
                published,
            )
            .await;
        });
    }
}

async fn publish_diagnostics(
    client: &Client,
    published_state: &Mutex<HashMap<lsp_types::Url, diagnostics::PublishedDiagnostics>>,
    diagnostic_revision: &AtomicU64,
    revision: u64,
    published: Vec<diagnostics::PublishedDiagnostics>,
) {
    let current = published
        .into_iter()
        .map(|published| (published.uri.clone(), published))
        .collect::<HashMap<_, _>>();
    let (previous, uris) = {
        let previous = published_state.lock().unwrap();
        let mut uris = previous
            .keys()
            .chain(current.keys())
            .cloned()
            .collect::<Vec<_>>();
        uris.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        uris.dedup();
        (previous.clone(), uris)
    };

    for uri in uris {
        if diagnostic_revision.load(Ordering::SeqCst) != revision {
            return;
        }
        if previous.get(&uri) == current.get(&uri) {
            continue;
        }
        let (diagnostics, version) = current
            .get(&uri)
            .map(|published| (published.diagnostics.clone(), published.version))
            .unwrap_or_default();
        client
            .publish_diagnostics(uri.clone(), diagnostics, version)
            .await;
        let mut actual = published_state.lock().unwrap();
        if let Some(published) = current.get(&uri) {
            actual.insert(uri, published.clone());
        } else {
            actual.remove(&uri);
        }
    }
}

fn full_text(changes: Vec<TextDocumentContentChangeEvent>) -> Option<String> {
    changes
        .into_iter()
        .rev()
        .find(|change| change.range.is_none())
        .map(|change| change.text)
}

const TOKEN_KEYWORD: u32 = 0;
const TOKEN_COMMENT: u32 = 1;
const TOKEN_STRING: u32 = 2;
const TOKEN_NUMBER: u32 = 3;
const TOKEN_OPERATOR: u32 = 4;
const TOKEN_FUNCTION: u32 = 5;
const TOKEN_METHOD: u32 = 6;
const TOKEN_VARIABLE: u32 = 7;
const TOKEN_TYPE: u32 = 8;
const TOKEN_STRUCT: u32 = 9;
const TOKEN_ENUM: u32 = 10;
const TOKEN_INTERFACE: u32 = 11;
const TOKEN_PROPERTY: u32 = 12;
const TOKEN_NAMESPACE: u32 = 13;
const TOKEN_PARAMETER: u32 = 14;
const MOD_DECLARATION: u32 = 1 << 0;
const MOD_MUTABLE: u32 = 1 << 1;
const MOD_STATIC: u32 = 1 << 2;
const MOD_DEFAULT_LIBRARY: u32 = 1 << 3;

fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,
            SemanticTokenType::COMMENT,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::OPERATOR,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::METHOD,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::TYPE,
            SemanticTokenType::STRUCT,
            SemanticTokenType::ENUM,
            SemanticTokenType::INTERFACE,
            SemanticTokenType::PROPERTY,
            SemanticTokenType::NAMESPACE,
            SemanticTokenType::PARAMETER,
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::new("mutable"),
            SemanticTokenModifier::STATIC,
            SemanticTokenModifier::DEFAULT_LIBRARY,
        ],
    }
}

#[cfg(test)]
fn semantic_tokens_for_source(source: &str) -> SemanticTokens {
    semantic_tokens_for_source_with_options(source, CompileOptions { use_std: false }, false)
}

fn semantic_tokens_for_source_with_options(
    source: &str,
    compile_options: CompileOptions,
    default_library_source: bool,
) -> SemanticTokens {
    let tokens = frontend::lexer::lex(source);
    let result = riddlec::pipeline::resolve_with_options(source, compile_options);
    let mut raw_tokens = Vec::new();

    for (index, token) in tokens.iter().enumerate() {
        let Some((token_type, token_modifiers_bitset)) = semantic_token(&tokens, index, source)
        else {
            continue;
        };
        let text = token.text(source);
        if text.contains('\n') {
            continue;
        }
        raw_tokens.push(RawSemanticToken {
            range: TextRange::new(
                (token.span.start as u32).into(),
                (token.span.end as u32).into(),
            ),
            token_type,
            token_modifiers_bitset,
        });
    }

    if let Some(hir) = &result.hir {
        collect_hir_symbol_tokens(
            hir,
            source,
            &tokens,
            default_library_source,
            &mut raw_tokens,
        );
    }

    encode_semantic_tokens(source, remove_overlapping_tokens(raw_tokens))
}

fn is_standard_library_uri(uri: &lsp_types::Url) -> bool {
    uri.path().contains("/std/std/") || uri.path().ends_with("/std/lib.rid")
}

const USAGE: &str = "usage: riddle-lsp [--no-std]";

struct Opts {
    compile_options: CompileOptions,
}

fn parse_args(args: &[String]) -> std::result::Result<Opts, &'static str> {
    let mut use_std = true;

    for arg in &args[1..] {
        match arg.as_str() {
            "--no-std" => use_std = false,
            "--help" | "-h" => {
                println!("{USAGE}");
                process::exit(0);
            }
            "--version" | "-V" => {
                println!(
                    "riddle-lsp {} ({})",
                    env!("CARGO_PKG_VERSION"),
                    riddlec::GIT_HASH
                );
                process::exit(0);
            }
            _ => return Err("unknown flag"),
        }
    }

    Ok(Opts {
        compile_options: CompileOptions { use_std },
    })
}

#[derive(Debug, Clone, Copy)]
struct RawSemanticToken {
    range: TextRange,
    token_type: u32,
    token_modifiers_bitset: u32,
}

fn collect_hir_symbol_tokens(
    hir: &hir::HirFile,
    source: &str,
    tokens: &[frontend::lexer::Token],
    default_library_source: bool,
    out: &mut Vec<RawSemanticToken>,
) {
    let source_len = source.len();
    let mut symbol_types = HashMap::new();
    let mut method_modifiers = HashMap::new();
    let mut function_modifiers = HashMap::new();

    for (_, item) in hir.item_tree.structs.iter() {
        let in_source = range_is_in_source(item.name_range, source_len);
        let modifiers = if default_library_source || !in_source {
            MOD_DEFAULT_LIBRARY
        } else {
            0
        };
        if in_source {
            symbol_types.insert(item.name.0.as_str(), (TOKEN_STRUCT, modifiers));
        } else {
            symbol_types
                .entry(item.name.0.as_str())
                .or_insert((TOKEN_STRUCT, modifiers));
        }
    }
    for (_, item) in hir.item_tree.enums.iter() {
        let in_source = range_is_in_source(item.name_range, source_len);
        let modifiers = if default_library_source || !in_source {
            MOD_DEFAULT_LIBRARY
        } else {
            0
        };
        if in_source {
            symbol_types.insert(item.name.0.as_str(), (TOKEN_ENUM, modifiers));
        } else {
            symbol_types
                .entry(item.name.0.as_str())
                .or_insert((TOKEN_ENUM, modifiers));
        }
        for variant in &item.variants {
            let in_source = range_is_in_source(variant.name_range, source_len);
            let modifiers = if default_library_source || !in_source {
                MOD_DEFAULT_LIBRARY
            } else {
                0
            };
            if in_source {
                symbol_types.insert(variant.name.0.as_str(), (TOKEN_ENUM, modifiers));
            } else {
                symbol_types
                    .entry(variant.name.0.as_str())
                    .or_insert((TOKEN_ENUM, modifiers));
            }
        }
    }
    for (_, item) in hir.item_tree.traits.iter() {
        symbol_types.entry(item.name.0.as_str()).or_insert((
            TOKEN_INTERFACE,
            if default_library_source {
                MOD_DEFAULT_LIBRARY
            } else {
                0
            },
        ));
        for method in &item.methods {
            if range_is_in_source(method.name_range, source_len) {
                let mut modifiers = MOD_DECLARATION;
                if method
                    .params
                    .first()
                    .is_none_or(|param| param.name.0 != "self")
                {
                    modifiers |= MOD_STATIC;
                }
                if default_library_source {
                    modifiers |= MOD_DEFAULT_LIBRARY;
                }
                out.push(RawSemanticToken {
                    range: method.name_range,
                    token_type: TOKEN_METHOD,
                    token_modifiers_bitset: modifiers,
                });
            }
        }
    }
    for (_, item) in hir.item_tree.impls.iter() {
        for method_id in &item.methods {
            let method = &hir.item_tree.functions[*method_id];
            let in_source = range_is_in_source(method.name_range, source_len);
            let mut modifiers = 0;
            if method
                .params
                .first()
                .is_none_or(|param| param.name.0 != "self")
            {
                modifiers |= MOD_STATIC;
            }
            if default_library_source || !in_source {
                modifiers |= MOD_DEFAULT_LIBRARY;
            }
            method_modifiers.insert(*method_id, modifiers);
            if in_source {
                out.push(RawSemanticToken {
                    range: method.name_range,
                    token_type: TOKEN_METHOD,
                    token_modifiers_bitset: MOD_DECLARATION | modifiers,
                });
            }
        }
    }
    for token in tokens {
        let Some(&(token_type, token_modifiers_bitset)) = symbol_types.get(token.text(source))
        else {
            continue;
        };
        out.push(RawSemanticToken {
            range: TextRange::new(
                (token.span.start as u32).into(),
                (token.span.end as u32).into(),
            ),
            token_type,
            token_modifiers_bitset,
        });
    }

    for (function_id, function) in hir.item_tree.functions.iter() {
        let in_source = range_is_in_source(function.name_range, source_len);
        let modifiers = if default_library_source || !in_source {
            MOD_DEFAULT_LIBRARY
        } else {
            0
        };
        function_modifiers.insert(function_id, modifiers);
        if in_source && !method_modifiers.contains_key(&function_id) {
            out.push(RawSemanticToken {
                range: function.name_range,
                token_type: TOKEN_FUNCTION,
                token_modifiers_bitset: MOD_DECLARATION | modifiers,
            });
        }
        for param in &function.params {
            if param.name.0 != "self" && range_is_in_source(param.name_range, source_len) {
                out.push(RawSemanticToken {
                    range: param.name_range,
                    token_type: TOKEN_PARAMETER,
                    token_modifiers_bitset: MOD_DECLARATION,
                });
            }
        }
    }

    for (_, body) in hir.bodies.iter() {
        for (_, stmt) in body.stmts.iter() {
            if let Stmt::Let {
                name_range, is_mut, ..
            } = stmt
            {
                if !is_mut {
                    continue;
                }
                let Some(range) = *name_range else {
                    continue;
                };
                if !range_is_in_source(range, source_len) {
                    continue;
                }
                out.push(RawSemanticToken {
                    range,
                    token_type: TOKEN_VARIABLE,
                    token_modifiers_bitset: MOD_DECLARATION | MOD_MUTABLE,
                });
            }
        }

        for (expr_id, expr) in body.exprs.iter() {
            if let Expr::Lambda { params, .. } = expr {
                for param in params {
                    let Some(range) = param.name_range else {
                        continue;
                    };
                    if range_is_in_source(range, source_len) {
                        out.push(RawSemanticToken {
                            range,
                            token_type: TOKEN_PARAMETER,
                            token_modifiers_bitset: MOD_DECLARATION,
                        });
                    }
                }
            }

            let Expr::Path { path, resolved } = expr else {
                continue;
            };
            let Some(name) = path.segments.last() else {
                continue;
            };
            if path.as_single_name().is_some() && name.0 == "self" {
                continue;
            }

            let Some(range) = body
                .source_map
                .expr_ranges
                .get(&expr_id)
                .and_then(|range| last_identifier_range(source, *range))
            else {
                continue;
            };
            if !range_is_in_source(range, source_len) {
                continue;
            }

            let Some((token_type, token_modifiers_bitset)) = semantic_token_for_resolution(
                body,
                resolved.as_ref(),
                &method_modifiers,
                &function_modifiers,
            ) else {
                continue;
            };
            out.push(RawSemanticToken {
                range,
                token_type,
                token_modifiers_bitset,
            });
        }
    }
}

fn semantic_token_for_resolution(
    body: &hir::body::Body,
    resolved: Option<&ResolvedName>,
    method_modifiers: &HashMap<FunctionId, u32>,
    function_modifiers: &HashMap<FunctionId, u32>,
) -> Option<(u32, u32)> {
    match resolved {
        Some(ResolvedName::Local(stmt_id)) => match &body.stmts[*stmt_id] {
            Stmt::Let { is_mut: true, .. } => Some((TOKEN_VARIABLE, MOD_MUTABLE)),
            _ => None,
        },
        Some(ResolvedName::Param(_) | ResolvedName::LambdaParam { .. }) => {
            Some((TOKEN_PARAMETER, 0))
        }
        Some(ResolvedName::Function(function_id)) => {
            if let Some(modifiers) = method_modifiers.get(function_id) {
                Some((TOKEN_METHOD, *modifiers))
            } else {
                Some((
                    TOKEN_FUNCTION,
                    function_modifiers.get(function_id).copied().unwrap_or(0),
                ))
            }
        }
        Some(ResolvedName::Struct(_)) => Some((TOKEN_STRUCT, 0)),
        Some(ResolvedName::Enum(_) | ResolvedName::EnumVariant(_, _)) => Some((TOKEN_ENUM, 0)),
        Some(ResolvedName::Trait(_)) => Some((TOKEN_INTERFACE, 0)),
        Some(ResolvedName::TypeAlias(_)) => Some((TOKEN_TYPE, 0)),
        Some(ResolvedName::Module(_)) => Some((TOKEN_NAMESPACE, 0)),
        Some(ResolvedName::Const(_)) => Some((TOKEN_VARIABLE, 0)),
        _ => None,
    }
}

fn range_is_in_source(range: TextRange, source_len: usize) -> bool {
    usize::from(range.end()) <= source_len
}

fn trim_source_range(source: &str, range: TextRange) -> Option<TextRange> {
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    let text = source.get(start..end)?;
    let start = start + text.len() - text.trim_start().len();
    let end = end - (text.len() - text.trim_end().len());

    (start < end).then(|| TextRange::new((start as u32).into(), (end as u32).into()))
}

fn last_identifier_range(source: &str, range: TextRange) -> Option<TextRange> {
    let range = trim_source_range(source, range)?;
    let end = usize::from(range.end());
    let text = source.get(usize::from(range.start())..end)?;
    let length = text
        .chars()
        .rev()
        .take_while(|ch| is_identifier_continue(*ch))
        .map(char::len_utf8)
        .sum::<usize>();
    (length > 0).then(|| TextRange::new(((end - length) as u32).into(), (end as u32).into()))
}

fn remove_overlapping_tokens(raw_tokens: Vec<RawSemanticToken>) -> Vec<RawSemanticToken> {
    let (mut preferred, mut fallback): (Vec<_>, Vec<_>) =
        raw_tokens.into_iter().partition(|token| {
            matches!(
                token.token_type,
                TOKEN_FUNCTION
                    | TOKEN_METHOD
                    | TOKEN_VARIABLE
                    | TOKEN_STRUCT
                    | TOKEN_ENUM
                    | TOKEN_INTERFACE
                    | TOKEN_PARAMETER
            )
        });
    preferred.sort_by_key(|token| {
        (
            token.range.start(),
            token.range.end(),
            std::cmp::Reverse(token.token_modifiers_bitset.count_ones()),
        )
    });
    fallback.sort_by_key(|token| (token.range.start(), token.range.end()));

    let mut kept_preferred: Vec<RawSemanticToken> = Vec::new();
    for token in preferred {
        if kept_preferred
            .last()
            .is_some_and(|kept| ranges_overlap(kept.range, token.range))
        {
            continue;
        }
        kept_preferred.push(token);
    }

    let mut preferred_index = 0;
    let mut kept_fallback: Vec<RawSemanticToken> = Vec::new();
    for token in fallback {
        while kept_preferred
            .get(preferred_index)
            .is_some_and(|preferred| preferred.range.end() <= token.range.start())
        {
            preferred_index += 1;
        }
        if kept_preferred
            .get(preferred_index)
            .is_some_and(|preferred| ranges_overlap(preferred.range, token.range))
            || kept_fallback
                .last()
                .is_some_and(|kept| ranges_overlap(kept.range, token.range))
        {
            continue;
        }
        kept_fallback.push(token);
    }

    kept_preferred.extend(kept_fallback);
    kept_preferred.sort_by_key(|token| (token.range.start(), token.range.end()));
    kept_preferred
}

fn ranges_overlap(a: TextRange, b: TextRange) -> bool {
    a.start() < b.end() && b.start() < a.end()
}

fn encode_semantic_tokens(source: &str, raw_tokens: Vec<RawSemanticToken>) -> SemanticTokens {
    let mut data = Vec::new();
    let mut cursor = 0;
    let mut line = 0;
    let mut character = 0;
    let mut prev_line = 0;
    let mut prev_start = 0;

    for token in raw_tokens {
        let start_offset = usize::from(token.range.start());
        let end_offset = usize::from(token.range.end());
        let Some(text) = source.get(start_offset..end_offset) else {
            continue;
        };
        if text.is_empty() || text.contains('\n') {
            continue;
        }

        let Some(skipped) = source.get(cursor..start_offset) else {
            continue;
        };
        for ch in skipped.chars() {
            if ch == '\n' {
                line += 1;
                character = 0;
            } else {
                character += ch.len_utf16() as u32;
            }
        }
        cursor = start_offset;

        let length = text.chars().map(char::len_utf16).sum::<usize>() as u32;
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 {
            character - prev_start
        } else {
            character
        };

        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: token.token_type,
            token_modifiers_bitset: token.token_modifiers_bitset,
        });
        prev_line = line;
        prev_start = character;
    }

    SemanticTokens {
        result_id: None,
        data,
    }
}

fn semantic_token(
    tokens: &[frontend::lexer::Token],
    index: usize,
    source: &str,
) -> Option<(u32, u32)> {
    let token = &tokens[index];
    let token_type = match token.kind {
        SyntaxKind::Whitespace | SyntaxKind::ErrorNode | SyntaxKind::Eof => None,
        SyntaxKind::LineComment => Some(TOKEN_COMMENT),
        SyntaxKind::String | SyntaxKind::Char => Some(TOKEN_STRING),
        SyntaxKind::Number | SyntaxKind::Float => Some(TOKEN_NUMBER),
        SyntaxKind::Ident => ident_token_type(tokens, index, source),
        kind if is_keyword(kind) => Some(TOKEN_KEYWORD),
        kind if is_operator(kind) => Some(TOKEN_OPERATOR),
        _ => None,
    }?;

    Some((token_type, 0))
}

fn ident_token_type(tokens: &[frontend::lexer::Token], index: usize, source: &str) -> Option<u32> {
    let text = tokens[index].text(source);
    if COMPLETION_BUILTIN_TYPES.contains(&text) {
        return Some(TOKEN_KEYWORD);
    }
    let previous = previous_significant(tokens, index).map(|token| token.kind);
    let next = next_significant(tokens, index).map(|token| token.kind);
    match previous {
        Some(SyntaxKind::Fun) => Some(TOKEN_FUNCTION),
        Some(SyntaxKind::Struct) => Some(TOKEN_STRUCT),
        Some(SyntaxKind::Enum) => Some(TOKEN_ENUM),
        Some(SyntaxKind::Trait) => Some(TOKEN_INTERFACE),
        Some(SyntaxKind::Mod) | Some(SyntaxKind::Use) => Some(TOKEN_NAMESPACE),
        Some(SyntaxKind::TypeKw) | Some(SyntaxKind::Impl) => Some(TOKEN_TYPE),
        Some(SyntaxKind::Dot) => {
            if next == Some(SyntaxKind::LParen) {
                Some(TOKEN_METHOD)
            } else {
                Some(TOKEN_PROPERTY)
            }
        }
        _ if next == Some(SyntaxKind::LParen) => Some(TOKEN_FUNCTION),
        _ if token_starts_uppercase(text) => Some(TOKEN_TYPE),
        _ => None,
    }
}

fn previous_significant(
    tokens: &[frontend::lexer::Token],
    index: usize,
) -> Option<&frontend::lexer::Token> {
    tokens[..index]
        .iter()
        .rev()
        .find(|token| !token.kind.is_trivia())
}

fn next_significant(
    tokens: &[frontend::lexer::Token],
    index: usize,
) -> Option<&frontend::lexer::Token> {
    tokens[index + 1..]
        .iter()
        .find(|token| !token.kind.is_trivia())
}

fn token_starts_uppercase(text: &str) -> bool {
    text.chars().next().map(char::is_uppercase).unwrap_or(false)
}

fn is_keyword(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Let
            | SyntaxKind::Fun
            | SyntaxKind::Struct
            | SyntaxKind::If
            | SyntaxKind::Else
            | SyntaxKind::While
            | SyntaxKind::Break
            | SyntaxKind::Continue
            | SyntaxKind::Return
            | SyntaxKind::As
            | SyntaxKind::SelfKw
            | SyntaxKind::Mod
            | SyntaxKind::Use
            | SyntaxKind::Mut
            | SyntaxKind::Pub
            | SyntaxKind::SuperKw
            | SyntaxKind::CrateKw
            | SyntaxKind::Enum
            | SyntaxKind::Trait
            | SyntaxKind::Impl
            | SyntaxKind::Match
            | SyntaxKind::Const
            | SyntaxKind::TypeKw
            | SyntaxKind::Extern
            | SyntaxKind::Unsafe
            | SyntaxKind::For
            | SyntaxKind::In
            | SyntaxKind::Where
            | SyntaxKind::True
            | SyntaxKind::False
    )
}

fn is_operator(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Arrow
            | SyntaxKind::EqEq
            | SyntaxKind::BangEq
            | SyntaxKind::LessEq
            | SyntaxKind::GreaterEq
            | SyntaxKind::AmpAmp
            | SyntaxKind::PipePipe
            | SyntaxKind::FatArrow
            | SyntaxKind::PlusEq
            | SyntaxKind::MinusEq
            | SyntaxKind::StarEq
            | SyntaxKind::SlashEq
            | SyntaxKind::PercentEq
            | SyntaxKind::AmpEq
            | SyntaxKind::PipeEq
            | SyntaxKind::CaretEq
            | SyntaxKind::ShlEq
            | SyntaxKind::ShrEq
            | SyntaxKind::Shl
            | SyntaxKind::Shr
            | SyntaxKind::Plus
            | SyntaxKind::Minus
            | SyntaxKind::Star
            | SyntaxKind::Slash
            | SyntaxKind::Percent
            | SyntaxKind::Amp
            | SyntaxKind::Pipe
            | SyntaxKind::Caret
            | SyntaxKind::Less
            | SyntaxKind::Greater
            | SyntaxKind::Bang
            | SyntaxKind::Eq
    )
}

#[tokio::main]
async fn main() {
    let args = env::args().collect::<Vec<_>>();
    let opts = match parse_args(&args) {
        Ok(opts) => opts,
        Err(msg) => {
            eprintln!("riddle-lsp: {msg}");
            eprintln!("{USAGE}");
            process::exit(1);
        }
    };
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend {
        client,
        docs: Arc::new(Mutex::new(HashMap::new())),
        published: Arc::new(Mutex::new(HashMap::new())),
        publish_gate: Arc::new(tokio::sync::Mutex::new(())),
        diagnostic_revision: Arc::new(AtomicU64::new(0)),
        diagnostic_sessions: Arc::new(Mutex::new(DiagnosticSessions::default())),
        semantic_tokens: Arc::new(Mutex::new(HashMap::new())),
        compile_options: opts.compile_options,
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
#[path = "../../../tests/riddle_lsp/unit.rs"]
mod tests;
