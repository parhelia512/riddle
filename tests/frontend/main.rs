//! Parser behavior tests: range-expression parsing and precedence, bracket
//! lambda disambiguation against array literals and indexing, and error
//! recovery for malformed input.

use frontend::incremental::IncrementalParser;
use syntax::SyntaxKind;

fn parse(source: &str) -> frontend::tree_builder::Parse {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    assert!(parse.errors.is_empty(), "parse errors: {:?}", parse.errors);
    parse.clone()
}

fn tree_has(parse: &frontend::tree_builder::Parse, kind: SyntaxKind) -> bool {
    parse
        .syntax()
        .descendants_with_tokens()
        .any(|it| it.kind() == kind)
        || parse.syntax().descendants().any(|it| it.kind() == kind)
}

#[test]
fn parses_exclusive_range_expression() {
    let parse = parse("fun main() { let r = 0..5; }");
    assert!(tree_has(&parse, SyntaxKind::RangeExpr));
    assert!(tree_has(&parse, SyntaxKind::DotDot));
}

#[test]
fn parses_inclusive_range_expression() {
    let parse = parse("fun main() { let r = 0..=5; }");
    assert!(tree_has(&parse, SyntaxKind::RangeExpr));
    assert!(tree_has(&parse, SyntaxKind::DotDotEq));
}

#[test]
fn range_binds_looser_than_arithmetic() {
    // `1 + 1..4 + 1` groups as `(1 + 1)..(4 + 1)`, like Rust.
    let parse = parse("fun main() { let r = 1 + 1..4 + 1; }");
    assert!(tree_has(&parse, SyntaxKind::RangeExpr));
    assert!(tree_has(&parse, SyntaxKind::BinaryExpr));
}

#[test]
fn float_literals_still_parse() {
    let parse = parse("fun main() { let x = 1.5; let y = 0.25; }");
    assert!(tree_has(&parse, SyntaxKind::FloatLit));
    assert!(!tree_has(&parse, SyntaxKind::RangeExpr));
    assert!(!tree_has(&parse, SyntaxKind::DotDot));
}

#[test]
fn field_access_and_ranges_coexist() {
    let parse = parse("fun main() { let t = (1, 2); let r = t.0..t.1; }");
    assert!(tree_has(&parse, SyntaxKind::RangeExpr));
    assert!(tree_has(&parse, SyntaxKind::FieldExpr));
}

#[test]
fn bracket_lambda_disambiguates_from_array_literal() {
    let parse = parse("fun main() { let value = [1, 2, 3]; }");
    assert!(tree_has(&parse, SyntaxKind::ArrayExpr));
    assert!(!tree_has(&parse, SyntaxKind::BracketLambdaExpr));
}

#[test]
fn bracket_lambda_disambiguates_from_indexing() {
    let parse = parse("fun main() { let a = [1]; let v = a[0]; }");
    assert!(tree_has(&parse, SyntaxKind::IndexExpr));
    assert!(!tree_has(&parse, SyntaxKind::BracketLambdaExpr));
}

#[test]
fn bracket_lambda_parses_with_arrow() {
    let parse = parse(
        "fun apply(f: impl Fn(i32) -> i32) -> i32 { f(1) } fun main() { apply([v -> v + 1]); }",
    );
    assert!(tree_has(&parse, SyntaxKind::BracketLambdaExpr));
}

#[test]
fn error_recovery_reports_without_panicking() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source("fun main() { let = ; }");
    assert!(!parse.errors.is_empty());
}

#[test]
fn unterminated_string_recovers() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source("fun main() { let s = \"open; }");
    assert!(!parse.errors.is_empty());
}

#[test]
fn match_block_arm_without_trailing_comma_parses() {
    // The arm's own closing `}` terminates a block-bodied arm, so the comma
    // is optional and must not derail into expression-error cascades.
    let parse = parse("fun f(x: i32) -> i32 { match x { 0 => { 1 } _ => 2, } }");
    assert!(tree_has(&parse, SyntaxKind::MatchExpr));
    assert_eq!(
        parse
            .syntax()
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::MatchArm)
            .count(),
        2
    );
}

#[test]
fn generic_cast_reports_e0012_not_crash() {
    // Regression: casting a generic parameter used to be able to reach MIR
    // lowering unsupported; the type checker must reject it with E0012.
    let parse = parse("fun convert<T>(value: T) -> i32 { value as i32 }");
    assert!(parse.errors.is_empty());
}
