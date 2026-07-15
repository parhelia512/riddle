use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    process,
    sync::Mutex,
};

use frontend::syntax_kind::SyntaxKind;
use hir::body::{Expr, ResolvedName, Stmt};
use lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, InitializeParams, InitializeResult,
    InitializedParams, Location, MessageType, NumberOrString, Position, PositionEncodingKind,
    Range, SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensResult, SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
    TextDocumentContentChangeEvent, TextDocumentSyncCapability, TextDocumentSyncKind,
};
use riddlec::pipeline::{CompileOptions, CompileResult, DiagnosticExt, IntoDiagnosticExt};
use rowan::TextRange;
use tower_lsp::jsonrpc::Result;
use tower_lsp::{Client, LanguageServer, LspService, Server};

#[derive(Debug)]
struct Backend {
    client: Client,
    docs: Mutex<HashMap<lsp_types::Url, Document>>,
    compile_options: CompileOptions,
}

#[derive(Debug, Clone)]
struct Document {
    text: String,
    version: Option<i32>,
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
        self.publish_all().await;
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
        self.publish_all().await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.lock().unwrap().remove(&uri);
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
        self.publish_all().await;
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let text = self
            .docs
            .lock()
            .unwrap()
            .get(&uri)
            .map(|doc| doc.text.clone())
            .unwrap_or_default();

        Ok(Some(SemanticTokensResult::Tokens(
            semantic_tokens_with_options(&text, self.compile_options),
        )))
    }
}

impl Backend {
    async fn publish_all(&self) {
        let docs = self.docs.lock().unwrap().clone();
        // ponytail: recompile each open document; group by project if latency becomes measurable.
        for (uri, doc) in &docs {
            let diagnostics =
                collect_document_diagnostics(uri, &doc.text, &docs, self.compile_options);
            self.client
                .publish_diagnostics(uri.clone(), diagnostics, doc.version)
                .await;
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

fn collect_diagnostics(
    uri: &lsp_types::Url,
    source: &str,
    result: &CompileResult,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for diagnostic in diagnostic_exts(result) {
        push_diagnostic(&mut out, uri, source, diagnostic);
    }
    out
}

fn collect_document_diagnostics(
    uri: &lsp_types::Url,
    source: &str,
    docs: &HashMap<lsp_types::Url, Document>,
    options: CompileOptions,
) -> Vec<Diagnostic> {
    let Ok(path) = uri.to_file_path() else {
        let result = riddlec::pipeline::compile_with_options(source, options);
        return collect_diagnostics(uri, source, &result);
    };
    let Some(root) = clue::find_project_root(&path) else {
        let result = riddlec::pipeline::compile_with_options(source, options);
        return collect_diagnostics(uri, source, &result);
    };
    let overlays = docs
        .iter()
        .filter_map(|(uri, document)| {
            uri.to_file_path()
                .ok()
                .map(|path| (path, document.text.clone()))
        })
        .collect::<HashMap<PathBuf, String>>();
    let analysis = match clue::analyze_project_with_options(&root, &overlays, options) {
        Ok(analysis) => analysis,
        Err(error) => return vec![project_error(error)],
    };
    let Ok(path) = fs::canonicalize(path) else {
        return Vec::new();
    };
    if !analysis.source.source_map.contains_file(&path) {
        let result = riddlec::pipeline::compile_with_options(source, options);
        return collect_diagnostics(uri, source, &result);
    }

    let entry = fs::canonicalize(&analysis.entry).ok();
    diagnostic_exts(&analysis.result)
        .filter_map(|diagnostic| {
            if diagnostic.labels.is_empty() && entry.as_deref() == Some(path.as_path()) {
                return Some(to_lsp(uri, source, diagnostic));
            }
            to_lsp_mapped(&path, &analysis.source.source_map, diagnostic)
        })
        .collect()
}

fn diagnostic_exts(result: &CompileResult) -> impl Iterator<Item = DiagnosticExt> + '_ {
    result
        .parse_errors
        .iter()
        .map(IntoDiagnosticExt::to_ext)
        .chain(result.hir_diagnostics.iter().map(IntoDiagnosticExt::to_ext))
        .chain(
            result
                .type_result
                .diagnostics
                .iter()
                .map(IntoDiagnosticExt::to_ext),
        )
        .chain(
            result
                .analysis_diagnostics
                .iter()
                .map(IntoDiagnosticExt::to_ext),
        )
}

fn to_lsp_mapped(
    current_path: &Path,
    source_map: &riddlec::pipeline::SourceMap,
    diagnostic: DiagnosticExt,
) -> Option<Diagnostic> {
    let primary = diagnostic
        .labels
        .first()
        .and_then(|label| source_map.map_range(label.range))?;
    if primary.path != current_path {
        return None;
    }

    let mut message = diagnostic.message;
    if let Some(help) = diagnostic.help {
        message.push_str("\nhelp: ");
        message.push_str(&help);
    }
    for note in diagnostic.notes {
        message.push_str("\nnote: ");
        message.push_str(&note);
    }
    let related_information = diagnostic
        .labels
        .iter()
        .skip(1)
        .filter(|label| !label.message.is_empty())
        .filter_map(|label| {
            let mapped = source_map.map_range(label.range)?;
            let uri = lsp_types::Url::from_file_path(mapped.path).ok()?;
            Some(DiagnosticRelatedInformation {
                location: Location::new(uri, range(mapped.source, mapped.range)),
                message: label.message.clone(),
            })
        })
        .collect::<Vec<_>>();

    Some(Diagnostic {
        range: range(primary.source, primary.range),
        severity: Some(severity(diagnostic.severity)),
        code: (!diagnostic.code.is_empty()).then(|| NumberOrString::String(diagnostic.code.into())),
        source: Some("riddle".into()),
        message,
        related_information: (!related_information.is_empty()).then_some(related_information),
        ..Diagnostic::default()
    })
}

fn project_error(error: impl std::fmt::Display) -> Diagnostic {
    Diagnostic {
        range: Range::default(),
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("clue".into()),
        message: error.to_string(),
        ..Diagnostic::default()
    }
}

fn push_diagnostic(
    out: &mut Vec<Diagnostic>,
    uri: &lsp_types::Url,
    source: &str,
    diagnostic: DiagnosticExt,
) {
    if diagnostic_in_source(source, &diagnostic) {
        out.push(to_lsp(uri, source, diagnostic));
    }
}

fn diagnostic_in_source(source: &str, diagnostic: &DiagnosticExt) -> bool {
    diagnostic
        .labels
        .first()
        .is_none_or(|label| range_in_source(source, label.range))
}

fn range_in_source(source: &str, range: TextRange) -> bool {
    usize::from(range.end()) <= source.len()
}

fn to_lsp(uri: &lsp_types::Url, source: &str, diagnostic: DiagnosticExt) -> Diagnostic {
    let primary = diagnostic
        .labels
        .first()
        .map(|label| label.range)
        .unwrap_or_else(|| TextRange::empty(0.into()));

    let mut message = diagnostic.message;
    if let Some(help) = diagnostic.help {
        message.push_str("\nhelp: ");
        message.push_str(&help);
    }
    for note in diagnostic.notes {
        message.push_str("\nnote: ");
        message.push_str(&note);
    }

    let related_information = diagnostic
        .labels
        .iter()
        .skip(1)
        .filter(|label| !label.message.is_empty() && range_in_source(source, label.range))
        .map(|label| DiagnosticRelatedInformation {
            location: Location::new(uri.clone(), range(source, label.range)),
            message: label.message.clone(),
        })
        .collect::<Vec<_>>();

    Diagnostic {
        range: range(source, primary),
        severity: Some(severity(diagnostic.severity)),
        code: (!diagnostic.code.is_empty()).then(|| NumberOrString::String(diagnostic.code.into())),
        source: Some("riddle".into()),
        message,
        related_information: (!related_information.is_empty()).then_some(related_information),
        ..Diagnostic::default()
    }
}

fn severity(severity: type_checker::Severity) -> DiagnosticSeverity {
    match severity {
        type_checker::Severity::Error => DiagnosticSeverity::ERROR,
        type_checker::Severity::Warning => DiagnosticSeverity::WARNING,
        type_checker::Severity::Note => DiagnosticSeverity::INFORMATION,
        type_checker::Severity::Help => DiagnosticSeverity::HINT,
    }
}

fn range(source: &str, range: TextRange) -> Range {
    Range::new(
        position(source, range.start().into()),
        position(source, range.end().into()),
    )
}

fn position(source: &str, offset: usize) -> Position {
    let offset = offset.min(source.len());
    let mut line = 0u32;
    let mut character = 0u32;

    for ch in source[..offset].chars() {
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }

    Position::new(line, character)
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
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::new("mutable"),
        ],
    }
}

#[cfg(test)]
fn semantic_tokens(source: &str) -> SemanticTokens {
    semantic_tokens_with_options(source, CompileOptions::default())
}

fn semantic_tokens_with_options(source: &str, options: CompileOptions) -> SemanticTokens {
    let tokens = frontend::lexer::lex(source);
    let result = riddlec::pipeline::compile_with_options(source, options);
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
        collect_hir_variable_tokens(hir, source, &mut raw_tokens);
    }

    raw_tokens.sort_by_key(|token| {
        (
            token.token_type != TOKEN_VARIABLE,
            token.range.start(),
            token.range.end(),
        )
    });
    let mut raw_tokens = remove_overlapping_tokens(raw_tokens);
    raw_tokens.sort_by_key(|token| (token.range.start(), token.range.end()));
    encode_semantic_tokens(source, raw_tokens)
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

fn collect_hir_variable_tokens(hir: &hir::HirFile, source: &str, out: &mut Vec<RawSemanticToken>) {
    let source_len = source.len();
    for (_, body) in hir.bodies.iter() {
        for (_, stmt) in body.stmts.iter() {
            if let Stmt::Let {
                name_range, is_mut, ..
            } = stmt
            {
                let Some(range) = *name_range else {
                    continue;
                };
                if !range_is_in_source(range, source_len) {
                    continue;
                }

                let mut modifiers = MOD_DECLARATION;
                if *is_mut {
                    modifiers |= MOD_MUTABLE;
                }
                out.push(RawSemanticToken {
                    range,
                    token_type: TOKEN_VARIABLE,
                    token_modifiers_bitset: modifiers,
                });
            }
        }

        for (expr_id, expr) in body.exprs.iter() {
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

            let Some(token_modifiers_bitset) =
                variable_modifiers_for_resolution(body, resolved.as_ref())
            else {
                continue;
            };
            out.push(RawSemanticToken {
                range,
                token_type: TOKEN_VARIABLE,
                token_modifiers_bitset,
            });
        }
    }
}

fn variable_modifiers_for_resolution(
    body: &hir::body::Body,
    resolved: Option<&ResolvedName>,
) -> Option<u32> {
    match resolved {
        Some(ResolvedName::Local(stmt_id)) => match &body.stmts[*stmt_id] {
            Stmt::Let { is_mut, .. } => Some(if *is_mut { MOD_MUTABLE } else { 0 }),
            _ => None,
        },
        Some(ResolvedName::Param(_)) => Some(0),
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
    let mut out = Vec::new();
    for token in raw_tokens {
        if out
            .iter()
            .any(|kept: &RawSemanticToken| ranges_overlap(kept.range, token.range))
        {
            continue;
        }
        out.push(token);
    }
    out
}

fn ranges_overlap(a: TextRange, b: TextRange) -> bool {
    a.start() < b.end() && b.start() < a.end()
}

fn encode_semantic_tokens(source: &str, raw_tokens: Vec<RawSemanticToken>) -> SemanticTokens {
    let mut data = Vec::new();
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

        let start = position(source, start_offset);
        let length = text.chars().map(char::len_utf16).sum::<usize>() as u32;
        let delta_line = start.line - prev_line;
        let delta_start = if delta_line == 0 {
            start.character - prev_start
        } else {
            start.character
        };

        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: token.token_type,
            token_modifiers_bitset: token.token_modifiers_bitset,
        });
        prev_line = start.line;
        prev_start = start.character;
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
        docs: Mutex::new(HashMap::new()),
        compile_options: opts.compile_options,
    });
    Server::new(stdin, stdout, socket)
        .concurrency_level(1)
        .serve(service)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "riddle-lsp-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn position_counts_utf16_columns() {
        let source = "a😀\nb";
        assert_eq!(position(source, 0), Position::new(0, 0));
        assert_eq!(position(source, "a😀".len()), Position::new(0, 3));
        assert_eq!(position(source, source.len()), Position::new(1, 1));
    }

    #[test]
    fn full_text_uses_latest_full_sync_change() {
        let old = TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "old".into(),
        };
        let new = TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "new".into(),
        };

        assert_eq!(full_text(vec![old, new]).as_deref(), Some("new"));
    }

    #[test]
    fn semantic_tokens_classifies_core_tokens() {
        let tokens = semantic_tokens("fun main() {\n  let mut x = \"hi\"; // ok\n}");
        let types = tokens
            .data
            .iter()
            .map(|token| token.token_type)
            .collect::<Vec<_>>();

        assert!(types.contains(&TOKEN_KEYWORD));
        assert!(types.contains(&TOKEN_FUNCTION));
        assert!(types.contains(&TOKEN_VARIABLE));
        assert!(types.contains(&TOKEN_STRING));
        assert!(types.contains(&TOKEN_COMMENT));
    }

    #[test]
    fn semantic_tokens_use_utf16_lengths() {
        let tokens = semantic_tokens("let x = '😀';");
        let string = tokens
            .data
            .iter()
            .find(|token| token.token_type == TOKEN_STRING)
            .unwrap();

        assert_eq!(string.length, 4);
    }

    #[test]
    fn semantic_tokens_mark_mutable_locals_like_rust_analyzer() {
        let tokens = semantic_tokens("fun main() { let x = 1; x; let mut y = 2; y; }");
        let variables = tokens
            .data
            .iter()
            .filter(|token| token.token_type == TOKEN_VARIABLE)
            .map(|token| token.token_modifiers_bitset)
            .collect::<Vec<_>>();

        assert_eq!(
            variables,
            vec![
                MOD_DECLARATION,
                0,
                MOD_DECLARATION | MOD_MUTABLE,
                MOD_MUTABLE
            ]
        );
    }

    #[test]
    fn semantic_tokens_prefer_resolved_variables_over_lexical_types() {
        let tokens = semantic_tokens("fun main() { let mut Foo = 1; Foo; }");
        let types = tokens
            .data
            .iter()
            .map(|token| token.token_type)
            .collect::<Vec<_>>();

        assert_eq!(
            types
                .iter()
                .filter(|token_type| **token_type == TOKEN_VARIABLE)
                .count(),
            2
        );
        assert!(!types.contains(&TOKEN_TYPE));
    }

    #[test]
    fn collect_diagnostics_ignores_appended_std_diagnostics() {
        let source = include_str!("../../../std/std/array.rid");
        let result = riddlec::pipeline::compile(source);
        let uri = lsp_types::Url::parse("file:///std/std/array.rid").unwrap();
        let diagnostics = collect_diagnostics(&uri, source, &result);

        assert!(
            diagnostics.iter().all(|diagnostic| !diagnostic
                .message
                .contains("expected IntoIter<T, N>, got IntoIter<T, N>")),
            "{diagnostics:#?}"
        );
    }

    #[test]
    fn project_diagnostics_use_unsaved_module_source() {
        let root = temp_root("project-diagnostics");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Clue.toml"),
            "[package]\nname = \"app\"\n\n[dependencies]\n",
        )
        .unwrap();
        fs::write(
            root.join("src/main.rid"),
            "mod util;\nfun main() -> i32 { util::value() }\n",
        )
        .unwrap();
        let util = root.join("src/util.rid");
        fs::write(&util, "pub fun value() -> i32 { 1 }\n").unwrap();
        let uri = lsp_types::Url::from_file_path(&util).unwrap();
        let text = "pub fun value() -> i32 { missing }\n".to_string();
        let docs = HashMap::from([(
            uri.clone(),
            Document {
                text: text.clone(),
                version: Some(1),
            },
        )]);

        let diagnostics =
            collect_document_diagnostics(&uri, &text, &docs, CompileOptions::default());

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("unresolved name: `missing`")),
            "{diagnostics:#?}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_diagnostics_follow_peer_overlay_removal() {
        let root = temp_root("peer-overlay-removal");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Clue.toml"),
            "[package]\nname = \"app\"\n\n[dependencies]\n",
        )
        .unwrap();
        let main = root.join("src/main.rid");
        let main_text = "mod util;\nfun main() -> i32 { util::value() }\n".to_string();
        fs::write(&main, &main_text).unwrap();
        let util = root.join("src/util.rid");
        fs::write(&util, "pub fun value() -> i32 { 1 }\n").unwrap();
        let main_uri = lsp_types::Url::from_file_path(&main).unwrap();
        let util_uri = lsp_types::Url::from_file_path(&util).unwrap();
        let mut docs = HashMap::from([
            (
                main_uri.clone(),
                Document {
                    text: main_text.clone(),
                    version: Some(1),
                },
            ),
            (
                util_uri.clone(),
                Document {
                    text: "pub fun other() -> i32 { 1 }\n".into(),
                    version: Some(1),
                },
            ),
        ]);

        let stale =
            collect_document_diagnostics(&main_uri, &main_text, &docs, CompileOptions::default());
        assert!(
            stale
                .iter()
                .any(|diagnostic| diagnostic.message.contains("unresolved")),
            "{stale:#?}"
        );

        docs.remove(&util_uri);
        let refreshed =
            collect_document_diagnostics(&main_uri, &main_text, &docs, CompileOptions::default());
        assert!(refreshed.is_empty(), "{refreshed:#?}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parse_args_accepts_no_std() {
        let args = vec!["riddle-lsp".into(), "--no-std".into()];
        let opts = parse_args(&args).unwrap();

        assert!(!opts.compile_options.use_std);
    }

    #[test]
    fn semantic_tokens_place_local_declaration_and_use_on_identifier() {
        let source = "fun main() {\n  let mut foo_bar = 1; foo_bar;\n}";
        let tokens = semantic_token_positions(&semantic_tokens(source));
        let variables = tokens
            .iter()
            .filter(|token| token.token_type == TOKEN_VARIABLE)
            .collect::<Vec<_>>();

        assert_eq!(
            variables,
            vec![
                &SemanticTokenPosition {
                    line: 1,
                    start: 10,
                    length: 7,
                    token_type: TOKEN_VARIABLE,
                    token_modifiers_bitset: MOD_DECLARATION | MOD_MUTABLE,
                },
                &SemanticTokenPosition {
                    line: 1,
                    start: 23,
                    length: 7,
                    token_type: TOKEN_VARIABLE,
                    token_modifiers_bitset: MOD_MUTABLE,
                },
            ]
        );
    }

    #[derive(Debug, PartialEq, Eq)]
    struct SemanticTokenPosition {
        line: u32,
        start: u32,
        length: u32,
        token_type: u32,
        token_modifiers_bitset: u32,
    }

    fn semantic_token_positions(tokens: &SemanticTokens) -> Vec<SemanticTokenPosition> {
        let mut line = 0;
        let mut start = 0;
        tokens
            .data
            .iter()
            .map(|token| {
                line += token.delta_line;
                if token.delta_line == 0 {
                    start += token.delta_start;
                } else {
                    start = token.delta_start;
                }
                SemanticTokenPosition {
                    line,
                    start,
                    length: token.length,
                    token_type: token.token_type,
                    token_modifiers_bitset: token.token_modifiers_bitset,
                }
            })
            .collect()
    }
}
