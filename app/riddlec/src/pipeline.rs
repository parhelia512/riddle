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
    let path = fs::canonicalize(path)?;
    let module_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let source = load_source_file_inner(&path, &module_dir, &mut stack, &mut files)?;
    Ok(LoadedSource { source, files })
}

fn load_source_file_inner(
    path: &Path,
    module_dir: &Path,
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
    let expanded = expand_external_mods(&source, module_dir, stack, files)?;
    stack.remove(&path);
    Ok(expanded)
}

fn expand_external_mods(
    source: &str,
    module_dir: &Path,
    stack: &mut HashSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> io::Result<String> {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    if !parse.errors.is_empty() {
        return Ok(source.to_string());
    }

    let mut mods = Vec::new();
    collect_external_mods(&parse.syntax(), module_dir, &mut mods);
    if mods.is_empty() {
        return Ok(source.to_string());
    }

    let mut replacements = Vec::new();
    for ExternalMod { module, module_dir } in mods {
        let Some(name) = module.name().map(|token| token.text().to_string()) else {
            continue;
        };
        let child = find_module_file(&module_dir, &name)?;
        let child_dir = module_dir.join(&name);
        let child_source = load_source_file_inner(&child, &child_dir, stack, files)?;
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
