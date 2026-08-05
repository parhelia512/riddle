use super::*;

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
                text: main_text,
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
fn parse_args_accepts_completion_delay_ms() {
    let args = vec![
        "riddle-lsp".into(),
        "--completion-delay-ms".into(),
        "25".into(),
    ];
    let opts = parse_args(&args).unwrap();

    assert_eq!(opts.completion_delay, Duration::from_millis(25));
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
fn workspace_sessions_observe_project_disk_edits() {
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
    let docs = HashMap::from([(
        main_uri,
        Document {
            text: main_source.into(),
            version: Some(1),
        },
    )]);
    let mut sessions = DiagnosticSessions::default();

    collect_workspace_diagnostics_with_sessions(&docs, CompileOptions::default(), &mut sessions);

    fs::write(&util_path, "pub fun value() { missing; }\n").unwrap();
    let published = collect_workspace_diagnostics_with_sessions(
        &docs,
        CompileOptions::default(),
        &mut sessions,
    );
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
