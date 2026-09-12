//! Snapshot-style syntax suite: each test reprints the parsed expression
//! tree in a fully parenthesized canonical form, so operator grouping
//! (including the Rust/C bitwise precedence order) is pinned exactly.

use frontend::incremental::IncrementalParser;
use syntax::SyntaxKind;

fn parse(source: &str) -> frontend::tree_builder::Parse {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    assert!(parse.errors.is_empty(), "parse errors: {:?}", parse.errors);
    parse.clone()
}

/// Reprint an expression subtree fully parenthesized: every `BinaryExpr`
/// becomes `(lhs op rhs)`, unary expressions `(op operand)`. Anything else
/// prints its source text verbatim, so the canonical form is a faithful
/// snapshot of how the parser grouped the source.
fn reprint(node: &syntax::SyntaxNode) -> String {
    match node.kind() {
        SyntaxKind::BinaryExpr | SyntaxKind::UnaryExpr => {
            let mut parts: Vec<String> = Vec::new();
            for child in node.children_with_tokens() {
                match child {
                    rowan::NodeOrToken::Token(token) => {
                        let text = token.text().trim();
                        if !text.is_empty() {
                            parts.push(text.to_string());
                        }
                    }
                    rowan::NodeOrToken::Node(inner) => parts.push(reprint(&inner)),
                }
            }
            // Unary prints tight (`-a`); binary keeps operator spacing.
            if parts.len() == 2 {
                format!("({})", parts.concat())
            } else {
                format!("({})", parts.join(" "))
            }
        }
        // Leaf nodes carry their leading whitespace in this grammar; trim it
        // so the canonical form depends only on grouping.
        _ => node.text().to_string().trim().replace('\n', " "),
    }
}

/// Parses `fun f(...) -> _ { <body-expr> }` and returns the outermost
/// binary expression of the function body, reprinted.
fn reprint_body(body: &str) -> String {
    let source = format!("fun f(a: i32, b: i32, c: i32, d: i32) -> i32 {{ {body} }}");
    let parse = parse(&source);
    let binary = parse
        .syntax()
        .descendants()
        .find(|node| {
            node.kind() == SyntaxKind::BinaryExpr
                && !node
                    .ancestors()
                    .skip(1)
                    .any(|ancestor| ancestor.kind() == SyntaxKind::BinaryExpr)
        })
        .expect("body must contain a binary expression");
    reprint(&binary)
}

#[test]
fn bitwise_precedence_canonical_form() {
    // The full Rust/C chain, one grouping per pair.
    assert_eq!(reprint_body("a | b & c"), "(a | (b & c))");
    assert_eq!(reprint_body("a ^ b & c"), "(a ^ (b & c))");
    assert_eq!(reprint_body("a | b ^ c"), "(a | (b ^ c))");
    assert_eq!(reprint_body("a & b << c"), "(a & (b << c))");
    assert_eq!(reprint_body("a & b >> c"), "(a & (b >> c))");
    assert_eq!(reprint_body("a << b + c"), "(a << (b + c))");
    assert_eq!(reprint_body("a >> b - c"), "(a >> (b - c))");
    assert_eq!(reprint_body("a + b * c"), "(a + (b * c))");
    assert_eq!(reprint_body("a - b / c"), "(a - (b / c))");
    // `*` and `%` share a tier: left association.
    assert_eq!(reprint_body("a * b % c"), "((a * b) % c)");
}

#[test]
fn bitwise_binds_tighter_than_comparisons_canonical_form() {
    assert_eq!(reprint_body("a & b == c"), "((a & b) == c)");
    assert_eq!(reprint_body("a | b != c"), "((a | b) != c)");
    assert_eq!(reprint_body("a << b < c"), "((a << b) < c)");
    assert_eq!(reprint_body("a + b <= c"), "((a + b) <= c)");
    assert_eq!(reprint_body("a * b >= c"), "((a * b) >= c)");
}

#[test]
fn logical_and_equality_canonical_form() {
    assert_eq!(reprint_body("a == b && c != d"), "((a == b) && (c != d))");
    assert_eq!(
        reprint_body("a < b && c > d || a == c"),
        "(((a < b) && (c > d)) || (a == c))"
    );
}

#[test]
fn associativity_canonical_form() {
    // Same-precedence chains group left, including shifts.
    assert_eq!(reprint_body("a - b - c"), "((a - b) - c)");
    assert_eq!(reprint_body("a / b / c"), "((a / b) / c)");
    assert_eq!(reprint_body("a << b << c"), "((a << b) << c)");
    assert_eq!(reprint_body("a >> b >> c"), "((a >> b) >> c)");
    assert_eq!(reprint_body("a & b & c"), "((a & b) & c)");
    assert_eq!(reprint_body("a | b | c"), "((a | b) | c)");
}

#[test]
fn unary_groups_tighter_than_binary_canonical_form() {
    assert_eq!(reprint_body("-a * b"), "((-a) * b)");
    assert_eq!(reprint_body("!a && b"), "((!a) && b)");
    assert_eq!(reprint_body("-a + b"), "((-a) + b)");
}

#[test]
fn mixed_precedence_chain_canonical_form() {
    // One expression exercising every tier from `|` down to `*`.
    assert_eq!(
        reprint_body("a | b ^ c & d << a + b * c"),
        "(a | (b ^ (c & (d << (a + (b * c))))))"
    );
}

#[test]
fn nested_generic_shift_source_roundtrips() {
    let parse = parse(
        "fun main() { let x = make::<Vec<Vec<i32>>>(); let y = 4i32 >> 1i32; let z = a::<Vec<Vec<i32>>>(b) >> c; }",
    );
    // `>>>` must survive (two generic closes + argument paren) without
    // loss, and the shift still parses as a binary expression after the
    // generic call.
    let text = parse.syntax().text().to_string();
    assert!(text.contains(">>>"), "source text must round-trip: {text}");
    let shifts = parse
        .syntax()
        .descendants()
        .filter(|node| {
            node.kind() == SyntaxKind::BinaryExpr
                && node.children_with_tokens().any(|child| {
                    child
                        .as_token()
                        .is_some_and(|t| t.kind() == SyntaxKind::Shr)
                })
        })
        .count();
    assert_eq!(shifts, 2);
}

#[test]
fn formats_rust_bitwise_precedence_shape() {
    let parse = parse("fun main(a: i32, b: i32, c: i32) { let x = a | b & c; }");
    let binary = parse
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::BinaryExpr)
        .count();
    assert_eq!(binary, 2);
    assert!(parse.syntax().text().to_string().contains("a | b & c"));
}

// == formatter output snapshots ==

fn format(source: &str) -> String {
    riddlec::fmt::format_source(source, riddlec::fmt::FormatOptions::default())
}

#[test]
fn formatter_preserves_bitwise_precedence_tokens() {
    // One statement per precedence tier; the formatter must keep every
    // operator and operand on one line without inserting grouping that
    // would change or obscure the Rust/C precedence reading.
    let formatted = format("fun f(a: i32, b: i32, c: i32) -> i32 { a | b & c ^ d }");
    assert_eq!(
        formatted,
        "fun f(a: i32, b: i32, c: i32) -> i32 {
    a | b & c ^ d
}
"
    );
}

#[test]
fn formatter_preserves_shifts_after_generic_calls() {
    // The formatter re-lexes the source; `>>>` after nested generic close
    // and a following `>>` shift must survive the round trip unchanged.
    let formatted = format("fun main() { let x = make::<Vec<Vec<i32>>>(); let y = 4i32 >> 1i32; }");
    assert_eq!(
        formatted,
        "fun main() {
    let x = make::<Vec<Vec<i32>>>();
    let y = 4i32 >> 1i32;
}
"
    );
}

#[test]
fn formatter_is_idempotent_on_precedence_chains() {
    let source = "fun f(a: i32, b: i32, c: i32, d: i32) -> i32 { a | b ^ c & d << a + b * c }";
    let once = format(source);
    let twice = format(&once);
    assert_eq!(once, twice);
    assert!(once.contains("a | b ^ c & d << a + b * c"));
}
