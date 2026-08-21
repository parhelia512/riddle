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
fn if_let_binding_is_visible_in_the_then_block() {
    let result = check(&format!(
        r"
        {OPTION}

        fun main() -> i32 {{
            let opt = Option::Some(41);
            if let Option::Some(x) = opt {{
                let payload: i32 = x;
                payload
            }} else {{
                0
            }}
        }}
        "
    ));

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn if_let_binding_has_the_payload_type() {
    let result = check(&format!(
        r"
        {OPTION}

        fun main() {{
            let opt = Option::Some(41);
            if let Option::Some(x) = opt {{
                let wrong: bool = x;
            }}
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
fn if_let_binding_is_not_visible_in_the_else_block() {
    let codes = hir_diagnostic_codes(&format!(
        r"
        {OPTION}

        fun main() -> i32 {{
            let opt = Option::Some(1);
            if let Option::Some(x) = opt {{
                x
            }} else {{
                x
            }}
        }}
        "
    ));

    assert!(codes.iter().any(|code| code == "E0050"), "{codes:#?}");
}

#[test]
fn if_let_binding_is_not_visible_after_the_statement() {
    let codes = hir_diagnostic_codes(&format!(
        r"
        {OPTION}

        fun main() -> i32 {{
            let opt = Option::Some(1);
            if let Option::Some(x) = opt {{
            }}
            x
        }}
        "
    ));

    assert!(codes.iter().any(|code| code == "E0050"), "{codes:#?}");
}

#[test]
fn if_let_supports_else_if_let_chains() {
    let result = check(&format!(
        r"
        {OPTION}

        fun main() -> i32 {{
            let first = Option::None;
            let second = Option::Some(2);
            if let Option::Some(x) = first {{
                x
            }} else if let Option::Some(y) = second {{
                y
            }} else {{
                0
            }}
        }}
        "
    ));

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn if_let_without_an_else_branch_compiles() {
    let result = check(&format!(
        r"
        {OPTION}

        fun main() -> i32 {{
            let opt = Option::Some(7);
            let mut seen = 0;
            if let Option::Some(x) = opt {{
                seen = x;
            }}
            seen
        }}
        "
    ));

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn if_let_rejects_a_scrutinee_of_the_wrong_type() {
    let result = check(&format!(
        r"
        {OPTION}

        fun main() -> i32 {{
            if let Option::Some(x) = 1i32 {{
                x
            }} else {{
                0
            }}
        }}
        "
    ));

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0038"),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn while_let_binding_is_visible_in_the_loop_body() {
    let result = check(&format!(
        r"
        {OPTION}

        fun next(state: i32) -> Option {{
            if state > 0 {{ Option::Some(state - 1) }} else {{ Option::None }}
        }}

        fun main() -> i32 {{
            let mut state = 3;
            let mut total = 0;
            while let Option::Some(v) = next(state) {{
                let step: i32 = v;
                total += step;
                state = v;
            }}
            total
        }}
        "
    ));

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn while_let_binding_is_not_visible_after_the_loop() {
    let codes = hir_diagnostic_codes(&format!(
        r"
        {OPTION}

        fun next(state: i32) -> Option {{
            if state > 0 {{ Option::Some(state - 1) }} else {{ Option::None }}
        }}

        fun main() -> i32 {{
            let mut state = 1;
            while let Option::Some(v) = next(state) {{
                state = v;
            }}
            v
        }}
        "
    ));

    assert!(codes.iter().any(|code| code == "E0050"), "{codes:#?}");
}

#[test]
fn while_let_rejects_a_scrutinee_of_the_wrong_type() {
    let result = check(&format!(
        r"
        {OPTION}

        fun main() {{
            while let Option::Some(v) = 1i32 {{
            }}
        }}
        "
    ));

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0038"),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn if_let_guards_are_a_syntax_error() {
    let mut parser = frontend::incremental::IncrementalParser::new();
    let parse = parser.set_source(&format!(
        r"
        {OPTION}

        fun main() {{
            let opt = Option::Some(1);
            if let Option::Some(x) = opt if x > 0 {{
            }}
        }}
        "
    ));

    assert!(
        !parse.errors.is_empty(),
        "if-let guards are not part of the grammar"
    );
}
