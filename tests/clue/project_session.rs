use clue::{ProjectSession, resolve_project_with_session};
use riddlec::pipeline::CompileOptions;
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "clue-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn project_session_drops_removed_overlays() {
    let root = temp_root("removed-overlay");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"cached\"\n\n[dependencies]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.rid"),
        "mod util; fun main() -> i32 { util::value() }\n",
    )
    .unwrap();
    let util = root.join("src/util.rid");
    fs::write(&util, "pub fun value() -> i32 { 1 }\n").unwrap();
    let mut session = ProjectSession::default();
    let stale = HashMap::from([(util, "pub fun other() -> i32 { 1 }\n".to_string())]);

    let first =
        resolve_project_with_session(&root, &stale, CompileOptions::default(), &mut session)
            .unwrap();
    assert!(first.result.hir_diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("unresolved name: `util::value`")
    }));

    let second = resolve_project_with_session(
        &root,
        &HashMap::new(),
        CompileOptions::default(),
        &mut session,
    )
    .unwrap();
    assert!(second.result.hir_diagnostics.iter().all(|diagnostic| {
        !diagnostic
            .message
            .contains("unresolved name: `util::value`")
    }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn project_accepts_an_unsaved_module_overlay() {
    let root = temp_root("unsaved-module");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"overlay\"\n\n[dependencies]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.rid"),
        "mod fresh; fun main() -> i32 { fresh::value() }\n",
    )
    .unwrap();
    let fresh = root.join("src/fresh.rid");
    let overlays = HashMap::from([(fresh, "pub fun value() -> i32 { 7 }\n".to_string())]);

    let analysis = resolve_project_with_session(
        &root,
        &overlays,
        CompileOptions::default(),
        &mut ProjectSession::default(),
    )
    .unwrap();

    assert!(
        analysis.result.success(),
        "{:#?}",
        analysis.result.hir_diagnostics
    );
    let _ = fs::remove_dir_all(root);
}
