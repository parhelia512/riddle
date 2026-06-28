mod backend_c;
mod builder;
mod display;
mod lower;

use ast::{self, support::AstNode};
use frontend::incremental::IncrementalParser;
use hir::{HirFile, lower_root};
use mir::Module;
use scope_graph::{builder::build_scope_graph, resolve::resolve_hir};
use type_checker::TypeCheckResult;

/// Parse source, run the full frontend pipeline, and return the lowered MIR module.
fn compile(source: &str) -> (HirFile, TypeCheckResult, move_checker::AnalysisResult, Module) {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);

    assert!(parse.errors.is_empty(), "parse errors: {:?}", parse.errors);

    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax.clone()).unwrap();
    let mut hir = lower_root(root);

    let (sg, _) = build_scope_graph(&hir, &syntax);
    resolve_hir(&mut hir, &sg);

    let type_result = type_checker::check_hir(&hir);
    let escape_result = escape_analysis::analyze_escapes(&hir);
    let analysis = move_checker::analyze(&hir, &type_result, &escape_result);

    let mir_module = mir::lower_hir(&hir, &type_result, &escape_result);

    (hir, type_result, analysis, mir_module)
}

/// Parse and run the full pipeline, returning just the MIR module.
fn lower(source: &str) -> Module {
    let (.., module) = compile(source);
    module
}
