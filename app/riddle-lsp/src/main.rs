use std::collections::HashMap;
use std::sync::Mutex;

use lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, InitializeParams, InitializeResult,
    InitializedParams, Location, MessageType, NumberOrString, Position, PositionEncodingKind,
    Range, ServerCapabilities, ServerInfo, TextDocumentContentChangeEvent,
    TextDocumentSyncCapability, TextDocumentSyncKind,
};
use riddlec::pipeline::{CompileResult, DiagnosticExt, IntoDiagnosticExt};
use rowan::TextRange;
use tower_lsp::jsonrpc::Result;
use tower_lsp::{Client, LanguageServer, LspService, Server};

#[derive(Debug)]
struct Backend {
    client: Client,
    docs: Mutex<HashMap<lsp_types::Url, Document>>,
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
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "riddle-lsp".into(),
                version: Some(env!("GIT_HASH").into()),
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
        self.docs.lock().unwrap().insert(uri.clone(), doc.clone());
        self.publish(uri, doc).await;
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
        self.docs.lock().unwrap().insert(uri.clone(), doc.clone());
        self.publish(uri, doc).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.lock().unwrap().remove(&uri);
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }
}

impl Backend {
    async fn publish(&self, uri: lsp_types::Url, doc: Document) {
        let result = riddlec::pipeline::compile(&doc.text);
        let diagnostics = collect_diagnostics(&uri, &doc.text, &result);
        self.client
            .publish_diagnostics(uri, diagnostics, doc.version)
            .await;
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

    for diagnostic in &result.parse_errors {
        out.push(to_lsp(uri, source, diagnostic.to_ext()));
    }
    for diagnostic in &result.hir_diagnostics {
        out.push(to_lsp(uri, source, diagnostic.to_ext()));
    }
    for diagnostic in &result.type_result.diagnostics {
        out.push(to_lsp(uri, source, diagnostic.to_ext()));
    }
    for diagnostic in &result.analysis_diagnostics {
        out.push(to_lsp(uri, source, diagnostic.to_ext()));
    }

    out
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
        .filter(|label| !label.message.is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend {
        client,
        docs: Mutex::new(HashMap::new()),
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}
