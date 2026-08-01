mod analysis;
mod cli;
mod code_actions;
mod completion;
mod diagnostics;
mod inlay_hints;
mod navigation;
mod semantic_tokens;
mod server;
mod session;
mod text;

pub use cli::{Options, parse_args};

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    pub use crate::completion::{completion_items_for_document, completion_items_for_source};
    pub use crate::diagnostics::{
        DiagnosticSessions, PublishedDiagnostics, collect_diagnostics,
        collect_document_diagnostics, collect_workspace_diagnostics,
        collect_workspace_diagnostics_cancellable, collect_workspace_diagnostics_with_sessions,
        to_lsp, to_lsp_mapped,
    };
    pub use crate::inlay_hints::{inlay_hints_for_document, inlay_hints_for_source};
    pub use crate::navigation::{
        definition_for_document, definition_for_source, hover_for_document, hover_for_source,
        implementation_for_source, prepare_rename_for_document, prepare_rename_for_source,
        references_for_document, references_for_source, rename_for_document, rename_for_source,
    };
    pub use crate::semantic_tokens::{
        MOD_DECLARATION, MOD_DEFAULT_LIBRARY, MOD_MUTABLE, MOD_STATIC, TOKEN_COMMENT, TOKEN_ENUM,
        TOKEN_FUNCTION, TOKEN_INTERFACE, TOKEN_KEYWORD, TOKEN_MACRO, TOKEN_METHOD, TOKEN_PARAMETER,
        TOKEN_STRING, TOKEN_STRUCT, TOKEN_TYPE, TOKEN_VARIABLE, semantic_token_delta,
        semantic_tokens_for_document, semantic_tokens_for_source,
        semantic_tokens_for_source_with_options,
    };
    pub use crate::server::{Document, RequestRevisions, documents_for_uri};
    pub use crate::session::AnalysisSessions;
    pub use crate::text::apply_content_changes;
}

use server::Backend;
use tower_lsp::{LspService, Server};

pub async fn serve(options: Options) {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| {
        Backend::new(client, options.compile_options, options.completion_delay)
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}
