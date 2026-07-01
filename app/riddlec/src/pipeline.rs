use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

use ast::{self, support::AstNode};
use frontend::ParseError;
use frontend::incremental::IncrementalParser;
use frontend::syntax_kind::SyntaxNode;
use hir::lower_root;
use mir::backend::{Backend, c::CBackend};
use mir::{self, Module};
use scope_graph::{builder::build_scope_graph, resolve::resolve_hir};
use type_checker::{self, TypeCheckResult, check_hir};

const STD_PRELUDE: &str = include_str!("../../../std/lib.rid");

pub struct CompileResult {
    pub hir: Option<hir::HirFile>,
    pub type_result: TypeCheckResult,
    pub hir_diagnostics: Vec<type_checker::Diagnostic>,
    pub analysis_diagnostics: Vec<type_checker::Diagnostic>,
    pub analysis: move_checker::AnalysisResult,
    pub mir_module: Option<Module>,
    pub parse_errors: Vec<ParseError>,
}

#[derive(Debug, Clone)]
pub struct LoadedSource {
    pub source: String,
    pub files: Vec<PathBuf>,
}

/// An adapter that lets the diagnostics printer handle diagnostics from
/// different sources (parse errors, type errors, move errors) uniformly.
/// This is also used as the bridge type in the diagnostics printer.
#[derive(Debug, Clone)]
pub struct DiagnosticExt {
    pub code: &'static str,
    pub severity: type_checker::Severity,
    pub message: String,
    pub labels: Vec<type_checker::SourceLabel>,
    pub help: Option<String>,
    pub notes: Vec<String>,
}

pub trait IntoDiagnosticExt {
    fn to_ext(&self) -> DiagnosticExt;
}

impl IntoDiagnosticExt for type_checker::Diagnostic {
    fn to_ext(&self) -> DiagnosticExt {
        DiagnosticExt {
            code: self.code,
            severity: self.severity,
            message: self.message.clone(),
            labels: self.labels.clone(),
            help: self.help.clone(),
            notes: self.notes.clone(),
        }
    }
}

impl IntoDiagnosticExt for ParseError {
    fn to_ext(&self) -> DiagnosticExt {
        DiagnosticExt {
            code: "",
            severity: type_checker::Severity::Error,
            message: self.message.clone(),
            labels: vec![type_checker::SourceLabel {
                range: self.span,
                message: String::new(),
                style: type_checker::LabelStyle::Primary,
            }],
            help: None,
            notes: Vec::new(),
        }
    }
}

pub fn load_source_file(path: impl AsRef<Path>) -> io::Result<LoadedSource> {
    let mut files = Vec::new();
    let mut stack = HashSet::new();
    let source = load_source_file_inner(path.as_ref(), &mut stack, &mut files)?;
    Ok(LoadedSource { source, files })
}

fn load_source_file_inner(
    path: &Path,
    stack: &mut HashSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> io::Result<String> {
    let path = fs::canonicalize(path)?;
    if !stack.insert(path.clone()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cyclic module import involving `{}`", path.display()),
        ));
    }

    let source = fs::read_to_string(&path)?;
    files.push(path.clone());
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let expanded = expand_external_mods(&source, dir, stack, files)?;
    stack.remove(&path);
    Ok(expanded)
}

fn expand_external_mods(
    source: &str,
    dir: &Path,
    stack: &mut HashSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> io::Result<String> {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    if !parse.errors.is_empty() {
        return Ok(source.to_string());
    }

    let mut mods = Vec::new();
    collect_external_mods(&parse.syntax(), &mut mods);
    if mods.is_empty() {
        return Ok(source.to_string());
    }

    let mut replacements = Vec::new();
    for module in mods {
        let Some(name) = module.name().map(|token| token.text().to_string()) else {
            continue;
        };
        let child = find_module_file(dir, &name)?;
        let child_source = load_source_file_inner(&child, stack, files)?;
        let range = module.syntax().text_range();
        let visibility = if module.is_pub() { "pub " } else { "" };
        replacements.push((
            usize::from(range.start()),
            usize::from(range.end()),
            format!("{visibility}mod {name} {{\n{child_source}\n}}"),
        ));
    }

    replacements.sort_by_key(|(start, _, _)| *start);
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0;
    for (start, end, replacement) in replacements {
        out.push_str(&source[cursor..start]);
        out.push_str(&replacement);
        cursor = end;
    }
    out.push_str(&source[cursor..]);
    Ok(out)
}

fn collect_external_mods(node: &SyntaxNode, out: &mut Vec<ast::ModDecl>) {
    for child in node.children() {
        if let Some(module) = ast::ModDecl::cast(child.clone()) {
            if module.items().is_none() {
                out.push(module);
                continue;
            }
        }
        collect_external_mods(&child, out);
    }
}

fn find_module_file(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let flat = dir.join(format!("{name}.rid"));
    if flat.is_file() {
        return Ok(flat);
    }

    let nested = dir.join(name).join("mod.rid");
    if nested.is_file() {
        return Ok(nested);
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "module `{name}` not found; expected `{}` or `{}`",
            flat.display(),
            nested.display()
        ),
    ))
}

pub fn generate_c(module: &Module) -> Result<String, String> {
    let mut backend = CBackend::new();
    backend.compile(module)
}

/// Run the full frontend pipeline on `source`.
pub fn compile(source: &str) -> CompileResult {
    let owned_source = format!("{source}\n\n{STD_PRELUDE}");
    let source = owned_source.as_str();

    // 1. Parse
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);

    let parse_errors = parse.errors.clone();

    if !parse_errors.is_empty() {
        return CompileResult {
            hir: None,
            type_result: TypeCheckResult::default(),
            hir_diagnostics: Vec::new(),
            analysis_diagnostics: Vec::new(),
            analysis: move_checker::AnalysisResult::default(),
            mir_module: None,
            parse_errors,
        };
    }

    // 2. Lower AST → HIR
    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax.clone()).unwrap();
    let mut hir = lower_root(root);

    // 3. Build scope graph + resolve names
    let (sg, scope_diagnostics) = build_scope_graph(&hir, &syntax);
    resolve_hir(&mut hir, &sg);

    /// Convert a `hir::body::Diagnostic` to a `type_checker::Diagnostic`.
    fn convert_hir_diag(d: &hir::body::Diagnostic) -> type_checker::Diagnostic {
        type_checker::Diagnostic {
            code: d.code,
            severity: match d.severity {
                hir::body::Severity::Error => type_checker::Severity::Error,
                hir::body::Severity::Warning => type_checker::Severity::Warning,
                hir::body::Severity::Note => type_checker::Severity::Note,
                hir::body::Severity::Help => type_checker::Severity::Help,
            },
            message: d.message.clone(),
            labels: d
                .labels
                .iter()
                .map(|l| type_checker::SourceLabel {
                    range: l.range,
                    message: l.message.clone(),
                    style: match l.style {
                        hir::body::LabelStyle::Primary => type_checker::LabelStyle::Primary,
                        hir::body::LabelStyle::Secondary => type_checker::LabelStyle::Secondary,
                    },
                })
                .collect(),
            help: d.help.clone(),
            notes: d.notes.clone(),
        }
    }

    // Collect HIR diagnostics (E0040 lowering + E0050 resolution + E0051/E0052 scope-graph builder)
    let hir_diagnostics: Vec<type_checker::Diagnostic> = hir
        .bodies
        .iter()
        .flat_map(|(_, body)| body.diagnostics.iter())
        .chain(scope_diagnostics.iter())
        .map(|d| convert_hir_diag(d))
        .collect();

    // 4. Type check
    let type_result = check_hir(&hir);

    // 5. Escape analysis (determines which locals need heap allocation)
    let escape_result = escape_analysis::analyze_escapes(&hir);

    // 6. Move check (uses escape results to skip borrow checks for heap locals)
    let analysis = move_checker::analyze(&hir, &type_result, &escape_result);
    let analysis_diagnostics = analysis.diagnostics.clone();

    // Only Error-severity diagnostics block compilation.
    // Notes (like E0200 heap promotion) and warnings are informational.
    let success = parse_errors.is_empty()
        && !hir_diagnostics
            .iter()
            .any(|d| d.severity == type_checker::Severity::Error)
        && !type_result
            .diagnostics
            .iter()
            .any(|d| d.severity == type_checker::Severity::Error)
        && !analysis_diagnostics
            .iter()
            .any(|d| d.severity == type_checker::Severity::Error);

    // 7. Lower HIR → MIR
    let mir_module = success.then(|| mir::lower_hir(&hir, &type_result, &escape_result));

    CompileResult {
        hir: Some(hir),
        type_result,
        hir_diagnostics,
        analysis_diagnostics,
        analysis,
        mir_module,
        parse_errors,
    }
}

impl CompileResult {
    pub fn success(&self) -> bool {
        self.parse_errors.is_empty()
            && !self
                .hir_diagnostics
                .iter()
                .any(|d| d.severity == type_checker::Severity::Error)
            && !self
                .type_result
                .diagnostics
                .iter()
                .any(|d| d.severity == type_checker::Severity::Error)
            && !self
                .analysis_diagnostics
                .iter()
                .any(|d| d.severity == type_checker::Severity::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn load_source_file_expands_external_mods() {
        let root = std::env::temp_dir().join(format!(
            "riddle-load-source-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("main.rid"),
            "mod util;\nfun main() -> i32 { util::one() }\n",
        )
        .unwrap();
        fs::write(root.join("util.rid"), "fun one() -> i32 { 1 }\n").unwrap();

        let loaded = load_source_file(root.join("main.rid")).unwrap();
        assert!(loaded.source.contains("mod util {"));
        assert!(loaded.source.contains("fun one() -> i32 { 1 }"));
        assert_eq!(loaded.files.len(), 2);

        let _ = fs::remove_dir_all(root);
    }
}
