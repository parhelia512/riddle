use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

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

fn clue(args: &[&str], root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clue"))
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

fn c_compiler() -> Option<OsString> {
    std::env::var_os("CC")
        .into_iter()
        .chain(["cc", "gcc", "clang"].into_iter().map(OsString::from))
        .find(|compiler| {
            Command::new(compiler)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
}

#[test]
fn init_creates_a_buildable_binary_project() {
    let root = temp_root("init-build");
    fs::create_dir_all(&root).unwrap();

    let init = clue(&["init", "hello"], &root);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let project = root.join("hello");
    assert!(project.join("src/main.rid").is_file());
    assert!(
        fs::read_to_string(project.join(".gitignore"))
            .unwrap()
            .contains("/.clue")
    );

    let check = clue(&["check", "hello"], &root);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(String::from_utf8_lossy(&check.stdout).contains("clue: checked"));

    let build = clue(&["build", "hello"], &root);
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(project.join(".clue/build/hello.c").is_file());

    let fresh = clue(&["build", "hello"], &root);
    assert!(fresh.status.success());
    assert!(String::from_utf8_lossy(&fresh.stdout).contains("clue: fresh"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn generated_c_with_gc_and_loop_control_compiles_and_runs() {
    let Some(compiler) = c_compiler() else {
        eprintln!("skipping C runtime test: no cc, gcc, or clang found");
        return;
    };
    let root = temp_root("native-gc");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    let project = root.join("app");
    fs::write(
        project.join("src/main.rid"),
        r#"struct Data { value: i32 }
struct Token { value: i32 }

extern "C" fun rgc_collect();

fun escaped(value: i32) -> &Data {
    let local = Data { value };
    &local
}

fun take(token: Token) -> i32 { token.value }

fun make_adder(base: i32) -> fun(i32) -> i32 {
    fun(value: i32) { base + value }
}

fun mutable_capture() -> i32 {
    let mut total = 0;
    let mut add = fun(value: i32) -> i32 {
        total += value;
        total
    };
    add(1);
    add(2)
}

fun value_capture() -> i32 {
    let token = Token { value: 7 };
    let consume = fun() { take(token) };
    consume()
}

fun nested(base: i32) -> fun(i32) -> fun(i32) -> i32 {
    fun(first: i32) {
        fun(second: i32) { base + first + second }
    }
}

fun main() -> i32 {
    let first = escaped(42);
    rgc_collect();
    let second = escaped(7);
    let mut i = 0;
    let mut while_sum = 0;
    while i < 6 {
        i += 1;
        if i == 2 { continue; }
        if i == 5 { break; }
        while_sum += i;
    }
    let mut for_sum = 0;
    for value in [1, 2, 3, 4, 5] {
        if value == 2 { continue; }
        if value == 5 { break; }
        for_sum += value;
    }
    let add = make_adder(40);
    let outer = nested(10);
    let inner = outer(20);
    if (*first).value == 42 && (*second).value == 7 && while_sum == 8 && for_sum == 8
        && add(2) == 42 && mutable_capture() == 3 && value_capture() == 7
        && inner(12) == 42 {
        0
    } else {
        1
    }
}
"#,
    )
    .unwrap();
    let build = clue(&["build", "app"], &root);
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let source = project.join(".clue/build/app.c");
    let generated = fs::read_to_string(&source).unwrap();
    assert!(generated.contains("rgc_alloc"));
    assert!(!generated.contains("GC_MALLOC") && !generated.contains("<gc.h>"));

    let executable = project.join(if cfg!(windows) { "app.exe" } else { "app" });
    let compile = Command::new(&compiler)
        .args(["-std=c11", "-O2"])
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&executable).output().unwrap();
    assert!(
        run.status.success(),
        "native program exited with {}",
        run.status
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn init_refuses_to_overwrite_source() {
    let root = temp_root("no-overwrite");
    fs::create_dir_all(root.join("hello/src")).unwrap();
    fs::write(root.join("hello/src/main.rid"), "keep me").unwrap();

    let output = clue(&["init", "hello"], &root);
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(root.join("hello/src/main.rid")).unwrap(),
        "keep me"
    );
    assert!(!root.join("hello/Clue.toml").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn new_requires_a_missing_destination() {
    let root = temp_root("new");
    fs::create_dir_all(root.join("existing")).unwrap();

    assert!(!clue(&["new", "existing"], &root).status.success());
    assert!(clue(&["new", "library", "--lib"], &root).status.success());
    assert!(root.join("library/src/lib.rid").is_file());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn build_loads_local_path_dependencies() {
    let root = temp_root("dependency");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "math", "--lib"], &root).status.success());
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/Clue.toml"),
        r#"[package]
name = "app"
version = "0.1.0"

[[bin]]
name = "app"
path = "src/main.rid"

[dependencies]
math = { path = "../math" }
"#,
    )
    .unwrap();
    fs::write(
        root.join("app/src/main.rid"),
        "fun main() -> i32 { math::add(1, 2) }\n",
    )
    .unwrap();

    let output = clue(&["build", "app"], &root);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join("app/.clue/build/app.c").is_file());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn check_reports_the_external_module_path() {
    let root = temp_root("module-diagnostic");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        "mod util;\nfun main() -> i32 { util::value() }\n",
    )
    .unwrap();
    fs::write(
        root.join("app/src/util.rid"),
        "pub fun value() -> i32 { missing }\n",
    )
    .unwrap();

    let output = clue(&["check", "app"], &root);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("util.rid:1"), "{stderr}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn check_reports_the_dependency_source_path() {
    let root = temp_root("dependency-diagnostic");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "math", "--lib"], &root).status.success());
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/Clue.toml"),
        r#"[package]
name = "app"

[[bin]]
path = "src/main.rid"

[dependencies]
math = { path = "../math" }
"#,
    )
    .unwrap();
    fs::write(
        root.join("app/src/main.rid"),
        "fun main() -> i32 { math::add(1, 2) }\n",
    )
    .unwrap();
    fs::write(
        root.join("math/src/lib.rid"),
        "pub fun add(x: i32, y: i32) -> i32 { missing }\n",
    )
    .unwrap();

    let output = clue(&["check", "app"], &root);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        stderr.contains("math\\src\\lib.rid:1") || stderr.contains("math/src/lib.rid:1"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(root);
}
