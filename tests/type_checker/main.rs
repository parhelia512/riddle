mod basic;
#[path = "../support/diagnostics.rs"]
mod diagnostic_support;
mod dst;
mod errors;
mod incremental;
mod structs;
mod traits;
#[path = "unsafe.rs"]
mod unsafe_test;

use ast::{self, support::AstNode};
use frontend::{incremental::IncrementalParser, tree_builder::Parse};
use hir::{HirFile, lower_root};
use scope_graph::{builder::build_scope_graph, resolve::resolve_hir};
use type_checker::{Diagnostic, TypeCheckResult, check_hir};

fn check(source: &str) -> TypeCheckResult {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let hir = lower_and_resolve(parse);
    let result = check_hir(&hir);
    diagnostic_support::assert_type_diagnostics(source, &result.diagnostics);
    result
}

fn lower_and_resolve(parse: &Parse) -> HirFile {
    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax.clone()).unwrap();
    let mut hir = lower_root(root);
    let (sg, _) = build_scope_graph(&hir, &syntax);
    resolve_hir(&mut hir, &sg);
    diagnostic_support::assert_hir_diagnostics(
        &syntax.to_string(),
        hir.bodies
            .iter()
            .flat_map(|(_, body)| body.diagnostics.iter()),
    );
    hir
}

fn messages(result: &TypeCheckResult) -> Vec<&str> {
    result
        .diagnostics
        .iter()
        .map(|Diagnostic { message, .. }| message.as_str())
        .collect()
}
