use std::path::{Path, PathBuf};

use ast::Stmt;
use lsp_types::{DocumentLink, Url};

use crate::{imports::parse_root, text::LineIndex};

#[cfg(feature = "test")]
#[must_use]
pub fn document_links_for_source(source: &str, module_dir: Option<&Path>) -> Vec<DocumentLink> {
    document_links(source, module_dir)
}

/// File links for a document snapshot; untitled buffers have no module
/// directory and get no links.
pub fn document_links_for_text(source: &str, module_dir: Option<&Path>) -> Vec<DocumentLink> {
    document_links(source, module_dir)
}

/// Links every file-based `mod foo;` of the top level to the module file the
/// compiler would load (`foo.rid` or `foo/mod.rid` next to this file).
/// Modules that do not resolve to an existing file are skipped so the editor
/// does not offer dead links.
fn document_links(source: &str, module_dir: Option<&Path>) -> Vec<DocumentLink> {
    let Some(root) = parse_root(source) else {
        return Vec::new();
    };
    let Some(module_dir) = module_dir else {
        return Vec::new();
    };
    let line_index = LineIndex::new(source);
    let mut links = Vec::new();
    for stmt in root.stmts() {
        let Stmt::ModDecl(decl) = stmt else {
            continue;
        };
        // Inline `mod foo { … }` blocks live in this file; only the file-based
        // form loads another module.
        if decl.items().is_some() {
            continue;
        }
        let Some(name) = decl.name() else {
            continue;
        };
        let Some(target) = module_file(module_dir, name.text()) else {
            continue;
        };
        let Ok(url) = Url::from_file_path(&target) else {
            continue;
        };
        let Some(range) = line_index.range(source, name.text_range()) else {
            continue;
        };
        links.push(DocumentLink {
            range,
            target: Some(url),
            tooltip: None,
            data: None,
        });
    }
    links
}

/// Mirrors the compiler's module file lookup: `name.rid` wins over
/// `name/mod.rid`; an ambiguous pair resolves to nothing.
fn module_file(module_dir: &Path, name: &str) -> Option<PathBuf> {
    let flat = module_dir.join(format!("{name}.rid"));
    let nested = module_dir.join(name).join("mod.rid");
    match (flat.is_file(), nested.is_file()) {
        (true, false) => Some(flat),
        (false, true) => Some(nested),
        _ => None,
    }
}
