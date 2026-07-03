use clue::manifest::{self, CLUE_PROJECT_FILE_NAME, Dependency, PackageKind};
use clue::package;
use riddlec::pipeline;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use toml::Value;

fn temp_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "riddle-clue-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn init_lib_creates_library_manifest() {
    let root = temp_root("init-lib");
    let project = root.join("hello");
    fs::create_dir_all(&root).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_clue"))
        .args(["init", "--lib"])
        .arg(&project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let manifest = fs::read_to_string(project.join(CLUE_PROJECT_FILE_NAME)).unwrap();
    let value = manifest.parse::<Value>().unwrap();
    assert_eq!(
        value
            .get("package")
            .and_then(Value::as_table)
            .and_then(|package| package.get("entry"))
            .and_then(Value::as_str),
        Some("src/lib.rid")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn init_bin_and_lib_conflict() {
    let root = temp_root("init-conflict");
    fs::create_dir_all(&root).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_clue"))
        .current_dir(&root)
        .args(["init", "--bin", "--lib", "hello"])
        .output()
        .unwrap();
    assert!(!output.status.success());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn builds_init_manifest_from_toml_values() {
    let manifest = manifest::new_manifest("hello", PackageKind::Library);
    let value = manifest.parse::<Value>().unwrap();

    assert_eq!(
        value
            .get("package")
            .and_then(Value::as_table)
            .and_then(|package| package.get("name"))
            .and_then(Value::as_str),
        Some("hello")
    );
    assert_eq!(
        value
            .get("package")
            .and_then(Value::as_table)
            .and_then(|package| package.get("entry"))
            .and_then(Value::as_str),
        Some("src/lib.rid")
    );
    assert!(
        value
            .get("dependencies")
            .and_then(Value::as_table)
            .is_some()
    );
}

#[test]
fn reads_explicit_entry_and_cargo_style_path_dependency() {
    let root = temp_root("read");
    fs::create_dir_all(root.join("src").join("bin")).unwrap();
    fs::write(
        root.join(CLUE_PROJECT_FILE_NAME),
        r#"[package]
name = "app"
entry = "src/bin/app.rid"
version = "0.1.0"

[dependencies]
math = { package = "math-core", path = "../math-core" }
"#,
    )
    .unwrap();
    fs::write(
        root.join("src").join("bin").join("app.rid"),
        "fun main() -> i32 { 0 }",
    )
    .unwrap();

    let manifest = manifest::read(&root, PackageKind::Binary).unwrap();
    assert_eq!(manifest.name, "app");
    assert_eq!(manifest.entry, root.join("src").join("bin").join("app.rid"));
    assert_eq!(
        manifest.dependencies,
        vec![Dependency {
            alias: "math".to_string(),
            package: "math-core".to_string(),
            path: PathBuf::from("../math-core"),
        }]
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn rejects_version_dependency() {
    let root = temp_root("version");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join(CLUE_PROJECT_FILE_NAME),
        r#"[package]
name = "app"

[dependencies]
math = "1.0"
"#,
    )
    .unwrap();
    fs::write(root.join("src").join("main.rid"), "fun main() -> i32 { 0 }").unwrap();

    let error = manifest::read(&root, PackageKind::Binary).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert!(error.to_string().contains("local path dependency"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn fingerprints_parsed_toml_not_formatting() {
    let root = temp_root("fingerprint");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join(CLUE_PROJECT_FILE_NAME),
        r#"# comment
[package]
name = "app"

[dependencies]
"#,
    )
    .unwrap();
    fs::write(root.join("src").join("main.rid"), "fun main() -> i32 { 0 }").unwrap();

    let first = manifest::read(&root, PackageKind::Binary)
        .unwrap()
        .fingerprint;
    fs::write(
        root.join(CLUE_PROJECT_FILE_NAME),
        r#"[package]
name = "app" # comment

[dependencies]
"#,
    )
    .unwrap();
    let second = manifest::read(&root, PackageKind::Binary)
        .unwrap()
        .fingerprint;

    assert_eq!(first, second);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn wraps_path_dependency_as_module() {
    let root = temp_root("dependency");
    let app = root.join("app");
    let math = root.join("math-core");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(math.join("src")).unwrap();
    fs::write(
        app.join(CLUE_PROJECT_FILE_NAME),
        r#"[package]
name = "app"
version = "0.1.0"

[dependencies]
math = { package = "math-core", path = "../math-core" }
"#,
    )
    .unwrap();
    fs::write(
        math.join(CLUE_PROJECT_FILE_NAME),
        r#"[package]
name = "math-core"
version = "0.1.0"
"#,
    )
    .unwrap();
    fs::write(
        app.join("src").join("main.rid"),
        "fun main() -> i32 { math::one() }",
    )
    .unwrap();
    fs::write(
        math.join("src").join("lib.rid"),
        "pub fun one() -> i32 { 1 }",
    )
    .unwrap();

    let loaded = package::load(&app).unwrap();
    assert_eq!(loaded.name, "app");
    assert!(loaded.source.source.contains("mod math {"));
    assert!(loaded.source.source.contains("fun one() -> i32 { 1 }"));
    assert!(pipeline::compile(&loaded.source.source).success());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn dependency_prefers_lib_entry() {
    let root = temp_root("lib-entry");
    let app = root.join("app");
    let math = root.join("math");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(math.join("src")).unwrap();
    fs::write(
        app.join(CLUE_PROJECT_FILE_NAME),
        r#"[package]
name = "app"

[dependencies]
math = { path = "../math" }
"#,
    )
    .unwrap();
    fs::write(
        math.join(CLUE_PROJECT_FILE_NAME),
        r#"[package]
name = "math"
"#,
    )
    .unwrap();
    fs::write(
        app.join("src").join("main.rid"),
        "fun main() -> i32 { math::one() }",
    )
    .unwrap();
    fs::write(
        math.join("src").join("main.rid"),
        "pub fun one() -> i32 { 2 }",
    )
    .unwrap();
    fs::write(
        math.join("src").join("lib.rid"),
        "pub fun one() -> i32 { 1 }",
    )
    .unwrap();

    let loaded = package::load(&app).unwrap();
    assert!(loaded.source.source.contains("fun one() -> i32 { 1 }"));
    assert!(!loaded.source.source.contains("fun one() -> i32 { 2 }"));

    let _ = fs::remove_dir_all(root);
}
