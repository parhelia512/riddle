use super::*;
use lsp_types::{DiagnosticSeverity, Range};
use riddlec::pipeline::IntoDiagnosticExt;
use std::{
    cell::Cell,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const DOCUMENTED_ERROR_CODES: &[&str] = &[
    "E0001", "E0002", "E0003", "E0004", "E0005", "E0006", "E0007", "E0008", "E0009", "E0010",
    "E0011", "E0012", "E0013", "E0020", "E0021", "E0022", "E0023", "E0024", "E0025", "E0026",
    "E0027", "E0028", "E0029", "E0030", "E0031", "E0032", "E0033", "E0034", "E0035", "E0036",
    "E0037", "E0038", "E0039", "E0040", "E0041", "E0042", "E0043", "E0044", "E0045", "E0050",
    "E0051", "E0052", "E0072", "E0100", "E0200", "E0300", "E0301", "E0302", "E0303", "E0304",
];
const SOURCE_UNREACHABLE_CODES: &[&str] = &["E0021", "E0200"];

fn temp_root(name: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "riddle-lsp-{name}-{}-{}",
        process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn semantic_tokens(source: &str) -> SemanticTokens {
    semantic_tokens_for_source(source)
}

fn source_label(
    range: TextRange,
    message: &str,
    style: type_checker::LabelStyle,
) -> type_checker::SourceLabel {
    type_checker::SourceLabel {
        range,
        message: message.into(),
        style,
    }
}

fn diagnostic_ext(
    code: &'static str,
    severity: type_checker::Severity,
    labels: Vec<type_checker::SourceLabel>,
) -> riddlec::pipeline::DiagnosticExt {
    riddlec::pipeline::DiagnosticExt {
        code,
        severity,
        message: "message".into(),
        labels,
        help: None,
        notes: Vec::new(),
    }
}

#[test]
fn position_counts_utf16_columns() {
    let source = "a😀\nb";
    assert_eq!(position(source, 0), Position::new(0, 0));
    assert_eq!(position(source, "a😀".len()), Position::new(0, 3));
    assert_eq!(position(source, source.len()), Position::new(1, 1));
}

#[test]
fn every_documented_error_code_keeps_rust_style_lsp_fields() {
    let error_docs = include_str!("../../docs/zh-CN/src/errorcode.md");
    let source = "  target  ";
    let uri = lsp_types::Url::parse("file:///diagnostic-codes.rid").unwrap();
    let primary = source_label(
        TextRange::new(0.into(), (source.len() as u32).into()),
        "",
        type_checker::LabelStyle::Primary,
    );

    for &code in DOCUMENTED_ERROR_CODES {
        let diagnostic = to_lsp(
            &uri,
            source,
            diagnostic_ext(code, type_checker::Severity::Error, vec![primary.clone()]),
        )
        .unwrap();

        assert_eq!(
            diagnostic.code,
            Some(lsp_types::NumberOrString::String(code.into()))
        );
        assert_eq!(diagnostic.source.as_deref(), Some("riddle"));
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diagnostic.range,
            Range::new(Position::new(0, 2), Position::new(0, 8))
        );
        let code_description = diagnostic.code_description.unwrap();
        let anchor = code.to_ascii_lowercase();
        assert_eq!(code_description.href.fragment(), Some(anchor.as_str()));
        assert!(
            error_docs.contains(&format!("<a id=\"{anchor}\"></a>")),
            "missing documentation anchor for {code}"
        );
    }
}

#[test]
fn diagnostic_conversion_uses_primary_style_utf16_and_related_labels() {
    let source = "😀  primary  secondary";
    let uri = lsp_types::Url::parse("file:///labels.rid").unwrap();
    let primary_start = source.find("primary").unwrap();
    let secondary_start = source.find("secondary").unwrap();
    let mut input = diagnostic_ext(
        "E0001",
        type_checker::Severity::Warning,
        vec![
            source_label(
                TextRange::new(
                    (secondary_start as u32).into(),
                    ((secondary_start + "secondary".len()) as u32).into(),
                ),
                "secondary label",
                type_checker::LabelStyle::Secondary,
            ),
            source_label(
                TextRange::new(
                    ((primary_start - 2) as u32).into(),
                    ((primary_start + "primary".len() + 2) as u32).into(),
                ),
                "primary label",
                type_checker::LabelStyle::Primary,
            ),
        ],
    );
    input.help = Some("fix it".into());
    input.notes.push("context".into());

    let diagnostic = to_lsp(&uri, source, input).unwrap();

    assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::WARNING));
    assert_eq!(
        diagnostic.range,
        Range::new(Position::new(0, 4), Position::new(0, 11))
    );
    assert_eq!(
        diagnostic.message,
        "message\nprimary label\nhelp: fix it\nnote: context"
    );
    let related = diagnostic.related_information.unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].location.uri, uri);
    assert_eq!(related[0].message, "secondary label");
    assert_eq!(
        related[0].location.range,
        Range::new(Position::new(0, 13), Position::new(0, 22))
    );
}

#[test]
fn diagnostic_conversion_maps_every_severity() {
    let source = "x";
    let uri = lsp_types::Url::parse("file:///severity.rid").unwrap();
    let label = source_label(
        TextRange::new(0.into(), 1.into()),
        "",
        type_checker::LabelStyle::Primary,
    );
    let cases = [
        (type_checker::Severity::Error, DiagnosticSeverity::ERROR),
        (type_checker::Severity::Warning, DiagnosticSeverity::WARNING),
        (
            type_checker::Severity::Note,
            DiagnosticSeverity::INFORMATION,
        ),
        (type_checker::Severity::Help, DiagnosticSeverity::HINT),
    ];

    for (input, expected) in cases {
        let diagnostic = to_lsp(
            &uri,
            source,
            diagnostic_ext("E0001", input, vec![label.clone()]),
        )
        .unwrap();
        assert_eq!(diagnostic.severity, Some(expected));
    }
}

#[test]
fn diagnostic_conversion_rejects_non_utf8_boundaries() {
    let source = "😀";
    let uri = lsp_types::Url::parse("file:///invalid-range.rid").unwrap();
    let diagnostic = diagnostic_ext(
        "E0001",
        type_checker::Severity::Error,
        vec![source_label(
            TextRange::new(1.into(), 2.into()),
            "",
            type_checker::LabelStyle::Primary,
        )],
    );

    assert!(to_lsp(&uri, source, diagnostic).is_none());
}

#[test]
fn parser_eof_diagnostic_stays_at_user_eof_with_std() {
    let source = "fun main() {";
    let uri = lsp_types::Url::parse("file:///eof.rid").unwrap();
    let result = riddlec::pipeline::compile(source);
    let diagnostics = collect_diagnostics(&uri, source, &result);

    assert!(!diagnostics.is_empty());
    assert!(diagnostics.iter().all(|diagnostic| {
        diagnostic.range.start == Position::new(0, source.len() as u32)
            && diagnostic.range.end == Position::new(0, source.len() as u32)
    }));
}

#[test]
fn positions_handle_crlf_and_utf16_at_eof() {
    let source = "a\r\n😀";
    assert_eq!(position(source, source.len()), Position::new(1, 2));
}

#[test]
fn full_text_uses_latest_full_sync_change() {
    let old = TextDocumentContentChangeEvent {
        range: None,
        range_length: None,
        text: "old".into(),
    };
    let new = TextDocumentContentChangeEvent {
        range: None,
        range_length: None,
        text: "new".into(),
    };

    assert_eq!(full_text(vec![old, new]).as_deref(), Some("new"));
}

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
    let source = "let fun struct if else while break continue return as self mod use mut pub super crate enum trait impl match const type extern unsafe for in where true false";
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
fn semantic_tokens_mark_mutable_locals_like_rust_analyzer() {
    let tokens = semantic_tokens("fun main() { let x = 1; x; let mut y = 2; y; }");
    let variables = tokens
        .data
        .iter()
        .filter(|token| token.token_type == TOKEN_VARIABLE)
        .map(|token| token.token_modifiers_bitset)
        .collect::<Vec<_>>();

    assert_eq!(variables, vec![MOD_DECLARATION | MOD_MUTABLE, MOD_MUTABLE]);
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
fn collect_diagnostics_ignores_appended_std_diagnostics() {
    let source = include_str!("../../std/std/array.rid");
    let result = riddlec::pipeline::compile(source);
    let uri = lsp_types::Url::parse("file:///std/std/array.rid").unwrap();
    let diagnostics = collect_diagnostics(&uri, source, &result);

    assert!(
        diagnostics.iter().all(|diagnostic| !diagnostic
            .message
            .contains("expected IntoIter<T, N>, got IntoIter<T, N>")),
        "{diagnostics:#?}"
    );
}

#[test]
fn project_diagnostics_use_unsaved_module_source() {
    let root = temp_root("project-diagnostics");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.rid"),
        "mod util;\nfun main() -> i32 { util::value() }\n",
    )
    .unwrap();
    let util = root.join("src/util.rid");
    fs::write(&util, "pub fun value() -> i32 { 1 }\n").unwrap();
    let uri = lsp_types::Url::from_file_path(&util).unwrap();
    let text = "pub fun value() -> i32 { missing }\n".to_string();
    let docs = HashMap::from([(
        uri.clone(),
        Document {
            text: text.clone(),
            version: Some(1),
        },
    )]);

    let diagnostics = collect_document_diagnostics(&uri, &text, &docs, CompileOptions::default());

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unresolved name: `missing`")),
        "{diagnostics:#?}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn project_diagnostics_include_unopened_modules() {
    let root = temp_root("unopened-module");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main = root.join("src/main.rid");
    let main_text = "mod util;\nfun main() { util::value(); }\n".to_string();
    fs::write(&main, &main_text).unwrap();
    let util = root.join("src/util.rid");
    let util_text = "pub fun value() { missing; }\n";
    fs::write(&util, util_text).unwrap();
    let main_uri = lsp_types::Url::from_file_path(&main).unwrap();
    let docs = HashMap::from([(
        main_uri,
        Document {
            text: main_text,
            version: Some(1),
        },
    )]);

    let published = collect_workspace_diagnostics(&docs, CompileOptions::default());
    let util_uri = lsp_types::Url::from_file_path(fs::canonicalize(&util).unwrap()).unwrap();
    let util_diagnostics = published.iter().find(|item| item.uri == util_uri).unwrap();
    let unresolved = util_diagnostics
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == Some(lsp_types::NumberOrString::String("E0050".into()))
        })
        .unwrap();
    let start = util_text.find("missing").unwrap();

    assert_eq!(util_diagnostics.version, None);
    assert_eq!(
        unresolved.range,
        range(
            util_text,
            TextRange::new(
                (start as u32).into(),
                ((start + "missing".len()) as u32).into(),
            ),
        )
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn project_diagnostics_preserve_the_open_document_uri() {
    let root = temp_root("open-uri-identity");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main = root.join("src/main.rid");
    fs::write(&main, "fun main() {}\n").unwrap();

    let aliased_path = PathBuf::from(main.as_os_str().to_string_lossy().to_ascii_uppercase());
    let opened_uri = lsp_types::Url::from_file_path(&aliased_path).unwrap();
    let docs = HashMap::from([(
        opened_uri.clone(),
        Document {
            text: "fun main() { missing; }\n".into(),
            version: Some(7),
        },
    )]);

    let published = collect_workspace_diagnostics(&docs, CompileOptions::default());
    let item = published
        .iter()
        .find(|item| {
            item.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == Some(lsp_types::NumberOrString::String("E0050".into()))
            })
        })
        .unwrap();

    assert_eq!(item.uri, opened_uri);
    assert_eq!(item.version, Some(7));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn mapped_diagnostic_keeps_cross_file_related_information() {
    let root = temp_root("cross-file-labels");
    fs::create_dir_all(&root).unwrap();
    let main = root.join("main.rid");
    let main_text = "mod util;\nfun main() { root_error; }\n";
    fs::write(&main, main_text).unwrap();
    let util = root.join("util.rid");
    let util_text = "pub fun value() { related; }\n";
    fs::write(&util, util_text).unwrap();
    let loaded = riddlec::pipeline::load_source_file(&main).unwrap();
    let primary_start = loaded.source.find("root_error").unwrap();
    let secondary_start = loaded.source.find("related").unwrap();
    let input = diagnostic_ext(
        "E0001",
        type_checker::Severity::Error,
        vec![
            source_label(
                TextRange::new(
                    (secondary_start as u32).into(),
                    ((secondary_start + "related".len()) as u32).into(),
                ),
                "declared here",
                type_checker::LabelStyle::Secondary,
            ),
            source_label(
                TextRange::new(
                    (primary_start as u32).into(),
                    ((primary_start + "root_error".len()) as u32).into(),
                ),
                "",
                type_checker::LabelStyle::Primary,
            ),
        ],
    );

    let (uri, diagnostic) = diagnostics::to_lsp_mapped(&loaded.source_map, input).unwrap();
    let related = diagnostic.related_information.unwrap();

    assert_eq!(
        uri,
        lsp_types::Url::from_file_path(fs::canonicalize(&main).unwrap()).unwrap()
    );
    assert_eq!(related.len(), 1);
    assert_eq!(
        related[0].location.uri,
        lsp_types::Url::from_file_path(fs::canonicalize(&util).unwrap()).unwrap()
    );
    let related_start = util_text.find("related").unwrap();
    assert_eq!(
        related[0].location.range,
        range(
            util_text,
            TextRange::new(
                (related_start as u32).into(),
                ((related_start + "related".len()) as u32).into(),
            ),
        )
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn reachable_diagnostic_producers_have_exact_primary_and_lsp_spans() {
    let cases = [
        (
            "E0001",
            "let initializer",
            "fun main() { let emoji = \"😀\"; let value: bool = 1; }",
            "let value: bool = 1;",
            "let value: bool = 1;",
        ),
        (
            "E0002",
            "if branches",
            "fun main() { let value = if true { 1 } else { false }; }",
            "if true { 1 } else { false }",
            "if true { 1 } else { false }",
        ),
        (
            "E0003",
            "remainder requires integer operands",
            "fun main() { let value = true % false; }",
            "true % false",
            "true % false",
        ),
        (
            "E0004",
            "cannot call value",
            "fun main() { let value = 1; value(); }",
            "value",
            "; value",
        ),
        (
            "E0005",
            "expects 1 argument",
            "fun takes(value: i32) {} fun main() { takes(); }",
            "takes()",
            "takes()",
        ),
        (
            "E0006",
            "unknown field",
            "struct Point { x: i32 } fun main() { let value = Point { y: 1 }; }",
            "Point { y: 1 }",
            "Point { y: 1 }",
        ),
        (
            "E0007",
            "missing field",
            "struct Point { x: i32, y: i32 } fun main() { let value = Point { x: 1 }; }",
            "Point { x: 1 }",
            "Point { x: 1 }",
        ),
        (
            "E0008",
            "cannot dereference",
            "fun main() { let value = *1; }",
            "1",
            "*1",
        ),
        (
            "E0009",
            "struct literal does not resolve",
            "fun main() { let value = Missing { x: 1 }; }",
            "Missing { x: 1 }",
            "Missing { x: 1 }",
        ),
        (
            "E0010",
            "tuple pattern expects",
            "fun check(value: (i32,)) { match value { (left, right) => {} } }",
            "(left, right)",
            "(left, right)",
        ),
        (
            "E0011",
            "out of range for `u8`",
            "fun check(value: u8) { match value { 256 => {}, _ => {} } }",
            "256",
            "256",
        ),
        (
            "E0012",
            "cannot cast `bool` to `f64`",
            "fun main() { let value = true as f64; }",
            "true as f64",
            "true as f64",
        ),
        (
            "E0013",
            "unknown method",
            "fun main() { let value = 1; value.missing(); }",
            "value.missing()",
            "value.missing()",
        ),
        (
            "E0020",
            "duplicate method",
            "trait Foo { fun bar(); fun bar(); }",
            "bar",
            "fun bar(); fun bar",
        ),
        (
            "E0022",
            "duplicate associated type",
            "trait Foo { type Item; type Item; }",
            "Item",
            "type Item; type Item",
        ),
        (
            "E0023",
            "unknown trait",
            "struct Point {}\nimpl Missing for Point {}",
            "Missing",
            "impl Missing",
        ),
        (
            "E0024",
            "duplicate method",
            "struct Point {}\nimpl Point { fun bar() {} fun bar() {} }",
            "bar",
            "fun bar() {} fun bar",
        ),
        (
            "E0025",
            "duplicate associated type",
            "struct Point {}\nimpl Point { type Item = i32; type Item = bool; }",
            "Item",
            "type Item = i32; type Item",
        ),
        (
            "E0026",
            "missing method",
            "trait Foo { fun bar(); }\nstruct Point {}\nimpl Foo for Point {}",
            "Point",
            "impl Foo for Point",
        ),
        (
            "E0027",
            "missing associated type",
            "trait Foo { type Item; }\nstruct Point {}\nimpl Foo for Point {}",
            "Point",
            "impl Foo for Point",
        ),
        (
            "E0028",
            "parameter count mismatch",
            "trait Foo { fun bar(value: i32); }\nstruct Point {}\nimpl Foo for Point { fun bar() {} }",
            "bar",
            "impl Foo for Point { fun bar",
        ),
        (
            "E0029",
            "parameter 1 type mismatch",
            "trait Foo { fun bar(value: i32); }\nstruct Point {}\nimpl Foo for Point { fun bar(value: bool) {} }",
            "bool",
            "value: bool",
        ),
        (
            "E0030",
            "return type mismatch",
            "trait Foo { fun bar() -> i32; }\nstruct Point {}\nimpl Foo for Point { fun bar() -> bool { true } }",
            "bool",
            "-> bool",
        ),
        (
            "E0031",
            "not declared as mutable",
            "fun main() { let value = 1; value = 2; }",
            "value = 2",
            "; value = 2",
        ),
        (
            "E0032",
            "expects 1 type argument",
            "struct Box<T> { value: T }\nfun main() { let value: Box<i32, bool>; }",
            "Box<i32, bool>",
            "Box<i32, bool>",
        ),
        (
            "E0033",
            "calling `g`",
            "struct Wrap<T> { inner: T } fun f<T>(x: T) -> T { return g(Wrap { inner: x }); } fun g<T>(x: T) -> T { return f(Wrap { inner: x }); }",
            "g(Wrap { inner: x })",
            "g(Wrap { inner: x })",
        ),
        (
            "E0034",
            "unknown type `Missing`",
            "fun main() { let value: Missing; }",
            "Missing",
            "Missing",
        ),
        (
            "E0035",
            "missing `IntoIterator` trait",
            "fun main() { for item in 1 {} }",
            "for item in 1 {}",
            "for item in 1 {}",
        ),
        (
            "E0036",
            "requires `PartialEq`",
            "#[lang = \"partial_eq\"] trait PartialEq {}\n#[lang = \"eq\"] trait Eq {}\nstruct Point {}\nimpl Eq for Point {}",
            "Point",
            "impl Eq for Point",
        ),
        (
            "E0037",
            "not strictly smaller",
            "trait Foo {}\nstruct Vec<T> { value: T }\nimpl<T> Foo for T where Vec<T>: Foo {}",
            "Foo",
            "Vec<T>: Foo",
        ),
        (
            "E0038",
            "requires a payload pattern",
            "enum State { Ready, Done(i32) }\nfun check(value: State) { match value { State::Done => {} } }",
            "State::Done",
            "State::Done",
        ),
        (
            "E0039",
            "non-exhaustive match",
            "fun check(value: bool) { match value { true => {} } }",
            "match value { true => {} }",
            "match value { true => {} }",
        ),
        (
            "E0040",
            "invalid integer literal",
            "fun main() { let value = 9223372036854775808; }",
            "9223372036854775808",
            "9223372036854775808",
        ),
        (
            "E0041",
            "non-Copy field",
            "#[lang = \"copy\"] trait Copy {}\nstruct Token { value: i32 }\nstruct Wrapper { value: Token }\nimpl Copy for Wrapper {}",
            "Wrapper",
            "impl Copy for Wrapper",
        ),
        (
            "E0042",
            "`break` outside",
            "fun main() { break; }",
            "break;",
            "break;",
        ),
        (
            "E0043",
            "contains unsized `str`",
            "fun main() { let value: str; }",
            "let value: str;",
            "let value: str;",
        ),
        (
            "E0044",
            "cannot capture `base`",
            "fun main() { match 1 { base => { let read = fun() { base }; } } }",
            "base",
            "fun() { base",
        ),
        (
            "E0045",
            "parameter `x`",
            "fun main() { let identity = fun(x) { x }; }",
            "x",
            "fun(x",
        ),
        (
            "E0050",
            "unresolved name",
            "fun main() { missing; }",
            "missing",
            "missing",
        ),
        (
            "E0051",
            "empty use declaration",
            "use crate;\nfun main() {}",
            "crate",
            "crate",
        ),
        (
            "E0052",
            "glob import target not found",
            "use missing::*;\nfun main() {}",
            "missing::*",
            "missing::*",
        ),
        (
            "E0072",
            "recursive type",
            "enum Loop { Next(Loop) }",
            "Loop",
            "enum Loop",
        ),
        (
            "E0100",
            "use of moved value",
            "struct Point { x: i32 } fun main() { let original = Point { x: 1 }; let moved = original; let second = original; }",
            "original",
            "let second = original",
        ),
        (
            "E0300",
            "borrow `point` as mutable",
            "struct Point { x: i32 } fun main() { let mut point = Point { x: 1 }; let shared = &point; let mutable = &mut point; }",
            "&mut point",
            "&mut point",
        ),
        (
            "E0301",
            "borrow `point` as immutable",
            "struct Point { x: i32 } fun main() { let mut point = Point { x: 1 }; let mutable = &mut point; let shared = &point; }",
            "&point",
            "&point",
        ),
        (
            "E0302",
            "borrow `point` as mutable more than once",
            "struct Point { x: i32 } fun main() { let mut point = Point { x: 1 }; let first = &mut point; let second = &mut point; }",
            "&mut point",
            "let second = &mut point",
        ),
        (
            "E0303",
            "assign to `point` while borrowed",
            "struct Point { x: i32 } fun main() { let mut point = Point { x: 1 }; let shared = &point; point = Point { x: 2 }; }",
            "point = Point { x: 2 }",
            "point = Point { x: 2 }",
        ),
        (
            "E0304",
            "cannot move `point` while borrowed",
            "struct Point { x: i32 } fun main() { let point = Point { x: 1 }; let shared = &point; let moved = point; }",
            "point",
            "let moved = point",
        ),
    ];
    let uri = lsp_types::Url::parse("file:///producer-spans.rid").unwrap();

    for &code in DOCUMENTED_ERROR_CODES {
        let expected_count = usize::from(!SOURCE_UNREACHABLE_CODES.contains(&code));
        assert_eq!(
            cases.iter().filter(|case| case.0 == code).count(),
            expected_count,
            "unexpected producer fixture count for {code}"
        );
    }

    for (code, message, source, expected, marker) in cases {
        let result =
            riddlec::pipeline::compile_with_options(source, CompileOptions { use_std: false });
        assert!(
            result.parse_errors.is_empty(),
            "{}: {:#?}",
            code,
            result.parse_errors
        );
        let diagnostic = result
            .hir_diagnostics
            .iter()
            .chain(result.type_result.diagnostics.iter())
            .chain(result.analysis_diagnostics.iter())
            .find(|diagnostic| {
                diagnostic.code == code && diagnostic.message.contains(message)
            })
            .unwrap_or_else(|| {
                panic!(
                    "missing {code} containing {message:?}; HIR: {:#?}; type: {:#?}; analysis: {:#?}",
                    result.hir_diagnostics,
                    result.type_result.diagnostics,
                    result.analysis_diagnostics,
                )
            });
        let primary = diagnostic
            .labels
            .iter()
            .find(|label| label.style == type_checker::LabelStyle::Primary)
            .unwrap();
        let actual = &source[usize::from(primary.range.start())..usize::from(primary.range.end())];
        let marker_start = source
            .find(marker)
            .unwrap_or_else(|| panic!("{code}: missing marker {marker:?}"));
        assert!(
            marker.ends_with(expected),
            "{code}: invalid marker {marker:?}"
        );
        let expected_start = marker_start + marker.len() - expected.len();
        let expected_end = expected_start + expected.len();

        assert_eq!(actual, expected, "{code}: {diagnostic:#?}");
        assert_eq!(
            usize::from(primary.range.start()),
            expected_start,
            "{code}: {diagnostic:#?}"
        );
        let lsp = to_lsp(&uri, source, diagnostic.to_ext()).unwrap();
        assert_eq!(
            lsp.range,
            Range::new(
                expected_test_position(source, expected_start),
                expected_test_position(source, expected_end),
            ),
            "{code}: {diagnostic:#?}"
        );
    }
}

#[test]
fn closure_diagnostic_spans_point_at_the_relevant_source() {
    let cases = [
        (
            "E0031",
            "mutable closure",
            "fun main() { let mut total = 0; let add = fun() { total += 1; }; add(); }",
            "add",
            false,
        ),
        (
            "E0100",
            "use of moved value: `once`",
            "struct Token { value: i32 } fun take(value: Token) {} fun main() { let token = Token { value: 1 }; let once = fun() { take(token); }; once(); once(); }",
            "once",
            true,
        ),
        (
            "E0303",
            "assign to `base` while borrowed",
            "fun main() { let mut base = 1; let read = fun() { base }; base = 2; read(); }",
            "base = 2",
            true,
        ),
    ];
    let uri = lsp_types::Url::parse("file:///closure-spans.rid").unwrap();

    for (code, message, source, expected, use_last) in cases {
        let result =
            riddlec::pipeline::compile_with_options(source, CompileOptions { use_std: false });
        let diagnostic = result
            .type_result
            .diagnostics
            .iter()
            .chain(result.analysis_diagnostics.iter())
            .find(|diagnostic| diagnostic.code == code && diagnostic.message.contains(message))
            .unwrap_or_else(|| panic!("missing {code} containing {message:?}"));
        let primary = diagnostic
            .labels
            .iter()
            .find(|label| label.style == type_checker::LabelStyle::Primary)
            .unwrap();
        let start = if use_last {
            source.rfind(expected)
        } else {
            source.find(expected)
        }
        .unwrap();
        let end = start + expected.len();

        assert_eq!(
            &source[usize::from(primary.range.start())..usize::from(primary.range.end())],
            expected,
            "{code}: {diagnostic:#?}"
        );
        let lsp = to_lsp(&uri, source, diagnostic.to_ext()).unwrap();
        assert_eq!(
            lsp.range,
            Range::new(
                expected_test_position(source, start),
                expected_test_position(source, end),
            ),
            "{code}: {diagnostic:#?}"
        );
    }
}

fn expected_test_position(source: &str, offset: usize) -> Position {
    let prefix = &source[..offset];
    let line_start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
    Position::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count() as u32,
        source[line_start..offset].encode_utf16().count() as u32,
    )
}

#[test]
fn project_diagnostics_follow_peer_overlay_removal() {
    let root = temp_root("peer-overlay-removal");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main = root.join("src/main.rid");
    let main_text = "mod util;\nfun main() -> i32 { util::value() }\n".to_string();
    fs::write(&main, &main_text).unwrap();
    let util = root.join("src/util.rid");
    fs::write(&util, "pub fun value() -> i32 { 1 }\n").unwrap();
    let main_uri = lsp_types::Url::from_file_path(&main).unwrap();
    let util_uri = lsp_types::Url::from_file_path(&util).unwrap();
    let mut docs = HashMap::from([
        (
            main_uri.clone(),
            Document {
                text: main_text.clone(),
                version: Some(1),
            },
        ),
        (
            util_uri.clone(),
            Document {
                text: "pub fun other() -> i32 { 1 }\n".into(),
                version: Some(1),
            },
        ),
    ]);

    let mut sessions = DiagnosticSessions::default();
    let stale = collect_workspace_diagnostics_with_sessions(
        &docs,
        CompileOptions::default(),
        &mut sessions,
    )
    .into_iter()
    .find(|published| published.uri == main_uri)
    .unwrap()
    .diagnostics;
    assert!(
        stale
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unresolved")),
        "{stale:#?}"
    );

    docs.remove(&util_uri);
    let refreshed = collect_workspace_diagnostics_with_sessions(
        &docs,
        CompileOptions::default(),
        &mut sessions,
    )
    .into_iter()
    .find(|published| published.uri == main_uri)
    .unwrap()
    .diagnostics;
    assert!(refreshed.is_empty(), "{refreshed:#?}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn parse_args_accepts_no_std() {
    let args = vec!["riddle-lsp".into(), "--no-std".into()];
    let opts = parse_args(&args).unwrap();

    assert!(!opts.compile_options.use_std);
}

#[test]
fn workspace_analysis_can_be_cancelled_between_documents() {
    let docs = HashMap::from([
        (
            lsp_types::Url::parse("untitled:first.rid").unwrap(),
            Document {
                text: "fun first() {}".into(),
                version: Some(1),
            },
        ),
        (
            lsp_types::Url::parse("untitled:second.rid").unwrap(),
            Document {
                text: "fun second() {}".into(),
                version: Some(1),
            },
        ),
    ]);
    let polls = Cell::new(0);
    let result = collect_workspace_diagnostics_cancellable(
        &docs,
        CompileOptions::default(),
        &mut DiagnosticSessions::default(),
        || {
            let next = polls.get() + 1;
            polls.set(next);
            next > 1
        },
    );

    assert!(result.is_none());
    assert_eq!(polls.get(), 2);
}

#[test]
fn workspace_sessions_skip_unchanged_standalone_documents() {
    let first_uri = lsp_types::Url::parse("untitled:first.rid").unwrap();
    let second_uri = lsp_types::Url::parse("untitled:second.rid").unwrap();
    let mut docs = HashMap::from([
        (
            first_uri.clone(),
            Document {
                text: "fun first() {}".into(),
                version: Some(1),
            },
        ),
        (
            second_uri,
            Document {
                text: "fun second() {}".into(),
                version: Some(1),
            },
        ),
    ]);
    let mut sessions = DiagnosticSessions::default();

    collect_workspace_diagnostics_with_sessions(&docs, CompileOptions::default(), &mut sessions);
    assert_eq!(sessions.check_counts(), (2, 0));

    docs.get_mut(&first_uri).unwrap().text = "fun first() { missing; }".into();
    collect_workspace_diagnostics_with_sessions(&docs, CompileOptions::default(), &mut sessions);
    assert_eq!(sessions.check_counts(), (3, 0));
}

#[test]
fn workspace_sessions_skip_projects_for_unrelated_edits() {
    let root = temp_root("project-session-reuse");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main_path = root.join("src/main.rid");
    let main_source = "mod util;\nfun main() { util::value(); }\n";
    fs::write(&main_path, main_source).unwrap();
    let util_path = root.join("src/util.rid");
    fs::write(&util_path, "pub fun value() {}\n").unwrap();
    let main_uri = lsp_types::Url::from_file_path(fs::canonicalize(&main_path).unwrap()).unwrap();
    let scratch_uri = lsp_types::Url::parse("untitled:scratch.rid").unwrap();
    let mut docs = HashMap::from([
        (
            main_uri,
            Document {
                text: main_source.into(),
                version: Some(1),
            },
        ),
        (
            scratch_uri.clone(),
            Document {
                text: "fun scratch() {}".into(),
                version: Some(1),
            },
        ),
    ]);
    let mut sessions = DiagnosticSessions::default();

    collect_workspace_diagnostics_with_sessions(&docs, CompileOptions::default(), &mut sessions);
    assert_eq!(sessions.check_counts(), (1, 1));

    docs.get_mut(&scratch_uri).unwrap().text = "fun scratch() { missing; }".into();
    collect_workspace_diagnostics_with_sessions(&docs, CompileOptions::default(), &mut sessions);
    assert_eq!(sessions.check_counts(), (2, 1));

    fs::write(&util_path, "pub fun value() { missing; }\n").unwrap();
    let published = collect_workspace_diagnostics_with_sessions(
        &docs,
        CompileOptions::default(),
        &mut sessions,
    );
    assert_eq!(sessions.check_counts(), (2, 2));
    let util_uri = lsp_types::Url::from_file_path(fs::canonicalize(&util_path).unwrap()).unwrap();
    assert!(published.iter().any(|item| {
        item.uri == util_uri
            && item
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("unresolved name: `missing`"))
    }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn project_cache_ignores_open_files_outside_the_module_graph() {
    let root = temp_root("unreferenced-project-file");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main_path = root.join("src/main.rid");
    fs::write(&main_path, "fun main() {}\n").unwrap();
    let scratch_path = root.join("src/scratch.rid");
    fs::write(&scratch_path, "fun scratch() {}\n").unwrap();
    let main_uri = lsp_types::Url::from_file_path(fs::canonicalize(&main_path).unwrap()).unwrap();
    let scratch_uri =
        lsp_types::Url::from_file_path(fs::canonicalize(&scratch_path).unwrap()).unwrap();
    let mut docs = HashMap::from([
        (
            main_uri,
            Document {
                text: "fun main() {}\n".into(),
                version: Some(1),
            },
        ),
        (
            scratch_uri.clone(),
            Document {
                text: "fun scratch() {}\n".into(),
                version: Some(1),
            },
        ),
    ]);
    let mut sessions = DiagnosticSessions::default();

    collect_workspace_diagnostics_with_sessions(&docs, CompileOptions::default(), &mut sessions);
    assert_eq!(sessions.check_counts(), (1, 1));

    docs.get_mut(&scratch_uri).unwrap().text = "fun scratch() { missing; }\n".into();
    collect_workspace_diagnostics_with_sessions(&docs, CompileOptions::default(), &mut sessions);
    assert_eq!(sessions.check_counts(), (2, 1));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn watched_topology_and_manifest_changes_reset_analysis_sessions() {
    let rid = lsp_types::Url::parse("file:///workspace/src/main.rid").unwrap();
    let manifest = lsp_types::Url::parse("file:///workspace/Clue.toml").unwrap();

    assert!(!watched_change_resets_sessions(&FileEvent {
        uri: rid.clone(),
        typ: FileChangeType::CHANGED,
    }));
    assert!(watched_change_resets_sessions(&FileEvent {
        uri: rid,
        typ: FileChangeType::CREATED,
    }));
    assert!(watched_change_resets_sessions(&FileEvent {
        uri: manifest,
        typ: FileChangeType::CHANGED,
    }));
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
    let source = r#"extern "C" fun putchar(c: i32) -> i32;

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
                start: 23,
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
        ]
    );
}

#[derive(Debug, PartialEq, Eq)]
struct SemanticTokenPosition {
    line: u32,
    start: u32,
    length: u32,
    token_type: u32,
    token_modifiers_bitset: u32,
}

fn semantic_token_positions(tokens: &SemanticTokens) -> Vec<SemanticTokenPosition> {
    let mut line = 0;
    let mut start = 0;
    tokens
        .data
        .iter()
        .map(|token| {
            line += token.delta_line;
            if token.delta_line == 0 {
                start += token.delta_start;
            } else {
                start = token.delta_start;
            }
            SemanticTokenPosition {
                line,
                start,
                length: token.length,
                token_type: token.token_type,
                token_modifiers_bitset: token.token_modifiers_bitset,
            }
        })
        .collect()
}
