pub mod incremental;
pub mod lexer;
pub mod parser;
pub mod syntax_kind;
pub mod tree_builder;

pub use parser::ParseError;

#[cfg(test)]
mod tests {
    use crate::incremental::IncrementalParser;

    #[test]
    fn accepts_rust_style_explicit_generic_calls() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r#"
            fun f<T>(x: T) {
                g::<T>(x);
                crate::g::<T>(x);
                x.convert::<T>();
            }
            "#,
        );

        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    }

    #[test]
    fn rejects_legacy_explicit_generic_call_syntax() {
        let mut parser = IncrementalParser::new();
        let parse = parser.set_source(
            r#"
            fun id<T>(value: T) -> T { value }
            fun main() { id<i32>(1); }
            "#,
        );

        assert!(!parse.errors.is_empty());
    }
}
