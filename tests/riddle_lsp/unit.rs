use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "riddle-lsp-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn semantic_tokens(source: &str) -> SemanticTokens {
    semantic_tokens_with_options(source, CompileOptions::default())
}

#[test]
fn position_counts_utf16_columns() {
    let source = "a😀\nb";
    assert_eq!(position(source, 0), Position::new(0, 0));
    assert_eq!(position(source, "a😀".len()), Position::new(0, 3));
    assert_eq!(position(source, source.len()), Position::new(1, 1));
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
fn semantic_tokens_mark_mutable_locals_like_rust_analyzer() {
    let tokens = semantic_tokens("fun main() { let x = 1; x; let mut y = 2; y; }");
    let variables = tokens
        .data
        .iter()
        .filter(|token| token.token_type == TOKEN_VARIABLE)
        .map(|token| token.token_modifiers_bitset)
        .collect::<Vec<_>>();

    assert_eq!(
        variables,
        vec![
            MOD_DECLARATION,
            0,
            MOD_DECLARATION | MOD_MUTABLE,
            MOD_MUTABLE
        ]
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

    let stale =
        collect_document_diagnostics(&main_uri, &main_text, &docs, CompileOptions::default());
    assert!(
        stale
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unresolved")),
        "{stale:#?}"
    );

    docs.remove(&util_uri);
    let refreshed =
        collect_document_diagnostics(&main_uri, &main_text, &docs, CompileOptions::default());
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
