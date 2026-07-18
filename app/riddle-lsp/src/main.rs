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
use hir::item_tree::HirTypeRef;
#[cfg(test)]
use lsp_types::Position;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, FileChangeType, FileEvent, InitializeParams, InitializeResult,
    InitializedParams, InlayHint, InlayHintKind, InlayHintLabel, InlayHintParams, MessageType,
    NumberOrString, OneOf, PositionEncodingKind, Range, SemanticToken, SemanticTokenModifier,
    SemanticTokenType, SemanticTokens, SemanticTokensFullOptions, SemanticTokensLegend,
    SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
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
        let tokens =
            tokio::task::spawn_blocking(move || semantic_tokens_for_source(&analyzed_text))
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
        ],
    }
}

fn semantic_tokens_for_source(source: &str) -> SemanticTokens {
    let tokens = frontend::lexer::lex(source);
    let result = riddlec::pipeline::resolve_with_options(source, CompileOptions { use_std: false });
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
        collect_hir_symbol_tokens(hir, source, &mut raw_tokens);
    }

    encode_semantic_tokens(source, remove_overlapping_tokens(raw_tokens))
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

fn collect_hir_symbol_tokens(hir: &hir::HirFile, source: &str, out: &mut Vec<RawSemanticToken>) {
    let source_len = source.len();

    for (_, function) in hir.item_tree.functions.iter() {
        for param in &function.params {
            if range_is_in_source(param.name_range, source_len) {
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
            if path.as_single_name().is_none() {
                continue;
            }

            let Some(range) = body
                .source_map
                .expr_ranges
                .get(&expr_id)
                .and_then(|range| trim_source_range(source, *range))
            else {
                continue;
            };
            if !range_is_in_source(range, source_len) {
                continue;
            }

            let Some((token_type, token_modifiers_bitset)) =
                semantic_token_for_resolution(body, resolved.as_ref())
            else {
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
) -> Option<(u32, u32)> {
    match resolved {
        Some(ResolvedName::Local(stmt_id)) => match &body.stmts[*stmt_id] {
            Stmt::Let { is_mut: true, .. } => Some((TOKEN_VARIABLE, MOD_MUTABLE)),
            _ => None,
        },
        Some(ResolvedName::Param(_) | ResolvedName::LambdaParam { .. }) => {
            Some((TOKEN_PARAMETER, 0))
        }
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

fn remove_overlapping_tokens(raw_tokens: Vec<RawSemanticToken>) -> Vec<RawSemanticToken> {
    let (mut preferred, mut fallback): (Vec<_>, Vec<_>) = raw_tokens
        .into_iter()
        .partition(|token| matches!(token.token_type, TOKEN_VARIABLE | TOKEN_PARAMETER));
    preferred.sort_by_key(|token| (token.range.start(), token.range.end()));
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
        _ if token_starts_uppercase(tokens[index].text(source)) => Some(TOKEN_TYPE),
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
