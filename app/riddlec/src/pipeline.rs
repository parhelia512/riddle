use std::{
    collections::{HashMap, HashSet},
    fs, io,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use ast::{self, support::AstNode};
use frontend::ParseError;
use frontend::incremental::IncrementalParser;
use frontend::syntax_kind::SyntaxNode;
use hir::lower_root;
use mir::backend::{Backend, c::CBackend};
use mir::{self, Module};
use scope_graph::{builder::build_scope_graph, resolve::resolve_hir};
use type_checker::{self, IncrementalTypeChecker, TypeCheckResult, check_hir};

const STD_PRELUDE: &str = include_str!(concat!(env!("OUT_DIR"), "/std.rid"));

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
    pub source_map: SourceMap,
}

#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    segments: Vec<SourceSegment>,
}

#[derive(Debug, Clone)]
struct SourceSegment {
    generated: Range<usize>,
    path: PathBuf,
    source: Arc<str>,
    original_start: usize,
}

pub struct MappedSource<'a> {
    pub path: &'a Path,
    pub source: &'a str,
    pub range: rowan::TextRange,
}

impl SourceMap {
    pub fn map_range(&self, range: rowan::TextRange) -> Option<MappedSource<'_>> {
        let start = usize::from(range.start());
        let end = usize::from(range.end());
        let segment = self
            .segments
            .iter()
            .find(|segment| segment.generated.contains(&start) && end <= segment.generated.end)
            .or_else(|| {
                if end == start {
                    self.segments
                        .iter()
                        .find(|segment| segment.generated.end == start)
                } else {
                    None
                }
            })?;
        let original_start = segment.original_start + start - segment.generated.start;
        let original_end = original_start + end - start;
        Some(MappedSource {
            path: &segment.path,
            source: &segment.source,
            range: rowan::TextRange::new(
                (original_start as u32).into(),
                (original_end as u32).into(),
            ),
        })
    }

    pub fn contains_file(&self, path: &Path) -> bool {
        self.segments.iter().any(|segment| segment.path == path)
    }

    pub fn extend(&mut self, mut other: SourceMap, generated_start: usize) {
        for segment in &mut other.segments {
            segment.generated.start += generated_start;
            segment.generated.end += generated_start;
        }
        self.segments.extend(other.segments);
    }

    fn push(
        &mut self,
        generated: Range<usize>,
        path: &Path,
        source: Arc<str>,
        original_start: usize,
    ) {
        if !generated.is_empty() || source.is_empty() {
            self.segments.push(SourceSegment {
                generated,
                path: path.to_path_buf(),
                source,
                original_start,
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompileOptions {
    pub use_std: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self { use_std: true }
    }
}

#[derive(Default)]
pub struct CheckSession {
    parser: IncrementalParser,
    type_checker: IncrementalTypeChecker,
}

impl CheckSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check_with_options(&mut self, source: &str, options: CompileOptions) -> CompileResult {
        run_pipeline_with_state(
            source,
            options,
            PipelineDepth::Check,
            &mut self.parser,
            Some(&mut self.type_checker),
        )
    }
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
    load_source_file_with_overlays(path, &HashMap::new())
}

pub fn load_source_file_with_overlays(
    path: impl AsRef<Path>,
    overlays: &HashMap<PathBuf, String>,
) -> io::Result<LoadedSource> {
    let mut files = Vec::new();
    let mut stack = HashSet::new();
    let overlays = overlays
        .iter()
        .filter_map(|(path, source)| {
            fs::canonicalize(path)
                .ok()
                .map(|path| (path, source.clone()))
        })
        .collect::<HashMap<_, _>>();
    let path = fs::canonicalize(path)?;
    let module_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let expanded = load_source_file_inner(&path, &module_dir, &overlays, &mut stack, &mut files)?;
    Ok(LoadedSource {
        source: expanded.source,
        files,
        source_map: expanded.source_map,
    })
}

fn load_source_file_inner(
    path: &Path,
    module_dir: &Path,
    overlays: &HashMap<PathBuf, String>,
    stack: &mut HashSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> io::Result<ExpandedSource> {
    let path = fs::canonicalize(path)?;
    if !stack.insert(path.clone()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cyclic module import involving `{}`", path.display()),
        ));
    }

    let source: Arc<str> = overlays
        .get(&path)
        .cloned()
        .map(Into::into)
        .map_or_else(|| fs::read_to_string(&path).map(Into::into), Ok)?;
    files.push(path.clone());
    let expanded = expand_external_mods(&source, &path, module_dir, overlays, stack, files)?;
    stack.remove(&path);
    Ok(expanded)
}

fn expand_external_mods(
    source: &Arc<str>,
    path: &Path,
    module_dir: &Path,
    overlays: &HashMap<PathBuf, String>,
    stack: &mut HashSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> io::Result<ExpandedSource> {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    if !parse.errors.is_empty() {
        return Ok(ExpandedSource::original(path, Arc::clone(source)));
    }

    let mut mods = Vec::new();
    collect_external_mods(&parse.syntax(), module_dir, &mut mods);
    if mods.is_empty() {
        return Ok(ExpandedSource::original(path, Arc::clone(source)));
    }

    let mut replacements = Vec::new();
    for ExternalMod { module, module_dir } in mods {
        let Some(name) = module.name().map(|token| token.text().to_string()) else {
            continue;
        };
        let child = find_module_file(&module_dir, &name)?;
        let child_dir = module_dir.join(&name);
        let child_source = load_source_file_inner(&child, &child_dir, overlays, stack, files)?;
        let range = module.syntax().text_range();
        let visibility = if module.is_pub() { "pub " } else { "" };
        replacements.push((
            usize::from(range.start()),
            usize::from(range.end()),
            format!("{visibility}mod {name} {{\n"),
            child_source,
        ));
    }

    replacements.sort_by_key(|(start, _, _, _)| *start);
    let mut out = String::with_capacity(source.len());
    let mut source_map = SourceMap::default();
    let mut cursor = 0;
    for (start, end, prefix, child) in replacements {
        append_original(&mut out, &mut source_map, path, source, cursor..start);
        out.push_str(&prefix);
        let child_start = out.len();
        out.push_str(&child.source);
        source_map.extend(child.source_map, child_start);
        out.push_str("\n}");
        cursor = end;
    }
    append_original(
        &mut out,
        &mut source_map,
        path,
        source,
        cursor..source.len(),
    );
    Ok(ExpandedSource {
        source: out,
        source_map,
    })
}

struct ExpandedSource {
    source: String,
    source_map: SourceMap,
}

impl ExpandedSource {
    fn original(path: &Path, source: Arc<str>) -> Self {
        let mut source_map = SourceMap::default();
        source_map.push(0..source.len(), path, Arc::clone(&source), 0);
        Self {
            source: source.to_string(),
            source_map,
        }
    }
}

fn append_original(
    out: &mut String,
    source_map: &mut SourceMap,
    path: &Path,
    source: &Arc<str>,
    original: Range<usize>,
) {
    let generated_start = out.len();
    out.push_str(&source[original.clone()]);
    source_map.push(
        generated_start..out.len(),
        path,
        Arc::clone(source),
        original.start,
    );
}

struct ExternalMod {
    module: ast::ModDecl,
    module_dir: PathBuf,
}

fn collect_external_mods(node: &SyntaxNode, module_dir: &Path, out: &mut Vec<ExternalMod>) {
    for child in node.children() {
        if let Some(module) = ast::ModDecl::cast(child.clone()) {
            if module.items().is_none() {
                out.push(ExternalMod {
                    module,
                    module_dir: module_dir.to_path_buf(),
                });
                continue;
            }
            let Some(name) = module.name().map(|token| token.text().to_string()) else {
                collect_external_mods(&child, module_dir, out);
                continue;
            };
            collect_external_mods(&child, &module_dir.join(name), out);
            continue;
        }
        collect_external_mods(&child, module_dir, out);
    }
}

fn find_module_file(module_dir: &Path, name: &str) -> io::Result<PathBuf> {
    let flat = module_dir.join(format!("{name}.rid"));
    let nested = module_dir.join(name).join("mod.rid");
    match (flat.is_file(), nested.is_file()) {
        (true, false) => Ok(flat),
        (false, true) => Ok(nested),
        (true, true) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "module `{name}` is ambiguous; both `{}` and `{}` exist",
                flat.display(),
                nested.display()
            ),
        )),
        (false, false) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "module `{name}` not found; expected `{}` or `{}`",
                flat.display(),
                nested.display()
            ),
        )),
    }
}

pub fn generate_c(module: &Module) -> Result<String, String> {
    let mut backend = CBackend::new();
    backend.compile(module)
}

/// Run the full frontend pipeline on `source`.
pub fn compile(source: &str) -> CompileResult {
    compile_with_options(source, CompileOptions::default())
}

pub fn compile_with_options(source: &str, options: CompileOptions) -> CompileResult {
    run_pipeline(source, options, PipelineDepth::Build)
}

pub fn check_with_options(source: &str, options: CompileOptions) -> CompileResult {
    run_pipeline(source, options, PipelineDepth::Check)
}

pub fn resolve_with_options(source: &str, options: CompileOptions) -> CompileResult {
    run_pipeline(source, options, PipelineDepth::Resolve)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PipelineDepth {
    Resolve,
    Check,
    Build,
}

fn run_pipeline(source: &str, options: CompileOptions, depth: PipelineDepth) -> CompileResult {
    let mut parser = IncrementalParser::new();
    run_pipeline_with_state(source, options, depth, &mut parser, None)
}

fn run_pipeline_with_state(
    source: &str,
    options: CompileOptions,
    depth: PipelineDepth,
    parser: &mut IncrementalParser,
    incremental_type_checker: Option<&mut IncrementalTypeChecker>,
) -> CompileResult {
    let user_source = source;
    let owned_source = options
        .use_std
        .then(|| format!("{source}\n\n{STD_PRELUDE}"));
    let source = owned_source.as_deref().unwrap_or(source);

    // 1. Parse
    let parse = update_parse(parser, source);

    let mut parse_errors = parse.errors.clone();
    if options.use_std
        && parse_errors
            .iter()
            .any(|error| usize::from(error.span.end()) > user_source.len())
    {
        let mut parser = IncrementalParser::new();
        let user_errors = parser.set_source(user_source).errors.clone();
        if !user_errors.is_empty() {
            parse_errors = user_errors;
        }
    }

    if !parse_errors.is_empty() {
        return parse_failure(parse_errors);
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
        .map(convert_hir_diag)
        .collect();

    if depth == PipelineDepth::Resolve {
        return CompileResult {
            hir: Some(hir),
            type_result: TypeCheckResult::default(),
            hir_diagnostics,
            analysis_diagnostics: Vec::new(),
            analysis: move_checker::AnalysisResult::default(),
            mir_module: None,
            parse_errors,
        };
    }

    // 4. Type check
    let type_result = incremental_type_checker
        .map(|checker| checker.check_with_syntax(&hir, &syntax).result)
        .unwrap_or_else(|| check_hir(&hir));

    // 5. Escape analysis (determines which locals need heap allocation)
    let escape_result = escape_analysis::analyze_escapes(&hir, &type_result);

    // 6. Move and borrow checking is independent of storage placement.
    let analysis = move_checker::analyze(&hir, &type_result);
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
    let mir_module = (success && depth == PipelineDepth::Build)
        .then(|| mir::lower_hir(&hir, &type_result, &escape_result));

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

fn update_parse<'a>(
    parser: &'a mut IncrementalParser,
    source: &str,
) -> &'a frontend::tree_builder::Parse {
    if parser.current_parse().is_none() {
        return parser.set_source(source);
    }
    if parser.source() == source {
        return parser.current_parse().expect("parse was initialized");
    }

    let (offset, delete_len, insert) = replacement(parser.source(), source);
    if parser.try_apply_edit(offset, delete_len, insert).is_err() {
        parser.set_source(source)
    } else {
        parser.current_parse().expect("edit updated the parse")
    }
}

fn replacement<'a>(old: &str, new: &'a str) -> (usize, usize, &'a str) {
    let mut prefix = 0;
    for (old_char, new_char) in old.chars().zip(new.chars()) {
        if old_char != new_char {
            break;
        }
        prefix += old_char.len_utf8();
    }

    let mut suffix = 0;
    for (old_char, new_char) in old[prefix..].chars().rev().zip(new[prefix..].chars().rev()) {
        if old_char != new_char {
            break;
        }
        suffix += old_char.len_utf8();
    }

    (
        prefix,
        old.len() - prefix - suffix,
        &new[prefix..new.len() - suffix],
    )
}

fn parse_failure(parse_errors: Vec<ParseError>) -> CompileResult {
    CompileResult {
        hir: None,
        type_result: TypeCheckResult::default(),
        hir_diagnostics: Vec::new(),
        analysis_diagnostics: Vec::new(),
        analysis: move_checker::AnalysisResult::default(),
        mir_module: None,
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
#[path = "../../../tests/riddlec/pipeline.rs"]
mod tests;
