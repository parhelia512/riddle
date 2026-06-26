mod basic;

use ast::{self, support::AstNode};
use frontend::{incremental::IncrementalParser, tree_builder::Parse};
use hir::{HirFile, lower_root};
use scope_graph::{builder::build_scope_graph, resolve::resolve_hir};
use type_checker::{Diagnostic, check_hir};

fn check_moves(source: &str) -> move_checker::MoveResult {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    assert!(parse.errors.is_empty(), "parse errors: {:?}", parse.errors);

    let hir = lower_and_resolve(&parse);
    let type_result = check_hir(&hir);

    // Fail if there are type errors — move tests assume well-typed input.
    assert!(
        type_result.diagnostics.is_empty(),
        "type errors: {:?}",
        type_result.diagnostics
    );

    move_checker::check_moves(&hir, &type_result)
}

fn lower_and_resolve(parse: &Parse) -> HirFile {
    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax.clone()).unwrap();
    let mut hir = lower_root(root);
    let sg = build_scope_graph(&hir, &syntax);
    resolve_hir(&mut hir, &sg);
    hir
}

fn messages(result: &move_checker::MoveResult) -> Vec<&str> {
    result
        .diagnostics
        .iter()
        .map(|Diagnostic { message }| message.as_str())
        .collect()
}
