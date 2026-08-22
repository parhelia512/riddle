use std::{
    collections::{HashMap, HashSet},
    hash::BuildHasher,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use lsp_types::request::{
    GotoDeclarationParams, GotoDeclarationResponse, GotoImplementationParams,
    GotoImplementationResponse, GotoTypeDefinitionParams, GotoTypeDefinitionResponse,
};
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    CallHierarchyServerCapability, CodeActionKind, CodeActionParams, CodeActionResponse,
    CompletionList, CompletionOptions, CompletionParams, CompletionResponse, CompletionTriggerKind,
    DeclarationCapability, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidChangeWorkspaceFoldersParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentFormattingParams,
    DocumentHighlight, DocumentHighlightParams, DocumentSymbolParams, DocumentSymbolResponse,
    FileSystemWatcher, FoldingRange, FoldingRangeParams, FoldingRangeProviderCapability,
    GlobPattern, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams,
    HoverProviderCapability, ImplementationProviderCapability, InitializeParams, InitializeResult,
    InitializedParams, InlayHint, InlayHintParams, MessageType, OneOf, PositionEncodingKind,
    PrepareRenameResponse, ReferenceParams, Registration, RenameOptions, RenameParams,
    SemanticTokens, SemanticTokensDeltaParams, SemanticTokensFullDeltaResult,
    SemanticTokensFullOptions, SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, SignatureHelp,
    SignatureHelpOptions, SignatureHelpParams, SymbolInformation, TextDocumentPositionParams,
    TextDocumentRegistrationOptions, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit,
    TypeDefinitionProviderCapability, TypeHierarchyItem, TypeHierarchyPrepareParams,
    TypeHierarchyRegistrationOptions, TypeHierarchySubtypesParams, TypeHierarchySupertypesParams,
    WorkspaceEdit, WorkspaceFoldersServerCapabilities, WorkspaceServerCapabilities,
    WorkspaceSymbolParams,
};
use riddlec::pipeline::CompileOptions;
use tower_lsp::jsonrpc::Result;
use tower_lsp::{Client, LanguageServer};

use crate::{
    code_actions::quick_fixes,
    completion::{
        completion_items_for_document, completion_trigger_characters, completion_trigger_is_active,
    },
    diagnostics::{self, DiagnosticSessions, collect_workspace_diagnostics_cancellable},
    editor_features::{
        document_symbols_for_document_cancellable, folding_ranges, format_source,
        workspace_symbols_for_document_cancellable,
    },
    hierarchy::{
        incoming_calls as hierarchy_incoming_calls, outgoing_calls as hierarchy_outgoing_calls,
        prepare_call_hierarchy as hierarchy_prepare_call,
        prepare_type_hierarchy as hierarchy_prepare_type, subtypes as hierarchy_subtypes,
        supertypes as hierarchy_supertypes,
    },
    index::{project_index_for_root_cancellable, workspace_symbols_for_index},
    inlay_hints::inlay_hints_for_document_cancellable,
    navigation::{
        definition_for_document_cancellable, document_highlights_for_document_cancellable,
        hover_for_document_cancellable, implementation_for_document_cancellable,
        prepare_rename_for_document_cancellable, references_for_document_cancellable,
        rename_for_document_cancellable, signature_help_for_document_cancellable,
        type_definition_for_document_cancellable, validate_identifier,
    },
    semantic_tokens::{
        semantic_token_delta, semantic_tokens_for_document_cancellable, semantic_tokens_legend,
    },
    session::AnalysisSessions,
    text::{LineIndex, apply_content_changes},
    workspace::WorkspaceState,
};

pub struct Backend {
    client: Client,
    docs: Arc<Mutex<HashMap<lsp_types::Url, Document>>>,
    published: Arc<Mutex<HashMap<lsp_types::Url, diagnostics::PublishedDiagnostics>>>,
    publish_gate: Arc<tokio::sync::Mutex<()>>,
    diagnostic_revision: Arc<AtomicU64>,
    diagnostic_sessions: Arc<Mutex<DiagnosticSessions>>,
    /// Sessions used for marker-source analysis (the source with the completion marker).
    completion_sessions: Arc<AnalysisSessions>,
    /// Shared sessions for every analysis of the original, unmodified source.
    analysis_sessions: Arc<AnalysisSessions>,
    analysis_revisions: Arc<RequestRevisions>,
    completion_revisions: Arc<RequestRevisions>,
    semantic_tokens: Arc<Mutex<HashMap<lsp_types::Url, CachedSemanticTokens>>>,
    semantic_token_revision: Arc<AtomicU64>,
    supports_watched_files: AtomicBool,
    supports_type_hierarchy: AtomicBool,
    workspace: Arc<WorkspaceState>,
    compile_options: CompileOptions,
    completion_delay: Duration,
}

const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(150);
const INDEX_DEBOUNCE: Duration = Duration::from_millis(300);

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
    /// Starts a new request revision for a document.
    ///
    /// # Panics
    ///
    /// Panics if the revision mutex is poisoned.
    pub fn begin(&self, uri: &lsp_types::Url) -> u64 {
        let mut revisions = self.0.lock().unwrap();
        let revision = revisions.entry(uri.clone()).or_default();
        *revision += 1;
        let current = *revision;
        drop(revisions);
        current
    }

    /// Returns whether a request revision is still current.
    ///
    /// # Panics
    ///
    /// Panics if the revision mutex is poisoned.
    pub fn is_current(&self, uri: &lsp_types::Url, revision: u64) -> bool {
        self.current(uri) == revision
    }

    /// Returns the current request revision for a document.
    ///
    /// # Panics
    ///
    /// Panics if the revision mutex is poisoned.
    pub fn current(&self, uri: &lsp_types::Url) -> u64 {
        self.0.lock().unwrap().get(uri).copied().unwrap_or(0)
    }

    /// Removes the request revision for a document.
    ///
    /// # Panics
    ///
    /// Panics if the revision mutex is poisoned.
    pub fn remove(&self, uri: &lsp_types::Url) {
        self.0.lock().unwrap().remove(uri);
    }
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        code_action_provider: Some(true.into()),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        declaration_provider: Some(DeclarationCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
        implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
        call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: lsp_types::WorkDoneProgressOptions::default(),
        })),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            trigger_characters: Some(completion_trigger_characters()),
            ..CompletionOptions::default()
        }),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into()]),
            retrigger_characters: Some(vec![",".into()]),
            ..SignatureHelpOptions::default()
        }),
        inlay_hint_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::from(
            SemanticTokensOptions {
                legend: semantic_tokens_legend(),
                full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                ..SemanticTokensOptions::default()
            },
        )),
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: Some(OneOf::Left(true)),
            }),
            ..WorkspaceServerCapabilities::default()
        }),
        ..ServerCapabilities::default()
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
        self.supports_type_hierarchy.store(
            params
                .capabilities
                .text_document
                .as_ref()
                .and_then(|capabilities| capabilities.type_hierarchy.as_ref())
                .and_then(|capabilities| capabilities.dynamic_registration)
                .unwrap_or(false),
            Ordering::SeqCst,
        );
        let workspace_roots = params
            .workspace_folders
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|folder| folder.uri.to_file_path().ok())
            .chain(
                params
                    .workspace_folders
                    .is_none()
                    .then_some(params.root_uri.as_ref())
                    .flatten()
                    .and_then(|uri| uri.to_file_path().ok()),
            );
        if let Err(error) = self.workspace.set_roots(workspace_roots) {
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("failed to discover workspace projects: {error}"),
                )
                .await;
        }
        Ok(InitializeResult {
            capabilities: server_capabilities(),
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
        if self.supports_type_hierarchy.load(Ordering::SeqCst) {
            let options = TypeHierarchyRegistrationOptions {
                text_document_registration_options: TextDocumentRegistrationOptions {
                    document_selector: None,
                },
                ..TypeHierarchyRegistrationOptions::default()
            };
            let registration = Registration {
                id: "riddle-type-hierarchy".into(),
                method: "textDocument/prepareTypeHierarchy".into(),
                register_options: Some(
                    serde_json::to_value(options)
                        .expect("type hierarchy registration options must serialize"),
                ),
            };
            if let Err(error) = self.client.register_capability(vec![registration]).await {
                self.client
                    .log_message(
                        MessageType::WARNING,
                        format!("failed to register type hierarchy: {error}"),
                    )
                    .await;
            }
        }
        self.client
            .log_message(MessageType::INFO, "riddle-lsp initialized")
            .await;
        self.schedule_workspace_indexing();
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
        self.schedule_document_indexing(&uri);
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
        self.schedule_document_indexing(&uri);
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
        self.analysis_sessions.retain_open(&open_docs);
        self.schedule_diagnostics();
        self.schedule_document_indexing(&uri);
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let Some(document) = self
            .docs
            .lock()
            .unwrap()
            .get(&params.text_document.uri)
            .cloned()
        else {
            return Ok(None);
        };
        let formatted = format_source(
            &document.text,
            params.options.tab_size,
            params.options.insert_spaces,
        );
        if formatted == document.text {
            return Ok(Some(Vec::new()));
        }
        let end = LineIndex::new(&document.text)
            .position(&document.text, document.text.len())
            .unwrap_or_default();
        Ok(Some(vec![TextEdit::new(
            lsp_types::Range::new(lsp_types::Position::new(0, 0), end),
            formatted,
        )]))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let Some(document) = self
            .docs
            .lock()
            .unwrap()
            .get(&params.text_document.uri)
            .cloned()
        else {
            return Ok(None);
        };
        Ok(Some(folding_ranges(&document.text)))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let revisions = Arc::clone(&self.analysis_revisions);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            document_symbols_for_document_cancellable(
                &analysis_uri,
                &docs,
                options,
                &sessions,
                &cancelled,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let symbols = match result {
            Ok(Some(symbols)) => symbols,
            Ok(None) => return Ok(None),
            Err(error) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("document symbols failed: {error}"),
                    )
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    #[allow(deprecated)]
    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let docs = self.docs.lock().unwrap().clone();
        let uris = docs.keys().cloned().collect::<Vec<_>>();
        let open_uris = uris.iter().cloned().collect::<HashSet<_>>();
        let projects = self.workspace.projects();
        let workspace = Arc::clone(&self.workspace);
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let revisions = Arc::clone(&self.analysis_revisions);
        let query = params.query;
        let result = tokio::task::spawn_blocking(move || {
            let mut symbols = Vec::new();
            for project in projects {
                if let Some(index) = workspace.snapshot(&project) {
                    symbols.extend(
                        workspace_symbols_for_index(&index, &query)
                            .into_iter()
                            .filter(|symbol| !open_uris.contains(&symbol.location.uri)),
                    );
                }
            }
            for uri in uris {
                let revision = revisions.current(&uri);
                let cancelled = || !revisions.is_current(&uri, revision);
                if let Some(found) = workspace_symbols_for_document_cancellable(
                    &uri, &docs, &query, options, &sessions, &cancelled,
                )? {
                    symbols.extend(found);
                }
            }
            symbols.sort_by(|left, right| {
                left.name
                    .cmp(&right.name)
                    .then_with(|| left.location.uri.as_str().cmp(right.location.uri.as_str()))
                    .then_with(|| {
                        left.location
                            .range
                            .start
                            .line
                            .cmp(&right.location.range.start.line)
                    })
                    .then_with(|| {
                        left.location
                            .range
                            .start
                            .character
                            .cmp(&right.location.range.start.character)
                    })
            });
            symbols
                .dedup_by(|left, right| left.name == right.name && left.location == right.location);
            Ok::<_, String>(symbols)
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        match result {
            Ok(symbols) => Ok(Some(symbols)),
            Err(error) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("workspace symbols failed: {error}"),
                    )
                    .await;
                Err(tower_lsp::jsonrpc::Error::internal_error())
            }
        }
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
        let analysis_revisions = Arc::clone(&self.analysis_revisions);
        let analysis_uri = uri.clone();
        let analyzed = tokio::task::spawn_blocking(move || {
            let cancelled = || !analysis_revisions.is_current(&analysis_uri, analysis_revision);
            semantic_tokens_for_document_cancellable(
                &analysis_uri,
                &docs,
                compile_options,
                &analysis_sessions,
                &cancelled,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let mut tokens = match analyzed {
            Ok(Some(tokens)) => tokens,
            Ok(None) => return Ok(None),
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
        let analysis_revisions = Arc::clone(&self.analysis_revisions);
        let analysis_uri = uri.clone();
        let analyzed = tokio::task::spawn_blocking(move || {
            let cancelled = || !analysis_revisions.is_current(&analysis_uri, analysis_revision);
            inlay_hints_for_document_cancellable(
                &analysis_uri,
                &docs,
                params.range,
                compile_options,
                &analysis_sessions,
                &cancelled,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let hints = match analyzed {
            Ok(Some(hints)) => hints,
            Ok(None) => return Ok(None),
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
        let retriggered = params.context.as_ref().is_some_and(|context| {
            context.trigger_kind == CompletionTriggerKind::TRIGGER_FOR_INCOMPLETE_COMPLETIONS
        });
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
        let trigger_is_active = completion_trigger_is_active(&text, position);
        if retriggered && !trigger_is_active {
            return Ok(Some(CompletionResponse::List(CompletionList {
                is_incomplete: false,
                items: Vec::new(),
            })));
        }
        let compile_options = self.compile_options;
        let analysis_sessions = Arc::clone(&self.completion_sessions);
        let fallback_sessions = Arc::clone(&self.analysis_sessions);
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

        Ok(Some(if trigger_is_active && !items.is_empty() {
            CompletionResponse::List(CompletionList {
                is_incomplete: true,
                items,
            })
        } else {
            CompletionResponse::Array(items)
        }))
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
        let revisions = Arc::clone(&self.analysis_revisions);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            hover_for_document_cancellable(
                &analysis_uri,
                &docs,
                position,
                options,
                &sessions,
                &cancelled,
            )
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

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let revisions = Arc::clone(&self.analysis_revisions);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            signature_help_for_document_cancellable(
                &analysis_uri,
                &docs,
                position,
                options,
                &sessions,
                &cancelled,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let help = match result {
            Ok(help) => help,
            Err(error) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("signature help failed: {error}"),
                    )
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(help)
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let revisions = Arc::clone(&self.analysis_revisions);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            document_highlights_for_document_cancellable(
                &analysis_uri,
                &docs,
                position,
                options,
                &sessions,
                &cancelled,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let highlights = match result {
            Ok(highlights) => highlights,
            Err(error) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("document highlight failed: {error}"),
                    )
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(highlights)
    }

    async fn goto_declaration(
        &self,
        params: GotoDeclarationParams,
    ) -> Result<Option<GotoDeclarationResponse>> {
        self.goto_definition(params).await
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
        let revisions = Arc::clone(&self.analysis_revisions);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            definition_for_document_cancellable(
                &analysis_uri,
                &docs,
                position,
                options,
                &sessions,
                &cancelled,
            )
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

    async fn goto_type_definition(
        &self,
        params: GotoTypeDefinitionParams,
    ) -> Result<Option<GotoTypeDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let revisions = Arc::clone(&self.analysis_revisions);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            type_definition_for_document_cancellable(
                &analysis_uri,
                &docs,
                position,
                options,
                &sessions,
                &cancelled,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let definition = match result {
            Ok(definition) => definition,
            Err(error) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("type definition failed: {error}"),
                    )
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(definition)
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let revisions = Arc::clone(&self.analysis_revisions);
        let workspace = Arc::clone(&self.workspace);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            hierarchy_prepare_call(
                &analysis_uri,
                &docs,
                position,
                options,
                &sessions,
                &workspace,
                &cancelled,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let items = result.map_err(|error| {
            tower_lsp::jsonrpc::Error::invalid_params(format!(
                "prepare call hierarchy failed: {error}"
            ))
        })?;
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(items)
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        hierarchy_incoming_calls(&params.item, &self.workspace)
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        hierarchy_outgoing_calls(&params.item, &self.workspace)
    }

    async fn prepare_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let revisions = Arc::clone(&self.analysis_revisions);
        let workspace = Arc::clone(&self.workspace);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            hierarchy_prepare_type(
                &analysis_uri,
                &docs,
                position,
                options,
                &sessions,
                &workspace,
                &cancelled,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let items = result.map_err(|error| {
            tower_lsp::jsonrpc::Error::invalid_params(format!(
                "prepare type hierarchy failed: {error}"
            ))
        })?;
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(items)
    }

    async fn supertypes(
        &self,
        params: TypeHierarchySupertypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        hierarchy_supertypes(&params.item, &self.workspace)
    }

    async fn subtypes(
        &self,
        params: TypeHierarchySubtypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        hierarchy_subtypes(&params.item, &self.workspace)
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
        let revisions = Arc::clone(&self.analysis_revisions);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            implementation_for_document_cancellable(
                &analysis_uri,
                &docs,
                position,
                options,
                &sessions,
                &cancelled,
            )
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

    async fn references(
        &self,
        params: ReferenceParams,
    ) -> Result<Option<Vec<lsp_types::Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let revisions = Arc::clone(&self.analysis_revisions);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            references_for_document_cancellable(
                &analysis_uri,
                &docs,
                position,
                include_declaration,
                options,
                &sessions,
                &cancelled,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let references = match result {
            Ok(references) => references,
            Err(error) => {
                self.client
                    .log_message(MessageType::ERROR, format!("references failed: {error}"))
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(references)
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let position = params.position;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let revisions = Arc::clone(&self.analysis_revisions);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            prepare_rename_for_document_cancellable(
                &analysis_uri,
                &docs,
                position,
                options,
                &sessions,
                &cancelled,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("prepare rename failed: {error}"),
                    )
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(prepared)
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        if let Err(error) = validate_identifier(&params.new_name) {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(error));
        }
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name;
        let Some((docs, text, revision)) = self.analysis_snapshot(&uri) else {
            return Ok(None);
        };
        let analysis_uri = uri.clone();
        let options = self.compile_options;
        let sessions = Arc::clone(&self.analysis_sessions);
        let revisions = Arc::clone(&self.analysis_revisions);
        let result = tokio::task::spawn_blocking(move || {
            let cancelled = || !revisions.is_current(&analysis_uri, revision);
            rename_for_document_cancellable(
                &analysis_uri,
                &docs,
                position,
                &new_name,
                options,
                &sessions,
                &cancelled,
            )
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        let edit = match result {
            Ok(edit) => edit,
            Err(error) => {
                self.client
                    .log_message(MessageType::ERROR, format!("rename failed: {error}"))
                    .await;
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        if !self.analysis_is_current(&uri, &text, revision) {
            return Ok(None);
        }
        Ok(edit)
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
        let mut invalidated = params
            .changes
            .iter()
            .filter_map(|change| change.uri.to_file_path().ok())
            .flat_map(|path| self.workspace.invalidate_path(&path))
            .collect::<std::collections::BTreeSet<_>>();
        invalidated.extend(
            params
                .changes
                .iter()
                .filter_map(|change| change.uri.to_file_path().ok())
                .filter_map(|path| clue::find_project_root(&path)),
        );
        {
            let docs = self.docs.lock().unwrap();
            for change in &params.changes {
                bump_related_revisions(&self.analysis_revisions, &docs, &change.uri);
            }
        }
        if reset_all {
            *self.diagnostic_sessions.lock().unwrap() =
                DiagnosticSessions::new(Arc::clone(&self.analysis_sessions));
            self.completion_sessions.clear_projects();
            self.analysis_sessions.clear_projects();
            if let Err(error) = self.workspace.set_roots(self.workspace.roots()) {
                self.client
                    .log_message(
                        MessageType::WARNING,
                        format!("failed to refresh workspace projects: {error}"),
                    )
                    .await;
            }
        } else {
            for change in &params.changes {
                self.diagnostic_sessions
                    .lock()
                    .unwrap()
                    .invalidate_project(&change.uri);
                self.completion_sessions.invalidate_project(&change.uri);
                self.analysis_sessions.invalidate_project(&change.uri);
            }
            self.completion_sessions.invalidate_roots(&invalidated);
            self.analysis_sessions.invalidate_roots(&invalidated);
        }
        self.schedule_diagnostics();
        if reset_all {
            self.schedule_workspace_indexing();
        } else {
            self.schedule_project_indexing(invalidated.into_iter().collect());
        }
    }

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        self.workspace.remove_roots(
            params
                .event
                .removed
                .iter()
                .filter_map(|folder| folder.uri.to_file_path().ok()),
        );
        if let Err(error) = self.workspace.add_roots(
            params
                .event
                .added
                .iter()
                .filter_map(|folder| folder.uri.to_file_path().ok()),
        ) {
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("failed to discover workspace projects: {error}"),
                )
                .await;
        }
        self.schedule_workspace_indexing();
    }
}

fn is_manifest(uri: &lsp_types::Url) -> bool {
    uri.path_segments()
        .and_then(|mut segments| segments.next_back())
        == Some("Clue.toml")
}

#[must_use]
pub fn documents_for_uri<S: BuildHasher>(
    docs: &HashMap<lsp_types::Url, Document, S>,
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
        let analysis_sessions = Arc::new(AnalysisSessions::default());
        Self {
            client,
            docs: Arc::new(Mutex::new(HashMap::new())),
            published: Arc::new(Mutex::new(HashMap::new())),
            publish_gate: Arc::new(tokio::sync::Mutex::new(())),
            diagnostic_revision: Arc::new(AtomicU64::new(0)),
            diagnostic_sessions: Arc::new(Mutex::new(DiagnosticSessions::new(Arc::clone(
                &analysis_sessions,
            )))),
            completion_sessions: Arc::new(AnalysisSessions::default()),
            analysis_sessions,
            analysis_revisions: Arc::new(RequestRevisions::default()),
            completion_revisions: Arc::new(RequestRevisions::default()),
            semantic_tokens: Arc::new(Mutex::new(HashMap::new())),
            semantic_token_revision: Arc::new(AtomicU64::new(1)),
            supports_watched_files: AtomicBool::new(false),
            supports_type_hierarchy: AtomicBool::new(false),
            workspace: Arc::new(WorkspaceState::default()),
            compile_options,
            completion_delay,
        }
    }

    fn analysis_snapshot(
        &self,
        uri: &lsp_types::Url,
    ) -> Option<(HashMap<lsp_types::Url, Document>, String, u64)> {
        let (docs, text) = {
            let all_docs = self.docs.lock().unwrap();
            let text = all_docs.get(uri)?.text.clone();
            let docs = documents_for_uri(&all_docs, uri);
            drop(all_docs);
            (docs, text)
        };
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
        let analysis_sessions = Arc::clone(&self.analysis_sessions);
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
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                if let Ok(published) = result {
                    drop(sessions);
                    Ok(published)
                } else {
                    *sessions = DiagnosticSessions::new(analysis_sessions);
                    drop(sessions);
                    Err(())
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

    fn schedule_workspace_indexing(&self) {
        self.schedule_project_indexing(self.workspace.projects());
    }

    fn schedule_document_indexing(&self, uri: &lsp_types::Url) {
        let Some(project) = uri
            .to_file_path()
            .ok()
            .and_then(|path| clue::find_project_root(&path))
        else {
            return;
        };
        self.schedule_project_indexing(vec![project]);
    }

    fn schedule_project_indexing(&self, mut projects: Vec<PathBuf>) {
        projects.sort();
        projects.dedup();
        if projects.is_empty() {
            return;
        }
        let overlays = Arc::new(
            self.docs
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(uri, document)| {
                    uri.to_file_path()
                        .ok()
                        .map(|path| (path, document.text.clone()))
                })
                .collect::<HashMap<_, _>>(),
        );
        for project in projects {
            let token = self.workspace.begin_rebuild(&project);
            let workspace = Arc::clone(&self.workspace);
            let sessions = Arc::clone(&self.analysis_sessions);
            let overlays = Arc::clone(&overlays);
            let client = self.client.clone();
            let options = self.compile_options;
            tokio::spawn(async move {
                tokio::time::sleep(INDEX_DEBOUNCE).await;
                if !workspace.is_current(&token) {
                    return;
                }
                let workspace_for_build = Arc::clone(&workspace);
                let token_for_build = token.clone();
                let result = tokio::task::spawn_blocking(move || {
                    project_index_for_root_cancellable(
                        &project,
                        overlays.as_ref(),
                        options,
                        sessions.as_ref(),
                        &|| !workspace_for_build.is_current(&token_for_build),
                    )
                })
                .await;
                match result {
                    Ok(Ok(Some(index))) => {
                        workspace.install(token, index);
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        client
                            .log_message(
                                MessageType::WARNING,
                                format!("failed to index workspace project: {error}"),
                            )
                            .await;
                    }
                    Err(error) => {
                        client
                            .log_message(
                                MessageType::ERROR,
                                format!("workspace indexing task failed: {error}"),
                            )
                            .await;
                    }
                }
            });
        }
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
