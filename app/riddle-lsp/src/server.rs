use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use lsp_types::{
    CodeActionParams, CodeActionResponse, CompletionOptions, CompletionParams, CompletionResponse,
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, FileChangeType, FileEvent, InitializeParams, InitializeResult,
    InitializedParams, InlayHint, InlayHintParams, MessageType, OneOf, PositionEncodingKind,
    SemanticTokens, SemanticTokensDeltaParams, SemanticTokensFullDeltaResult,
    SemanticTokensFullOptions, SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind,
};
use riddlec::pipeline::CompileOptions;
use tower_lsp::jsonrpc::Result;
use tower_lsp::{Client, LanguageServer};

use crate::{
    code_actions::quick_fixes,
    completion::{completion_items_for_document, completion_trigger_characters},
    diagnostics::{self, DiagnosticSessions, collect_workspace_diagnostics_cancellable},
    inlay_hints::inlay_hints_for_document,
    semantic_tokens::{semantic_token_delta, semantic_tokens_for_document, semantic_tokens_legend},
    session::AnalysisSessions,
    text::apply_content_changes,
};

pub(crate) struct Backend {
    client: Client,
    docs: Arc<Mutex<HashMap<lsp_types::Url, Document>>>,
    published: Arc<Mutex<HashMap<lsp_types::Url, diagnostics::PublishedDiagnostics>>>,
    publish_gate: Arc<tokio::sync::Mutex<()>>,
    diagnostic_revision: Arc<AtomicU64>,
    diagnostic_sessions: Arc<Mutex<DiagnosticSessions>>,
    /// Sessions used for the marker-source analysis (the source with COMPLETION_MARKER
    /// inserted). Kept separate from `completion_fallback_sessions` so the two
    /// IncrementalParsers never thrash each other's cache.
    completion_sessions: Arc<AnalysisSessions>,
    /// Sessions used for the fallback analysis (the original, unmodified source).
    completion_fallback_sessions: Arc<AnalysisSessions>,
    analysis_sessions: Arc<AnalysisSessions>,
    // ponytail: workspace-wide invalidation; shard by project if large workspaces show churn.
    analysis_revision: Arc<AtomicU64>,
    completion_revisions: Arc<RequestRevisions>,
    semantic_tokens: Arc<Mutex<HashMap<lsp_types::Url, CachedSemanticTokens>>>,
    semantic_token_revision: Arc<AtomicU64>,
    compile_options: CompileOptions,
    completion_delay: Duration,
}

const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct Document {
    pub text: String,
    pub version: Option<i32>,
}

#[derive(Clone)]
struct CachedSemanticTokens {
    text: String,
    analysis_revision: u64,
    tokens: SemanticTokens,
}

#[derive(Default)]
pub struct RequestRevisions(Mutex<HashMap<lsp_types::Url, u64>>);

impl RequestRevisions {
    pub fn begin(&self, uri: &lsp_types::Url) -> u64 {
        let mut revisions = self.0.lock().unwrap();
        let revision = revisions.entry(uri.clone()).or_default();
        *revision += 1;
        *revision
    }

    pub fn is_current(&self, uri: &lsp_types::Url, revision: u64) -> bool {
        self.0.lock().unwrap().get(uri) == Some(&revision)
    }

    pub fn remove(&self, uri: &lsp_types::Url) {
        self.0.lock().unwrap().remove(uri);
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(PositionEncodingKind::UTF16),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                code_action_provider: Some(true.into()),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: Some(completion_trigger_characters()),
                    ..CompletionOptions::default()
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(SemanticTokensServerCapabilities::from(
                    SemanticTokensOptions {
                        legend: semantic_tokens_legend(),
                        full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
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
        let mut docs = self.docs.lock().unwrap();
        docs.insert(uri, doc);
        self.analysis_revision.fetch_add(1, Ordering::SeqCst);
        drop(docs);
        self.schedule_diagnostics();
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let mut docs = self.docs.lock().unwrap();
        let Some(document) = docs.get_mut(&uri) else {
            return;
        };
        if !apply_content_changes(&mut document.text, params.content_changes) {
            return;
        }
        document.version = Some(params.text_document.version);
        self.analysis_revision.fetch_add(1, Ordering::SeqCst);
        drop(docs);
        self.schedule_diagnostics();
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        // Remove the document and capture the current open set in a single
        // critical section to avoid a TOCTOU race between remove() and
        // the retain_open() calls below.
        let open_docs = {
            let mut docs = self.docs.lock().unwrap();
            docs.remove(&uri);
            self.analysis_revision.fetch_add(1, Ordering::SeqCst);
            docs.clone()
        };
        self.semantic_tokens.lock().unwrap().remove(&uri);
        self.completion_revisions.remove(&uri);
        self.completion_sessions.retain_open(&open_docs);
        self.completion_fallback_sessions.retain_open(&open_docs);
        self.analysis_sessions.retain_open(&open_docs);
        self.schedule_diagnostics();
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let (docs, analysis_revision) = {
            let docs = self.docs.lock().unwrap();
            (docs.clone(), self.analysis_revision.load(Ordering::SeqCst))
        };
        let Some(text) = docs.get(&uri).map(|doc| doc.text.clone()) else {
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
            .filter(|cached| cached.text == text && cached.analysis_revision == analysis_revision)
        {
            return Ok(Some(SemanticTokensResult::Tokens(cached.tokens.clone())));
        }

        let compile_options = self.compile_options;
        let analysis_sessions = Arc::clone(&self.analysis_sessions);
        let analysis_uri = uri.clone();
        let analyzed = tokio::task::spawn_blocking(move || {
            semantic_tokens_for_document(&analysis_uri, &docs, compile_options, &analysis_sessions)
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let mut tokens = match analyzed {
            Ok(tokens) => tokens,
            Err(error) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("semantic tokens failed: {error}"),
                    )
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        tokens.result_id = Some(
            self.semantic_token_revision
                .fetch_add(1, Ordering::SeqCst)
                .to_string(),
        );
        let is_current = self.analysis_revision.load(Ordering::SeqCst) == analysis_revision
            && self
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
                analysis_revision,
                tokens: tokens.clone(),
            },
        );

        Ok(Some(SemanticTokensResult::Tokens(tokens)))
    }

    async fn semantic_tokens_full_delta(
        &self,
        params: SemanticTokensDeltaParams,
    ) -> Result<Option<SemanticTokensFullDeltaResult>> {
        let uri = params.text_document.uri;
        let previous = self.semantic_tokens.lock().unwrap().get(&uri).cloned();
        let full = self
            .semantic_tokens_full(SemanticTokensParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                work_done_progress_params: params.work_done_progress_params,
                partial_result_params: params.partial_result_params,
            })
            .await?;
        let Some(SemanticTokensResult::Tokens(tokens)) = full else {
            return Ok(None);
        };
        let Some(previous) = previous.filter(|cached| {
            cached.tokens.result_id.as_deref() == Some(params.previous_result_id.as_str())
        }) else {
            return Ok(Some(SemanticTokensFullDeltaResult::Tokens(tokens)));
        };
        Ok(Some(SemanticTokensFullDeltaResult::TokensDelta(
            semantic_token_delta(
                &previous.tokens.data,
                &tokens.data,
                tokens.result_id.clone().unwrap_or_default(),
            ),
        )))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let (docs, analysis_revision) = {
            let docs = self.docs.lock().unwrap();
            (docs.clone(), self.analysis_revision.load(Ordering::SeqCst))
        };
        let Some(text) = docs.get(&uri).map(|document| document.text.clone()) else {
            return Ok(Some(Vec::new()));
        };
        let compile_options = self.compile_options;
        let analysis_sessions = Arc::clone(&self.analysis_sessions);
        let analysis_uri = uri.clone();
        let analyzed = tokio::task::spawn_blocking(move || {
            inlay_hints_for_document(
                &analysis_uri,
                &docs,
                params.range,
                compile_options,
                &analysis_sessions,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let hints = match analyzed {
            Ok(hints) => hints,
            Err(error) => {
                self.client
                    .log_message(MessageType::ERROR, format!("inlay hints failed: {error}"))
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if self.analysis_revision.load(Ordering::SeqCst) != analysis_revision
            || self
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
        let request_revision = self.completion_revisions.begin(&uri);
        if !self.completion_delay.is_zero() {
            tokio::time::sleep(self.completion_delay).await;
            if !self.completion_revisions.is_current(&uri, request_revision) {
                return Ok(None);
            }
        }
        let (docs, analysis_revision) = {
            let docs = self.docs.lock().unwrap();
            (docs.clone(), self.analysis_revision.load(Ordering::SeqCst))
        };
        let Some(text) = docs.get(&uri).map(|document| document.text.clone()) else {
            return Ok(Some(CompletionResponse::Array(Vec::new())));
        };
        let compile_options = self.compile_options;
        let analysis_sessions = Arc::clone(&self.completion_sessions);
        let fallback_sessions = Arc::clone(&self.completion_fallback_sessions);
        let completion_revisions = Arc::clone(&self.completion_revisions);
        let current_analysis_revision = Arc::clone(&self.analysis_revision);
        let completion_uri = uri.clone();
        let analysis = tokio::task::spawn_blocking(move || {
            completion_items_for_document(
                &completion_uri,
                &docs,
                position,
                compile_options,
                &analysis_sessions,
                &fallback_sessions,
                || {
                    !completion_revisions.is_current(&completion_uri, request_revision)
                        || current_analysis_revision.load(Ordering::SeqCst) != analysis_revision
                },
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let items = match analysis {
            Ok(Some(items)) => items,
            Ok(None) => return Ok(None),
            Err(error) => {
                self.client
                    .log_message(MessageType::ERROR, format!("completion failed: {error}"))
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if self.analysis_revision.load(Ordering::SeqCst) != analysis_revision
            || self
                .docs
                .lock()
                .unwrap()
                .get(&uri)
                .is_none_or(|document| document.text != text)
            || !self.completion_revisions.is_current(&uri, request_revision)
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
        self.analysis_revision.fetch_add(1, Ordering::SeqCst);
        if params.changes.iter().any(watched_change_resets_sessions) {
            *self.diagnostic_sessions.lock().unwrap() = DiagnosticSessions::default();
            self.completion_sessions.clear_projects();
            self.completion_fallback_sessions.clear_projects();
            self.analysis_sessions.clear_projects();
        }
        self.schedule_diagnostics();
    }
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
    pub(crate) fn new(
        client: Client,
        compile_options: CompileOptions,
        completion_delay: Duration,
    ) -> Self {
        Self {
            client,
            docs: Arc::new(Mutex::new(HashMap::new())),
            published: Arc::new(Mutex::new(HashMap::new())),
            publish_gate: Arc::new(tokio::sync::Mutex::new(())),
            diagnostic_revision: Arc::new(AtomicU64::new(0)),
            diagnostic_sessions: Arc::new(Mutex::new(DiagnosticSessions::default())),
            completion_sessions: Arc::new(AnalysisSessions::default()),
            completion_fallback_sessions: Arc::new(AnalysisSessions::default()),
            analysis_sessions: Arc::new(AnalysisSessions::default()),
            analysis_revision: Arc::new(AtomicU64::new(1)),
            completion_revisions: Arc::new(RequestRevisions::default()),
            semantic_tokens: Arc::new(Mutex::new(HashMap::new())),
            semantic_token_revision: Arc::new(AtomicU64::new(1)),
            compile_options,
            completion_delay,
        }
    }

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
