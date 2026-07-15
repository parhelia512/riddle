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
use type_checker::{self, TypeCheckResult, check_hir};

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
        if !generated.is_empty() {
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
    let owned_source = options
        .use_std
        .then(|| format!("{source}\n\n{STD_PRELUDE}"));
    let source = owned_source.as_deref().unwrap_or(source);

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
        .map(convert_hir_diag)
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

    fn temp_source_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "riddle-load-source-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn load_source_file_expands_external_mods() {
        let root = temp_source_root("external-mods");
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

    #[test]
    fn source_map_points_into_external_module() {
        let root = temp_source_root("source-map");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("main.rid"),
            "mod util;\nfun main() -> i32 { util::value() }\n",
        )
        .unwrap();
        fs::write(
            root.join("util.rid"),
            "pub fun value() -> i32 { missing }\n",
        )
        .unwrap();

        let loaded = load_source_file(root.join("main.rid")).unwrap();
        let start = loaded.source.find("missing").unwrap();
        let mapped = loaded
            .source_map
            .map_range(rowan::TextRange::new(
                (start as u32).into(),
                ((start + "missing".len()) as u32).into(),
            ))
            .unwrap();

        assert_eq!(
            mapped.path,
            fs::canonicalize(root.join("util.rid")).unwrap()
        );
        assert_eq!(
            &mapped.source[usize::from(mapped.range.start())..usize::from(mapped.range.end())],
            "missing"
        );
        let generated_eof =
            loaded.source.find("pub fun").unwrap() + "pub fun value() -> i32 { missing }\n".len();
        let mapped_eof = loaded
            .source_map
            .map_range(rowan::TextRange::empty((generated_eof as u32).into()))
            .unwrap();
        assert_eq!(mapped_eof.path, mapped.path);
        assert_eq!(usize::from(mapped_eof.range.start()), mapped.source.len());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn source_loader_uses_in_memory_overlays() {
        let root = temp_source_root("source-overlay");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("main.rid"), "mod util;\n").unwrap();
        fs::write(root.join("util.rid"), "pub fun value() -> i32 { 1 }\n").unwrap();
        let mut overlays = HashMap::new();
        overlays.insert(
            root.join("util.rid"),
            "pub fun value() -> i32 { 2 }\n".into(),
        );

        let loaded = load_source_file_with_overlays(root.join("main.rid"), &overlays).unwrap();

        assert!(loaded.source.contains("value() -> i32 { 2 }"));
        assert!(!loaded.source.contains("value() -> i32 { 1 }"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_source_file_uses_rust_style_mod_rid_tree() {
        let root = temp_source_root("mod-rid-tree");
        fs::create_dir_all(root.join("foo")).unwrap();
        fs::write(
            root.join("main.rid"),
            "mod foo;\nfun main() -> i32 { foo::value() }\n",
        )
        .unwrap();
        fs::write(
            root.join("foo").join("mod.rid"),
            "mod bar;\npub fun value() -> i32 { bar::value() }\n",
        )
        .unwrap();
        fs::write(
            root.join("foo").join("bar.rid"),
            "pub fun value() -> i32 { 1 }\n",
        )
        .unwrap();

        let loaded = load_source_file(root.join("main.rid")).unwrap();
        assert!(
            loaded
                .files
                .contains(&fs::canonicalize(root.join("foo").join("mod.rid")).unwrap())
        );
        assert!(
            loaded
                .files
                .contains(&fs::canonicalize(root.join("foo").join("bar.rid")).unwrap())
        );
        assert!(compile(&loaded.source).success());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn flat_modules_resolve_children_from_module_directory() {
        let root = temp_source_root("flat-module-children");
        fs::create_dir_all(root.join("foo")).unwrap();
        fs::write(
            root.join("main.rid"),
            "mod foo;\nfun main() -> i32 { foo::value() }\n",
        )
        .unwrap();
        fs::write(
            root.join("foo.rid"),
            "mod bar;\npub fun value() -> i32 { bar::value() }\n",
        )
        .unwrap();
        fs::write(
            root.join("foo").join("bar.rid"),
            "pub fun value() -> i32 { 1 }\n",
        )
        .unwrap();
        fs::write(root.join("bar.rid"), "pub fun value() -> i32 { 99 }\n").unwrap();

        let loaded = load_source_file(root.join("main.rid")).unwrap();
        assert!(loaded.source.contains("pub fun value() -> i32 { 1 }"));
        assert!(!loaded.source.contains("pub fun value() -> i32 { 99 }"));
        assert!(compile(&loaded.source).success());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn inline_modules_resolve_children_from_module_directory() {
        let root = temp_source_root("inline-module-children");
        fs::create_dir_all(root.join("foo")).unwrap();
        fs::write(
            root.join("main.rid"),
            "mod foo { mod bar; pub fun value() -> i32 { bar::value() } }\nfun main() -> i32 { foo::value() }\n",
        )
        .unwrap();
        fs::write(
            root.join("foo").join("bar.rid"),
            "pub fun value() -> i32 { 1 }\n",
        )
        .unwrap();

        let loaded = load_source_file(root.join("main.rid")).unwrap();
        assert!(
            loaded
                .files
                .contains(&fs::canonicalize(root.join("foo").join("bar.rid")).unwrap())
        );
        assert!(compile(&loaded.source).success());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn duplicate_flat_and_mod_rid_modules_are_rejected() {
        let root = temp_source_root("duplicate-module-files");
        fs::create_dir_all(root.join("foo")).unwrap();
        fs::write(root.join("main.rid"), "mod foo;\n").unwrap();
        fs::write(root.join("foo.rid"), "pub fun value() -> i32 { 1 }\n").unwrap();
        fs::write(
            root.join("foo").join("mod.rid"),
            "pub fun value() -> i32 { 2 }\n",
        )
        .unwrap();

        let error = load_source_file(root.join("main.rid")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("ambiguous"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn undeclared_directory_modules_are_not_loaded() {
        let root = temp_source_root("undeclared-module");
        fs::create_dir_all(root.join("foo")).unwrap();
        fs::write(root.join("main.rid"), "fun main() -> i32 { 0 }\n").unwrap();
        fs::write(root.join("foo").join("mod.rid"), "this is not parsed\n").unwrap();

        let loaded = load_source_file(root.join("main.rid")).unwrap();
        assert_eq!(
            loaded.files,
            vec![fs::canonicalize(root.join("main.rid")).unwrap()]
        );
        assert!(compile(&loaded.source).success());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn std_range_iterator_type_checks() {
        let result = compile(
            r#"
            fun main() {
                let mut iter = range(0, 3);
                let first = iter.next();
            }
            "#,
        );

        assert!(result.success(), "{:#?}", result.type_result.diagnostics);
    }

    #[test]
    fn std_clone_and_comparison_methods_are_callable() {
        let result = compile(
            r#"
            fun main() -> i32 {
                let value: i32 = 7;
                let cloned = value.clone();
                let equal = value.eq(&7);
                let ordering = value.cmp(&cloned);
                let partial = value.partial_cmp(&cloned);
                if equal { cloned } else { 0 }
            }
            "#,
        );

        assert!(
            result.success(),
            "hir: {:#?}\ntype: {:#?}\nanalysis: {:#?}",
            result.hir_diagnostics,
            result.type_result.diagnostics,
            result.analysis_diagnostics
        );
        let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
        assert!(c.contains("clone__i32"), "{c}");
        assert!(c.contains("cmp__i32"), "{c}");
        assert!(c.contains("ref_tmp"), "{c}");
        assert!(!c.contains("&((int32_t)7)"), "{c}");
    }

    #[test]
    fn std_operator_trait_methods_are_callable() {
        let result = compile(
            r#"
            fun main() -> i32 {
                let value: i32 = 12;
                let reduced = value.sub(2).mul(3).div(2).rem(10);
                let bits = reduced.bitand(7).bitor(8).bitxor(1);
                let shifted = bits.shl(1).shr(1);
                let mut total = shifted;
                total.add_assign(2);
                total.sub_assign(1);
                total.mul_assign(2);
                total.div_assign(2);
                total.rem_assign(10);
                total
            }
            "#,
        );

        assert!(
            result.success(),
            "hir: {:#?}\ntype: {:#?}\nanalysis: {:#?}",
            result.hir_diagnostics,
            result.type_result.diagnostics,
            result.analysis_diagnostics
        );
        let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
        assert!(c.contains("sub__i32"), "{c}");
        assert!(c.contains("add_assign__i32"), "{c}");
    }

    #[test]
    fn compile_can_skip_std() {
        let result = compile_with_options(
            r#"
            fun main() {
                let value = range(0, 3);
            }
            "#,
            CompileOptions { use_std: false },
        );

        assert!(!result.success());
        assert!(
            result
                .hir_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("unresolved name: `range`")),
            "{:#?}",
            result.hir_diagnostics
        );
    }

    #[test]
    fn compile_without_std_accepts_basic_program() {
        let result = compile_with_options(
            r#"
            fun main() {
                let value = 1;
            }
            "#,
            CompileOptions { use_std: false },
        );

        assert!(result.success(), "{:#?}", result.type_result.diagnostics);
    }

    #[test]
    fn std_prelude_reexports_core_items() {
        let result = compile(
            r#"
            fun main() {
                let value: Option<i32> = Option::Some(1);
                let mut iter = range(0, 3);
                let first = iter.next();
            }
            "#,
        );

        assert!(result.success(), "{:#?}", result.type_result.diagnostics);
    }

    #[test]
    fn std_option_and_result_copy_depends_on_payloads() {
        let copy = compile(
            r#"
            fun main() {
                let option: Option<i32> = Some(1);
                let first_option = option;
                let second_option = option;
                let result: Result<i32, bool> = Ok(1);
                let first_result = result;
                let second_result = result;
            }
            "#,
        );
        assert!(copy.success(), "{:#?}", copy.analysis_diagnostics);
        assert!(
            !generate_c(copy.mir_module.as_ref().unwrap())
                .unwrap()
                .is_empty()
        );

        let moved = compile(
            r#"
            struct Token { value: i32 }

            fun main() {
                let option: Option<Token> = Option::Some(Token { value: 1 });
                let first = option;
                let second = option;
            }
            "#,
        );
        assert!(
            moved
                .analysis_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("use of moved value: `option`")),
            "{:#?}",
            moved.analysis_diagnostics
        );
    }

    #[test]
    fn copy_impl_requires_every_payload_to_be_copy() {
        let invalid = compile(
            r#"
            struct Token { value: i32 }
            struct Wrapper<T> { value: T }
            enum TokenState { Empty, Full(Token) }

            impl<T> Copy for Wrapper<T> {}
            impl Copy for TokenState {}

            fun main() {}
            "#,
        );

        let copy_errors = invalid
            .type_result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "E0041")
            .collect::<Vec<_>>();
        assert_eq!(
            copy_errors.len(),
            2,
            "{:#?}",
            invalid.type_result.diagnostics
        );
        assert!(
            copy_errors
                .iter()
                .any(|diagnostic| diagnostic.message.contains("Wrapper<T>"))
        );
        assert!(
            copy_errors
                .iter()
                .any(|diagnostic| diagnostic.message.contains("TokenState"))
        );
    }

    #[test]
    fn copy_impl_accepts_nested_conditional_copy_fields() {
        let result = compile(
            r#"
            struct Nested<T> { value: Option<T> }

            impl<T: Copy> Copy for Nested<T> {}

            fun main() {
                let value: Nested<i32> = Nested { value: Some(1) };
                let first = value;
                let second = value;
            }
            "#,
        );

        assert!(
            result.success(),
            "type: {:#?}\nanalysis: {:#?}",
            result.type_result.diagnostics,
            result.analysis_diagnostics
        );
    }

    #[test]
    fn enum_match_lowers_variants_guards_bindings_and_values() {
        let result = compile(
            r#"
            enum Message {
                Quit,
                Number(i32),
                Pair { left: i32, right: i32 },
            }

            fun select(value: Message) -> i32 {
                match value {
                    Message::Quit => 0,
                    Message::Number(number) if number > 10 => number,
                    Message::Number(number) => number + 1,
                    Message::Pair { left, right: other } => left + other,
                }
            }

            fun main() -> i32 {
                let pair = Message::Pair { right: 22, left: 20 };
                select(pair)
            }
            "#,
        );

        assert!(
            result.success(),
            "hir: {:#?}\ntype: {:#?}\nanalysis: {:#?}",
            result.hir_diagnostics,
            result.type_result.diagnostics,
            result.analysis_diagnostics
        );
        let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
        assert!(c.contains("Number_0;"), "{c}");
        assert!(c.contains(".Pair_left"), "{c}");
        assert!(c.contains("if ("), "{c}");
        assert!(c.contains("self->start < self->end"), "{c}");
    }

    #[test]
    fn enum_constructor_uses_the_flattened_payload_offset() {
        let result = compile(
            r#"
            enum Value {
                First(i32),
                Second(i32),
            }

            fun main() -> i32 {
                match Value::Second(7) {
                    Value::First(value) => value,
                    Value::Second(value) => value,
                }
            }
            "#,
        );

        assert!(result.success(), "{:#?}", result.type_result.diagnostics);
        let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
        assert!(c.contains(".Second_0 ="), "{c}");
        assert!(!c.contains(".First_0 = ((int32_t)7)"), "{c}");
    }

    #[test]
    fn literal_match_preserves_values_and_string_comparison() {
        let result = compile(
            r#"
            fun classify(value: i32) -> i32 {
                match value {
                    0 => 10,
                    1 => 20,
                    other => other,
                }
            }

            fun is_yes(value: &str) -> bool {
                match value {
                    "yes" => true,
                    _ => false,
                }
            }

            fun main() -> i32 {
                classify(1)
            }
            "#,
        );

        assert!(result.success(), "{:#?}", result.type_result.diagnostics);
        let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
        assert!(c.contains("memcmp"), "{c}");
        assert!(c.contains("== ((int32_t)0)"), "{c}");
    }

    #[test]
    fn non_exhaustive_enum_match_is_rejected() {
        let result = compile(
            r#"
            enum State { Ready, Done }

            fun main() -> i32 {
                match State::Ready {
                    State::Ready => 1,
                }
            }
            "#,
        );

        assert!(!result.success());
        assert!(
            result
                .type_result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0039"),
            "{:#?}",
            result.type_result.diagnostics
        );
    }

    #[test]
    fn unit_return_in_match_arm_remains_a_return() {
        let result = compile(
            r#"
            enum State { Ready, Done }

            fun consume(state: State) {
                match state {
                    State::Ready => { return; },
                    State::Done => {},
                }
            }

            fun main() {
                consume(State::Ready);
            }
            "#,
        );

        assert!(result.success(), "{:#?}", result.type_result.diagnostics);
        let module = result.mir_module.as_ref().unwrap();
        let consume = module
            .function_order
            .iter()
            .map(|id| &module.functions[*id])
            .find(|function| function.name == "consume")
            .unwrap();
        assert!(consume.blocks.iter().any(|(_, block)| {
            block.label.as_deref() == Some("match_arm")
                && matches!(block.terminator, mir::instr::Terminator::Return(None))
        }));
    }

    #[test]
    fn std_modules_expose_core_items() {
        let result = compile(
            r#"
            fun main() {
                let value = std::option::Option::Some(1);
                let mut iter: Range = std::ops::range(0, 3);
                let first = iter.next();
            }
            "#,
        );

        assert!(result.success(), "{:#?}", result.type_result.diagnostics);
    }

    #[test]
    fn std_array_into_iterator_accepts_non_copy_items() {
        let result = compile(
            r#"
            struct Token {
                value: i32,
            }

            fun main() {
                let values = [Token { value: 1 }, Token { value: 2 }];
                let mut iter = values.into_iter();
                let first = iter.next();

                for item in [Token { value: 3 }, Token { value: 4 }] {
                    let next = item.value + 1;
                }
            }
            "#,
        );

        assert!(
            result.success(),
            "type: {:#?}\nanalysis: {:#?}",
            result.type_result.diagnostics,
            result.analysis_diagnostics
        );
    }

    #[test]
    fn std_range_for_loop_lowers_to_mir_loop() {
        let result = compile(
            r#"
            fun main() {
                let mut sum = 0;
                for item in range(0, 3) {
                    sum += item;
                }
            }
            "#,
        );

        assert!(result.success(), "{:#?}", result.type_result.diagnostics);
        let module = result
            .mir_module
            .expect("successful compile should lower MIR");
        let main_id = module
            .function_order
            .iter()
            .copied()
            .find(|id| module.functions[*id].name == "main")
            .expect("main function should be lowered");
        let main = &module.functions[main_id];
        let has_loop_branch = main
            .blocks
            .iter()
            .any(|(_, block)| matches!(block.terminator, mir::instr::Terminator::CondBranch(..)));
        assert!(has_loop_branch, "{main:#?}");
        assert!(
            !generate_c(&module)
                .expect("C backend should lower for loop")
                .is_empty()
        );
    }
}
