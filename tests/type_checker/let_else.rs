use crate::check;

const OPTION: &str = "enum Option { Some(i32), None }";

// Unresolved names surface as E0050 on the HIR body during scope resolution,
// not in the type-checker result.
fn hir_diagnostic_codes(source: &str) -> Vec<String> {
    let mut parser = frontend::incremental::IncrementalParser::new();
    let parse = parser.set_source(source);
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    let hir = crate::lower_and_resolve(parse);
    hir.bodies
        .iter()
        .flat_map(|(_, body)| body.diagnostics.iter().map(|d| d.code.to_string()))
        .collect()
}

#[test]
fn let_else_binding_is_visible_after_the_statement() {
    let result = check(&format!(
        r"
        {OPTION}

        fun main() -> i32 {{
            let opt = Option::Some(41);
            let Option::Some(x) = opt else {{
                return 0;
            }};
            let payload: i32 = x;
            payload
        }}
        "
    ));

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn let_else_binding_has_the_payload_type() {
    let result = check(&format!(
        r"
        {OPTION}

        fun main() {{
            let opt = Option::Some(1);
            let Option::Some(x) = opt else {{
                return;
            }};
            let wrong: bool = x;
        }}
        "
    ));

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0001"),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn let_else_allows_a_refutable_pattern() {
    let result = check(&format!(
        r"
        {OPTION}

        fun main() -> i32 {{
            let opt = Option::Some(7);
            let Option::Some(x) = opt else {{
                return 0;
            }};
            x
        }}
        "
    ));

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn let_else_accepts_break_and_an_infinite_loop_as_diverging() {
    let result = check(&format!(
        r"
        {OPTION}

        fun main() -> i32 {{
            let mut i = 2;
            let mut total = 0;
            while i > 0 {{
                let Option::Some(v) = Option::Some(i) else {{
                    break;
                }};
                total += v;
                i -= 1;
            }}
            let Option::Some(x) = Option::Some(total) else {{
                loop {{}}
            }};
            x
        }}
        "
    ));

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn let_else_rejects_a_non_diverging_else_block() {
    for else_block in ["{ }", "{ 0 }"] {
        let result = check(&format!(
            r"
            {OPTION}

            fun main() -> i32 {{
                let opt = Option::Some(1);
                let Option::Some(x) = opt else {else_block};
                x
            }}
            "
        ));

        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0066"),
            "else block {else_block} must be rejected: {:#?}",
            result.diagnostics
        );
    }
}

#[test]
fn let_else_block_sees_outer_bindings() {
    let result = check(&format!(
        r"
        {OPTION}

        fun main() -> i32 {{
            let fallback = 9;
            let opt = Option::None;
            let Option::Some(x) = opt else {{
                return fallback;
            }};
            x
        }}
        "
    ));

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn let_else_binding_is_not_visible_in_the_else_block() {
    let codes = hir_diagnostic_codes(&format!(
        r"
        {OPTION}

        fun main() -> i32 {{
            let opt = Option::Some(1);
            let Option::Some(x) = opt else {{
                let leaked = x;
                return 0;
            }};
            x
        }}
        "
    ));

    assert!(codes.iter().any(|code| code == "E0050"), "{codes:#?}");
}

#[test]
fn let_else_without_an_initializer_is_a_syntax_error() {
    let mut parser = frontend::incremental::IncrementalParser::new();
    let parse = parser.set_source(
        r"
        fun main() {
            let x else {
                return;
            };
        }
        ",
    );

    assert!(
        !parse.errors.is_empty(),
        "let-else without an initializer is not part of the grammar"
    );
}
