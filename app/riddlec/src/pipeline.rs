use ast::{self, support::AstNode};
use frontend::incremental::IncrementalParser;
use hir::lower_root;
use scope_graph::{builder::build_scope_graph, resolve::resolve_hir};
use type_checker::{TypeCheckResult, check_hir};

pub struct CompileResult {
    pub type_result: TypeCheckResult,
    pub move_diagnostics: Vec<type_checker::Diagnostic>,
    pub parse_errors: Vec<String>,
}

/// Run the full frontend pipeline on `source`.
pub fn compile(source: &str) -> CompileResult {
    // 1. Parse
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);

    let parse_errors: Vec<String> = parse.errors.iter().map(|e| e.to_string()).collect();

    if !parse_errors.is_empty() {
        return CompileResult {
            type_result: TypeCheckResult::default(),
            move_diagnostics: Vec::new(),
            parse_errors,
        };
    }

    // 2. Lower AST → HIR
    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax.clone()).unwrap();
    let mut hir = lower_root(root);

    // 3. Build scope graph + resolve names
    let sg = build_scope_graph(&hir, &syntax);
    resolve_hir(&mut hir, &sg);

    // 4. Type check
    let type_result = check_hir(&hir);

    // 5. Move check (separate pass)
    let move_result = move_checker::check_moves(&hir, &type_result);

    CompileResult {
        type_result,
        move_diagnostics: move_result.diagnostics,
        parse_errors,
    }
}

impl CompileResult {
    pub fn success(&self) -> bool {
        self.parse_errors.is_empty()
            && self.type_result.diagnostics.is_empty()
            && self.move_diagnostics.is_empty()
    }
}
