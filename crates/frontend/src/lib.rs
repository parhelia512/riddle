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
        let pattern =
            parser.set_source("fun main() { let f = fun((left, right): (i32, i32)) { left }; }");
        assert!(!pattern.errors.is_empty());

        let generic = parser.set_source("fun main() { let f = fun<T>(value: T) { value }; }");
        assert!(!generic.errors.is_empty());
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
}
