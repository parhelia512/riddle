use super::*;

#[test]
fn semantic_tokens_classifies_core_tokens() {
    let tokens = semantic_tokens("fun main() {\n  let mut x = \"hi\"; // ok\n}");
    let types = tokens
        .data
        .iter()
        .map(|token| token.token_type)
        .collect::<Vec<_>>();

    assert!(types.contains(&TOKEN_KEYWORD));
    assert!(types.contains(&TOKEN_FUNCTION));
    assert!(types.contains(&TOKEN_VARIABLE));
    assert!(types.contains(&TOKEN_STRING));
    assert!(types.contains(&TOKEN_COMMENT));
}

#[test]
fn semantic_tokens_classify_every_keyword() {
    let source = "let fun struct if else while loop break continue return as self mod use mut pub super crate enum trait impl match const type extern unsafe for in where move true false";
    let tokens = semantic_tokens(source);

    assert_eq!(
        tokens
            .data
            .iter()
            .filter(|token| token.token_type == TOKEN_KEYWORD)
            .count(),
        source.split_whitespace().count()
    );
}

#[test]
fn inlay_hints_show_inferred_types() {
    let source =
        "struct Foo{}\n\nfun main(){\n    let a = Foo{};\n    let b = a;\n    let c = a;\n}";
    let hints = inlay_hints_for_source(
        source,
        Range::new(Position::new(0, 0), Position::new(u32::MAX, 0)),
    );
    let type_hints = hints
        .iter()
        .filter(|hint| hint.kind == Some(lsp_types::InlayHintKind::TYPE))
        .collect::<Vec<_>>();

    assert_eq!(hints.len(), 2);
    assert_eq!(type_hints.len(), 2);
    assert!(type_hints.iter().all(|hint| {
        matches!(&hint.label, lsp_types::InlayHintLabel::String(label) if label == ": Foo")
    }));
}

#[test]
fn inlay_hints_respect_the_start_character_on_a_single_line() {
    let source = "fun main() { let first = 1; let second = 2; }";
    let hints = inlay_hints_for_source(
        source,
        Range::new(
            position(source, source.find("let second").unwrap()),
            position(source, source.len()),
        ),
    );

    assert_eq!(hints.len(), 1, "{hints:#?}");
    assert_eq!(
        hints[0].position,
        position(source, source.find("second =").unwrap() + "second".len())
    );
    assert!(matches!(
        &hints[0].label,
        lsp_types::InlayHintLabel::String(label) if label == ": i32"
    ));
}

#[test]
fn inlay_hints_skip_invalid_initializers() {
    let source = "fun main(){\n    let a = 1;\n    let b = a as 2;\n    let c = missing;\n}";
    let hints = inlay_hints_for_source(
        source,
        Range::new(Position::new(0, 0), Position::new(u32::MAX, 0)),
    );

    assert_eq!(hints.len(), 1, "{hints:#?}");
    assert!(matches!(
        &hints[0].label,
        lsp_types::InlayHintLabel::String(label) if label == ": i32"
    ));
}

#[test]
fn semantic_tokens_use_utf16_lengths() {
    let tokens = semantic_tokens("let x = '😀';");
    let string = tokens
        .data
        .iter()
        .find(|token| token.token_type == TOKEN_STRING)
        .unwrap();

    assert_eq!(string.length, 4);
}

#[test]
fn semantic_tokens_keep_utf16_positions_across_lines() {
    let tokens = semantic_token_positions(&semantic_tokens("// 😀\nfun main() {}"));
    let function = tokens
        .iter()
        .find(|token| token.token_type == TOKEN_FUNCTION)
        .unwrap();

    assert_eq!(function.line, 1);
    assert_eq!(function.start, 4);
    assert_eq!(function.length, 4);
}

#[test]
fn semantic_tokens_exclude_cr_from_crlf_comment_lengths() {
    let tokens = semantic_token_positions(&semantic_tokens("// ok\r\nfun main() {}"));
    let comment = tokens
        .iter()
        .find(|token| token.token_type == TOKEN_COMMENT)
        .unwrap();
    let function = tokens
        .iter()
        .find(|token| token.token_type == TOKEN_FUNCTION)
        .unwrap();

    assert_eq!((comment.line, comment.start, comment.length), (0, 0, 5));
    assert_eq!((function.line, function.start, function.length), (1, 4, 4));
}

#[test]
fn semantic_tokens_highlight_str_and_self_as_keywords() {
    let source = "struct Foo { text: &str }\nimpl Foo { fun get(&self) -> &str { self.text } }";
    let tokens = semantic_token_positions(&semantic_tokens(source));

    assert!(
        tokens.iter().any(|token| {
            token.line == 0
                && token.start == 20
                && token.length == 3
                && token.token_type == TOKEN_KEYWORD
        }),
        "{tokens:#?}"
    );
    assert!(
        tokens.iter().any(|token| {
            token.line == 1
                && token.start == 20
                && token.length == 4
                && token.token_type == TOKEN_KEYWORD
        }),
        "{tokens:#?}"
    );
    assert!(
        tokens.iter().any(|token| {
            token.line == 1
                && token.start == 36
                && token.length == 4
                && token.token_type == TOKEN_KEYWORD
        }),
        "{tokens:#?}"
    );
}

#[test]
fn semantic_tokens_distinguish_methods_structs_enums_and_traits() {
    let source = r"struct Point {}
enum State { Ready }
trait Draw { fun draw(&self); }
impl Draw for Point { fun draw(&self) {} }
fun main() {
    let point = Point {};
    point.draw();
    let state = State::Ready;
}";
    let tokens = semantic_token_positions(&semantic_tokens(source));
    let symbols = tokens
        .iter()
        .map(|token| {
            let line = source.lines().nth(token.line as usize).unwrap();
            let start = token.start as usize;
            let end = start + token.length as usize;
            (
                &line[start..end],
                token.token_type,
                token.token_modifiers_bitset,
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, _)| *text == "Point" && *kind == TOKEN_STRUCT)
            .count(),
        3,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, _)| matches!((*text, *kind), ("State" | "Ready", TOKEN_ENUM)))
            .count(),
        4,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, _)| *text == "Draw" && *kind == TOKEN_INTERFACE)
            .count(),
        2,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, _)| *text == "draw" && *kind == TOKEN_METHOD)
            .count(),
        3,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, modifiers)| {
                *text == "draw" && *kind == TOKEN_METHOD && *modifiers == MOD_DECLARATION
            })
            .count(),
        2,
        "{symbols:#?}"
    );
}

#[test]
fn semantic_tokens_classify_std_structs_and_associated_new() {
    let source = include_str!("../../std/std/vector.rid");
    let tokens = semantic_token_positions(&semantic_tokens_for_source_with_options(
        source,
        CompileOptions::default(),
        true,
    ));
    let symbols = tokens
        .iter()
        .map(|token| {
            let line = source.lines().nth(token.line as usize).unwrap();
            let start = token.start as usize;
            let end = start + token.length as usize;
            (
                &line[start..end],
                token.token_type,
                token.token_modifiers_bitset,
            )
        })
        .collect::<Vec<_>>();

    assert!(
        symbols.iter().any(|(text, kind, modifiers)| {
            *text == "Vector" && *kind == TOKEN_STRUCT && *modifiers & MOD_DEFAULT_LIBRARY != 0
        }),
        "{symbols:#?}"
    );

    let new_tokens = symbols
        .iter()
        .filter(|(text, kind, _)| *text == "new" && *kind == TOKEN_METHOD)
        .collect::<Vec<_>>();
    assert!(!new_tokens.is_empty(), "{symbols:#?}");
    assert!(
        new_tokens.iter().all(|(_, _, modifiers)| {
            *modifiers & MOD_STATIC != 0 && *modifiers & MOD_DEFAULT_LIBRARY != 0
        }),
        "{symbols:#?}"
    );
}

#[test]
fn semantic_tokens_mark_standard_library_traits() {
    let source = "fun inspect(value: Copy) {}";
    let tokens = semantic_token_positions(&semantic_tokens_for_source_with_options(
        source,
        CompileOptions::default(),
        false,
    ));
    let copy_start =
        u32::try_from(source.find("Copy").unwrap()).expect("test offset should fit in u32");

    assert!(
        tokens.iter().any(|token| {
            token.line == 0
                && token.start == copy_start
                && token.token_type == TOKEN_INTERFACE
                && token.token_modifiers_bitset & MOD_DEFAULT_LIBRARY != 0
        }),
        "{tokens:#?}"
    );
}

#[test]
fn semantic_tokens_classify_primitives_type_annotations_and_free_functions() {
    let source = r"fun make(value: i32, size: usize, enabled: bool) -> Vector<i32> {
    Vector::new()
}

fun main() {
    let s: Vector<i32> = make(1i32, 0usize, true);
}";
    let tokens = semantic_token_positions(&semantic_tokens_for_source_with_options(
        source,
        CompileOptions::default(),
        false,
    ));
    let symbols = tokens
        .iter()
        .map(|token| {
            let line = source.lines().nth(token.line as usize).unwrap();
            let start = token.start as usize;
            let end = start + token.length as usize;
            (
                &line[start..end],
                token.token_type,
                token.token_modifiers_bitset,
            )
        })
        .collect::<Vec<_>>();

    for primitive in ["i32", "usize", "bool"] {
        assert!(
            symbols
                .iter()
                .any(|(text, kind, _)| { *text == primitive && *kind == TOKEN_KEYWORD }),
            "{primitive}: {symbols:#?}"
        );
    }
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, modifiers)| {
                *text == "Vector" && *kind == TOKEN_STRUCT && *modifiers & MOD_DEFAULT_LIBRARY != 0
            })
            .count(),
        3,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, _)| *text == "make" && *kind == TOKEN_FUNCTION)
            .count(),
        2,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, modifiers)| {
                matches!(*text, "make" | "main")
                    && *kind == TOKEN_FUNCTION
                    && *modifiers == MOD_DECLARATION
            })
            .count(),
        2,
        "{symbols:#?}"
    );
}

#[test]
fn semantic_tokens_mark_mutable_locals_like_rust_analyzer() {
    let tokens = semantic_tokens("fun main() { let x = 1; x; let mut y = 2; y; }");
    let variables = tokens
        .data
        .iter()
        .filter(|token| token.token_type == TOKEN_VARIABLE)
        .map(|token| token.token_modifiers_bitset)
        .collect::<Vec<_>>();

    // Immutable locals now appear at use sites with modifier 0;
    // mutable locals carry MOD_MUTABLE at use sites and MOD_DECLARATION|MOD_MUTABLE at their binding.
    assert_eq!(
        variables,
        vec![0, MOD_DECLARATION | MOD_MUTABLE, MOD_MUTABLE]
    );
}

#[test]
fn semantic_tokens_prefer_resolved_variables_over_lexical_types() {
    let tokens = semantic_tokens("fun main() { let mut Foo = 1; Foo; }");
    let types = tokens
        .data
        .iter()
        .map(|token| token.token_type)
        .collect::<Vec<_>>();

    assert_eq!(
        types
            .iter()
            .filter(|token_type| **token_type == TOKEN_VARIABLE)
            .count(),
        2
    );
    assert!(!types.contains(&TOKEN_TYPE));
}

#[test]
fn semantic_tokens_prefer_immutable_locals_over_same_named_structs() {
    let source = "struct Foo {}\nfun main() { let Foo = 1; Foo; }";
    let use_position = position(source, source.rfind("Foo;").unwrap());
    let use_line = use_position.line;
    let use_start = use_position.character;
    let tokens = semantic_token_positions(&semantic_tokens(source));
    let token = tokens
        .iter()
        .find(|token| token.line == use_line && token.start == use_start)
        .unwrap();

    assert_eq!(token.token_type, TOKEN_VARIABLE, "{tokens:#?}");
}

#[test]
fn semantic_tokens_place_local_declaration_and_use_on_identifier() {
    let source = "fun main() {\n  let mut foo_bar = 1; foo_bar;\n}";
    let tokens = semantic_token_positions(&semantic_tokens(source));
    let variables = tokens
        .iter()
        .filter(|token| token.token_type == TOKEN_VARIABLE)
        .collect::<Vec<_>>();

    assert_eq!(
        variables,
        vec![
            &SemanticTokenPosition {
                line: 1,
                start: 10,
                length: 7,
                token_type: TOKEN_VARIABLE,
                token_modifiers_bitset: MOD_DECLARATION | MOD_MUTABLE,
            },
            &SemanticTokenPosition {
                line: 1,
                start: 23,
                length: 7,
                token_type: TOKEN_VARIABLE,
                token_modifiers_bitset: MOD_MUTABLE,
            },
        ]
    );
}

#[test]
fn semantic_tokens_separate_parameters_from_immutable_locals() {
    let source = r#"unsafe extern "C" { fun putchar(c: i32) -> i32; }

fun print_digit(n: i32){
    putchar(n + 48);
    putchar(10);
}

fun main(){
    let t = fun(x) {
        x+1
    };
    let v = t(1);
    print_digit(v);
}"#;
    let symbols = semantic_token_positions(&semantic_tokens(source))
        .into_iter()
        .filter(|token| matches!(token.token_type, TOKEN_PARAMETER | TOKEN_VARIABLE))
        .collect::<Vec<_>>();

    assert_eq!(
        symbols,
        vec![
            SemanticTokenPosition {
                line: 0,
                start: 32,
                length: 1,
                token_type: TOKEN_PARAMETER,
                token_modifiers_bitset: MOD_DECLARATION,
            },
            SemanticTokenPosition {
                line: 2,
                start: 16,
                length: 1,
                token_type: TOKEN_PARAMETER,
                token_modifiers_bitset: MOD_DECLARATION,
            },
            SemanticTokenPosition {
                line: 3,
                start: 12,
                length: 1,
                token_type: TOKEN_PARAMETER,
                token_modifiers_bitset: 0,
            },
            SemanticTokenPosition {
                line: 8,
                start: 16,
                length: 1,
                token_type: TOKEN_PARAMETER,
                token_modifiers_bitset: MOD_DECLARATION,
            },
            SemanticTokenPosition {
                line: 9,
                start: 8,
                length: 1,
                token_type: TOKEN_PARAMETER,
                token_modifiers_bitset: 0,
            },
            // Immutable local closure `t` at its call site in `t(1)`.
            SemanticTokenPosition {
                line: 11,
                start: 12,
                length: 1,
                token_type: TOKEN_VARIABLE,
                token_modifiers_bitset: 0,
            },
            // Immutable local `v` at its use site in `print_digit(v)`.
            SemanticTokenPosition {
                line: 12,
                start: 16,
                length: 1,
                token_type: TOKEN_VARIABLE,
                token_modifiers_bitset: 0,
            },
        ]
    );
}
