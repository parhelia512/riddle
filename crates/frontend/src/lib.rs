pub mod incremental;
pub mod lexer;
pub mod parser;
pub mod tree_builder;

pub use parser::ParseError;

#[cfg(test)]
mod tests {
    use crate::incremental::IncrementalParser;

    #[test]
    fn accepts_rust_style_explicit_generic_calls() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun f<T>(x: T) {
                g::<T>(x);
                crate::g::<T>(x);
                x.convert::<T>();
                Vector::<i32>::new();
                crate::collections::Vector::<i32>::new();
                Vector::<i32>::convert::<u32>();
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    }

    #[test]
    fn rejects_legacy_explicit_generic_call_syntax() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun id<T>(value: T) -> T { value }
            fun main() { id<i32>(1); }
            ",
        );

        assert!(!parse.errors.is_empty());
    }

    #[test]
    fn accepts_callable_bounds_move_lambdas_and_mutable_parameters() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun apply(mut f: impl FnMut(i32) -> i32, value: i32) -> i32 {
                f(value)
            }

            fun main() {
                let bump = move fun(mut value: i32) {
                    value += 1;
                    value
                };
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    }

    #[test]
    fn accepts_parenthesized_callable_generic_bounds() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            "fun apply<F>(f: F, value: i32) -> i32 where F: Fn(i32) -> i32 { f(value) }",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    }

    #[test]
    fn rejects_removed_structural_function_type() {
        let mut parser = IncrementalParser::new();
        let source = ["fun apply(f: ", "fun(i32) -> i32) {}"].concat();
        let parse = parser.set_source(&source);

        assert!(parse.errors.iter().any(|error| {
            error
                .message
                .contains("function type syntax has been removed")
                && error.message.contains("impl Fn(i32) -> i32")
        }));
    }

    #[test]
    fn anonymous_functions_keep_named_nongeneric_parameters() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source("fun main() { let f = fun(value: i32) { value }; }");
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    }

    #[test]
    fn accepts_generic_anonymous_function_patterns_and_callable_impls() {
        let mut parser = IncrementalParser::new();
        let source = r"
            struct Adder { amount: i32 }
            impl Fn(i32) -> i32 for Adder {
                fun call(&self, value: i32) -> i32 { value + self.amount }
            }
            fun main() {
                let first = fun<T>((left, _): (T, T)) -> T { left };
            }
        ";
        let parse = parser.set_source(source);
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    }

    #[test]
    fn parses_function_like_macro_calls_in_supported_positions() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            make_item!();
            fun value(input: make_type!()) -> make_type!() {
                let make_pattern!() = make_expr!();
                make_expr!()
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert_eq!(
            parse
                .syntax()
                .descendants()
                .filter(|node| node.kind() == syntax::SyntaxKind::MacroCall)
                .count(),
            6
        );
    }

    #[test]
    fn block_statement_ends_before_following_prefix_dereference() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun main() {
                let mut value = 0;
                while false {}
                *value = 1;
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert_eq!(
            parse
                .syntax()
                .descendants()
                .filter(|node| node.kind() == syntax::SyntaxKind::ExprStmt)
                .count(),
            2
        );
    }

    #[test]
    fn parses_tuple_field_access() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun main() {
                let values = (1, true);
                values.0;
                values.1;
                values.0.value;
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert_eq!(
            parse
                .syntax()
                .descendants()
                .filter(|node| node.kind() == syntax::SyntaxKind::FieldExpr)
                .count(),
            4
        );
    }

    #[test]
    fn parses_radix_and_separated_integer_literals() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun main() {
                let binary = 0b1010_0101u8;
                let octal = 0o7_7;
                let decimal = 1_000_000i32;
                let hexadecimal = 0xff_ffusize;
                match hexadecimal { 0xFFFF => {}, _ => {} }
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    }

    fn tree_has(parse: &crate::tree_builder::Parse, kind: syntax::SyntaxKind) -> bool {
        parse.syntax().descendants().any(|node| node.kind() == kind)
    }

    #[test]
    fn parses_bracket_lambda_in_expression_position() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun main() {
                let f = [v -> v * 2];
                let g = [acc, v -> acc + v];
                let h = [v: &i32 -> *v > 3];
                let z = [ -> 42];
                let m = move [v -> base + v];
                let p = [(l, _) -> l];
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert!(tree_has(parse, syntax::SyntaxKind::BracketLambdaExpr));
        assert!(!tree_has(parse, syntax::SyntaxKind::ArrayExpr));
    }

    #[test]
    fn parses_bracket_lambda_multi_statement_body() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun main() {
                let classify = [v -> { let sq = v * v; if sq > 10 { 1 } else { 0 } }];
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert!(tree_has(parse, syntax::SyntaxKind::BracketLambdaExpr));
    }

    #[test]
    fn array_literals_are_not_confused_with_bracket_lambdas() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun main() {
                let values = [1, 2, 3];
                let repeated = [0i32; 3];
                let single = [values];
                let nested = [[1, 2], [3, 4]];
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert!(tree_has(parse, syntax::SyntaxKind::ArrayExpr));
        assert!(!tree_has(parse, syntax::SyntaxKind::BracketLambdaExpr));
    }

    #[test]
    fn array_of_bracket_lambdas_keeps_array_root() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun main() {
                let lambdas = [[v -> v], [v -> v * 2]];
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert!(tree_has(parse, syntax::SyntaxKind::ArrayExpr));
        assert_eq!(
            parse
                .syntax()
                .descendants()
                .filter(|node| node.kind() == syntax::SyntaxKind::BracketLambdaExpr)
                .count(),
            2
        );
    }

    #[test]
    fn array_containing_fun_lambda_is_not_misjudged_as_bracket_lambda() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun main() {
                let lambdas = [fun(x) -> i32 { x }];
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert!(tree_has(parse, syntax::SyntaxKind::ArrayExpr));
        assert!(!tree_has(parse, syntax::SyntaxKind::BracketLambdaExpr));
    }

    #[test]
    fn indexing_expressions_are_not_confused_with_bracket_lambdas() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            struct Row { values: [i32; 4] }
            fun pick(values: [i32; 4], index: usize) -> i32 { values[index] }
            fun main(row: Row, f: impl Fn(i32) -> i32) -> i32 {
                let first = row.values[0usize];
                let second = pick(row.values, 1usize);
                let third = f(2i32);
                let applied = [3i32][0usize];
                first + second + third + applied
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert!(tree_has(parse, syntax::SyntaxKind::IndexExpr));
        assert!(!tree_has(parse, syntax::SyntaxKind::BracketLambdaExpr));
    }

    #[test]
    fn parses_trailing_bracket_lambda_calls() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r#"
            fun apply(action: impl Fn() -> ()) { action() }
            fun main() {
                apply([ -> println("hello")]);
            }
            "#,
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    }

    #[test]
    fn parses_bracket_lambda_call_after_call_expression() {
        // `f(args) [lambda]` reads as calling the partial result with the
        // lambda (uniform with `(expr)(args)`); indexing without `->` is
        // unaffected.
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun fold(values: [i32; 3], init: i32) -> i32 { init }
            fun main(values: [i32; 3]) -> i32 {
                let mapped = values.map [v -> v * 2];
                let indexed = fold(values, 0i32)[0usize];
                mapped.len()
            }
            ",
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert!(tree_has(parse, syntax::SyntaxKind::BracketLambdaExpr));
        assert!(tree_has(parse, syntax::SyntaxKind::IndexExpr));
    }

    #[test]
    fn move_before_array_literal_is_rejected() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r"
            fun main() {
                let values = move [0, 1];
            }
            ",
        );

        assert!(!parse.errors.is_empty());
    }
}
