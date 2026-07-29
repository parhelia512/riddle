use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use lsp_types::request::{GotoImplementationParams, GotoImplementationResponse};
use lsp_types::{
    CodeActionKind, CodeActionParams, CodeActionResponse, CompletionOptions, CompletionParams,
    CompletionResponse, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, FileSystemWatcher, GlobPattern, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverParams, HoverProviderCapability,
    ImplementationProviderCapability, InitializeParams, InitializeResult, InitializedParams,
    InlayHint, InlayHintParams, MessageType, OneOf, PositionEncodingKind, Registration,
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
    navigation::{definition_for_document, hover_for_document, implementation_for_document},
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
    analysis_revisions: Arc<RequestRevisions>,
    completion_revisions: Arc<RequestRevisions>,
    semantic_tokens: Arc<Mutex<HashMap<lsp_types::Url, CachedSemanticTokens>>>,
    semantic_token_revision: Arc<AtomicU64>,
    supports_watched_files: AtomicBool,
    compile_options: CompileOptions,
    completion_delay: Duration,
}

const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub text: String,
    pub version: Option<i32>,
}

#[derive(Clone)]
struct CachedSemanticTokens {
    text: String,
    project_revision: u64,
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
        self.current(uri) == revision
    }

    pub fn current(&self, uri: &lsp_types::Url) -> u64 {
        self.0.lock().unwrap().get(uri).copied().unwrap_or(0)
    }

    pub fn remove(&self, uri: &lsp_types::Url) {
        self.0.lock().unwrap().remove(uri);
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        self.supports_watched_files.store(
            params
                .capabilities
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.did_change_watched_files.as_ref())
                .and_then(|capabilities| capabilities.dynamic_registration)
                .unwrap_or(false),
            Ordering::SeqCst,
        );
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(PositionEncodingKind::UTF16),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                code_action_provider: Some(true.into()),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
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
        if self.supports_watched_files.load(Ordering::SeqCst) {
            let options = DidChangeWatchedFilesRegistrationOptions {
                watchers: ["**/*.rid", "**/Clue.toml"]
                    .into_iter()
                    .map(|pattern| FileSystemWatcher {
                        glob_pattern: GlobPattern::String(pattern.into()),
                        kind: None,
                    })
                    .collect(),
            };
            let registration = Registration {
                id: "riddle-watched-files".into(),
                method: "workspace/didChangeWatchedFiles".into(),
                register_options: Some(
                    serde_json::to_value(options)
                        .expect("watched-file registration options must serialize"),
                ),
            };
            if let Err(error) = self.client.register_capability(vec![registration]).await {
                self.client
                    .log_message(
                        MessageType::WARNING,
                        format!("failed to register file watchers: {error}"),
                    )
                    .await;
            }
        }
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
        docs.insert(uri.clone(), doc);
        bump_related_revisions(&self.analysis_revisions, &docs, &uri);
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
        bump_related_revisions(&self.analysis_revisions, &docs, &uri);
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
            let related = related_document_uris(&docs, &uri);
            docs.remove(&uri);
            for related_uri in related {
                self.analysis_revisions.begin(&related_uri);
            }
            docs.clone()
        };
        self.semantic_tokens.lock().unwrap().remove(&uri);
        self.analysis_revisions.remove(&uri);
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
        let Some((docs, text, analysis_revision)) = self.analysis_snapshot(&uri) else {
            return Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
                result_id: None,
                data: Vec::new(),
            })));
        };
        let project_revision = self.analysis_sessions.current_revision(&uri, &docs);
        if let Some(cached) = self
            .semantic_tokens
            .lock()
            .unwrap()
            .get(&uri)
            .filter(|cached| {
                cached.text == text && project_revision == Some(cached.project_revision)
            })
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
        let project_revision = self.analysis_sessions.revision(&uri);
        if !self.analysis_is_current(&uri, &text, analysis_revision) {
            return Ok(None);
        }
        self.semantic_tokens.lock().unwrap().insert(
            uri,
            CachedSemanticTokens {
                text,
                project_revision,
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
        let Some((docs, text, analysis_revision)) = self.analysis_snapshot(&uri) else {
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
        if !self.analysis_is_current(&uri, &text, analysis_revision) {
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
        let Some((docs, text, analysis_revision)) = self.analysis_snapshot(&uri) else {
            return Ok(Some(CompletionResponse::Array(Vec::new())));
        };
        let compile_options = self.compile_options;
        let analysis_sessions = Arc::clone(&self.completion_sessions);
        let fallback_sessions = Arc::clone(&self.completion_fallback_sessions);
        let completion_revisions = Arc::clone(&self.completion_revisions);
        let current_analysis_revisions = Arc::clone(&self.analysis_revisions);
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
                        || !current_analysis_revisions
                            .is_current(&completion_uri, analysis_revision)
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
        if !self.analysis_is_current(&uri, &text, analysis_revision)
            || !self.completion_revisions.is_current(&uri, request_revision)
        {
            return Ok(None);
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let result = tokio::task::spawn_blocking(move || {
            hover_for_document(&analysis_uri, &docs, position, options, &sessions)
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let hover = match result {
            Ok(hover) => hover,
            Err(error) => {
                self.client
                    .log_message(MessageType::ERROR, format!("hover failed: {error}"))
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(hover)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let result = tokio::task::spawn_blocking(move || {
            definition_for_document(&analysis_uri, &docs, position, options, &sessions)
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let definition = match result {
            Ok(definition) => definition,
            Err(error) => {
                self.client
                    .log_message(MessageType::ERROR, format!("definition failed: {error}"))
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(definition)
    }

    async fn goto_implementation(
        &self,
        params: GotoImplementationParams,
    ) -> Result<Option<GotoImplementationResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let result = tokio::task::spawn_blocking(move || {
            implementation_for_document(&analysis_uri, &docs, position, options, &sessions)
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let implementation = match result {
            Ok(implementation) => implementation,
            Err(error) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("implementation failed: {error}"),
                    )
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(implementation)
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        if params.context.only.as_ref().is_some_and(|kinds| {
            !kinds
                .iter()
                .any(|kind| kind.as_str().is_empty() || kind == &CodeActionKind::QUICKFIX)
        }) {
            return Ok(Some(Vec::new()));
        }
        let uri = params.text_document.uri;
        let Some(document) = self.docs.lock().unwrap().get(&uri).cloned() else {
            return Ok(Some(Vec::new()));
        };
        let Some(published) = self.published.lock().unwrap().get(&uri).cloned() else {
            return Ok(Some(Vec::new()));
        };
        if published.version != document.version {
            return Ok(Some(Vec::new()));
        }
        let diagnostics = params
            .context
            .diagnostics
            .into_iter()
            .filter(|diagnostic| published.diagnostics.contains(diagnostic))
            .collect::<Vec<_>>();
        Ok(Some(quick_fixes(&uri, document.version, &diagnostics)))
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let reset_all = params.changes.iter().any(|change| is_manifest(&change.uri));
        {
            let docs = self.docs.lock().unwrap();
            for change in &params.changes {
                bump_related_revisions(&self.analysis_revisions, &docs, &change.uri);
            }
        }
        if reset_all {
            *self.diagnostic_sessions.lock().unwrap() = DiagnosticSessions::default();
            self.completion_sessions.clear_projects();
            self.completion_fallback_sessions.clear_projects();
            self.analysis_sessions.clear_projects();
        } else {
            for change in &params.changes {
                self.diagnostic_sessions
                    .lock()
                    .unwrap()
                    .invalidate_project(&change.uri);
                self.completion_sessions.invalidate_project(&change.uri);
                self.completion_fallback_sessions
                    .invalidate_project(&change.uri);
                self.analysis_sessions.invalidate_project(&change.uri);
            }
        }
        self.schedule_diagnostics();
    }
}

fn is_manifest(uri: &lsp_types::Url) -> bool {
    uri.path_segments()
        .and_then(|mut segments| segments.next_back())
        == Some("Clue.toml")
}

pub fn documents_for_uri(
    docs: &HashMap<lsp_types::Url, Document>,
    uri: &lsp_types::Url,
) -> HashMap<lsp_types::Url, Document> {
    let Some(root) = project_root(uri) else {
        return docs
            .get(uri)
            .cloned()
            .map(|document| HashMap::from([(uri.clone(), document)]))
            .unwrap_or_default();
    };
    docs.iter()
        .filter(|(candidate, _)| project_root(candidate).as_ref() == Some(&root))
        .map(|(uri, document)| (uri.clone(), document.clone()))
        .collect()
}

fn project_root(uri: &lsp_types::Url) -> Option<std::path::PathBuf> {
    uri.to_file_path()
        .ok()
        .and_then(|path| clue::find_project_root(&path))
}

fn related_document_uris(
    docs: &HashMap<lsp_types::Url, Document>,
    uri: &lsp_types::Url,
) -> Vec<lsp_types::Url> {
    documents_for_uri(docs, uri).into_keys().collect()
}

fn bump_related_revisions(
    revisions: &RequestRevisions,
    docs: &HashMap<lsp_types::Url, Document>,
    uri: &lsp_types::Url,
) {
    for related in related_document_uris(docs, uri) {
        revisions.begin(&related);
    }
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
            analysis_revisions: Arc::new(RequestRevisions::default()),
            completion_revisions: Arc::new(RequestRevisions::default()),
            semantic_tokens: Arc::new(Mutex::new(HashMap::new())),
            semantic_token_revision: Arc::new(AtomicU64::new(1)),
            supports_watched_files: AtomicBool::new(false),
            compile_options,
            completion_delay,
        }
    }

    fn analysis_snapshot(
        &self,
        uri: &lsp_types::Url,
    ) -> Option<(HashMap<lsp_types::Url, Document>, String, u64)> {
        let all_docs = self.docs.lock().unwrap();
        let text = all_docs.get(uri)?.text.clone();
        let docs = documents_for_uri(&all_docs, uri);
        let revision = self.analysis_revisions.current(uri);
        Some((docs, text, revision))
    }

    fn analysis_is_current(&self, uri: &lsp_types::Url, text: &str, revision: u64) -> bool {
        if !self.analysis_revisions.is_current(uri, revision) {
            return false;
        }
        let unchanged = self
            .docs
            .lock()
            .unwrap()
            .get(uri)
            .is_some_and(|document| document.text == text);
        unchanged && self.analysis_revisions.is_current(uri, revision)
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
