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
        .chain(
            ["cc", "gcc", "clang", "clang-cl", "cl"]
                .into_iter()
                .map(OsString::from),
        )
        .find(|compiler| {
            let is_msvc = Path::new(compiler)
                .file_stem()
                .is_some_and(|name| name == "cl" || name == "clang-cl");
            Command::new(compiler)
                .arg(if is_msvc { "/?" } else { "--version" })
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

    if c_compiler().is_none() {
        eprintln!("skipping native build assertions: no C compiler found");
        let _ = fs::remove_dir_all(root);
        return;
    }
    let build = clue(&["build", "hello"], &root);
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(project.join(".clue/build/hello.c").is_file());
    assert!(
        project
            .join(if cfg!(windows) {
                ".clue/build/hello.exe"
            } else {
                ".clue/build/hello"
            })
            .is_file()
    );

    let fresh = clue(&["build", "hello"], &root);
    assert!(fresh.status.success());
    assert!(String::from_utf8_lossy(&fresh.stdout).contains("clue: fresh"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn generated_c_with_gc_and_loop_control_compiles_and_runs() {
    if c_compiler().is_none() {
        eprintln!("skipping C runtime test: no cc, gcc, or clang found");
        return;
    }
    let root = temp_root("native-gc");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    let project = root.join("app");
    fs::write(
        project.join("src/main.rid"),
        r#"struct Data { value: i32 }
struct Token { value: i32 }

unsafe extern "C" {
    safe fun rgc_collect();
}

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

    let executable = project.join(if cfg!(windows) {
        ".clue/build/app.exe"
    } else {
        ".clue/build/app"
    });
    assert!(executable.is_file());
    let run = Command::new(&executable).output().unwrap();
    assert!(
        run.status.success(),
        "native program exited with {}",
        run.status
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn generated_c_array_and_by_value_param_refs_compile_and_run() {
    if c_compiler().is_none() {
        eprintln!("skipping C runtime test: no cc, gcc, or clang found");
        return;
    }
    let root = temp_root("native-escaping-places");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    let project = root.join("app");
    fs::write(
        project.join("src/main.rid"),
        r#"
struct Data { value: i32 }

struct Boxed { items: [Data; 2] }

struct Grid { items: [[Data; 3]; 2] }

fun array_ref() -> &Data {
    let items = [Data { value: 9 }, Data { value: 10 }];
    &items[0]
}

fun nested_array_ref() -> &Data {
    let items = [
        [Data { value: 13 }, Data { value: 14 }, Data { value: 15 }],
        [Data { value: 16 }, Data { value: 17 }, Data { value: 18 }],
    ];
    &items[1][2]
}

fun parameter_array_ref(items: [Data; 2]) -> &Data {
    &items[1]
}

fun copy_parameter(items: [Data; 2]) -> i32 {
    let mut copied = items;
    copied[0].value
}

fun field_array_ref() -> &Data {
    let boxed = Boxed {
        items: [Data { value: 19 }, Data { value: 20 }],
    };
    &boxed.items[1]
}

fun nested_field_array_ref() -> &Data {
    let grid = Grid {
        items: [
            [Data { value: 21 }, Data { value: 22 }, Data { value: 23 }],
            [Data { value: 24 }, Data { value: 25 }, Data { value: 26 }],
        ],
    };
    &grid.items[1][2]
}

fun param_ref(value: Data) -> &Data { &value }

fun lambda_ref() -> fun(Data) -> &Data {
    fun(value: Data) -> &Data { &value }
}

fun main() -> i32 {
    let array = array_ref();
    let nested_array = nested_array_ref();
    let parameter_array = parameter_array_ref([
        Data { value: 17 }, Data { value: 18 },
    ]);
    let copied = copy_parameter([Data { value: 27 }, Data { value: 28 }]);
    let field_array = field_array_ref();
    let nested_field_array = nested_field_array_ref();
    let param = param_ref(Data { value: 11 });
    let lambda = lambda_ref();
    let lambda_param = lambda(Data { value: 12 });
    if (*array).value == 9 && (*nested_array).value == 18
        && (*parameter_array).value == 18 && (*field_array).value == 20
        && (*nested_field_array).value == 26 && copied == 27
        && (*param).value == 11 && (*lambda_param).value == 12 {
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
    let executable = project.join(if cfg!(windows) {
        ".clue/build/app.exe"
    } else {
        ".clue/build/app"
    });
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
    assert!(clue(&["build", "library"], &root).status.success());
    assert!(root.join("library/.clue/build/library.c").is_file());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn run_builds_the_binary_and_propagates_its_status() {
    if c_compiler().is_none() {
        eprintln!("skipping clue run test: no C compiler found");
        return;
    }
    let root = temp_root("run");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(root.join("app/src/main.rid"), "fun main() -> i32 { 7 }\n").unwrap();

    let output = clue(&["run", "app", "--", "ignored"], &root);
    assert_eq!(output.status.code(), Some(7), "{output:#?}");
    assert!(
        root.join(if cfg!(windows) {
            "app/.clue/build/app.exe"
        } else {
            "app/.clue/build/app"
        })
        .is_file()
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn standard_library_basics_compile_and_run() {
    if c_compiler().is_none() {
        eprintln!("skipping standard library runtime test: no C compiler found");
        return;
    }
    let root = temp_root("stdlib");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"fun main() -> i32 {
    let value: Option<i32> = Some(2);
    let error: Result<i32, bool> = Err(true);
    let text: &str = "abc";
    if value.is_some() && value.unwrap_or(0) == 2
        && error.is_err() && error.err().is_some()
        && text.len() == 3usize && text.byte_at(1usize).unwrap_or(0u8) == 98u8
        && text.byte_at(3usize).is_none() {
        0
    } else {
        1
    }
}
"#,
    )
    .unwrap();

    let output = clue(&["run", "app"], &root);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn string_and_vector_compile_and_run() {
    if c_compiler().is_none() {
        eprintln!("skipping String and Vector runtime test: no C compiler found");
        return;
    }
    let root = temp_root("string-vector");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"fun main() -> i32 {
    let mut values: Vector<i32> = Vector::new();
    let mut index = 0;
    while index < 10 {
        values.push(index);
        index += 1;
    }

    let fallback = -1;
    let first = *values.get(0usize).unwrap_or(&fallback);
    let missing = values.get(10usize).is_none();
    let mut replacement = -1;
    {
        let second = values.get_mut(1usize).unwrap_or(&mut replacement);
        *second = 20;
    }
    let last = values.pop().unwrap_or(-1);
    let capacity_grew = values.capacity() >= 10usize;
    let mut sum = 0;
    for value in values {
        sum += value;
    }

    let mut text = String::from_str("hello");
    text.push_str(" world");
    let text_matches = text.len() == 11usize && text.as_str() == "hello world";
    text.clear();
    let text_cleared = text.is_empty() && text.as_str() == "";
    let empty = String::new();
    let empty_view = empty.as_str() == "";

    let mut cleared: Vector<i32> = Vector::new();
    cleared.push(1);
    cleared.clear();
    let vector_cleared = cleared.is_empty();

    if first == 0 && last == 9 && missing && capacity_grew && sum == 55
        && text_matches && text_cleared && empty_view && vector_cleared {
        0
    } else {
        1
    }
}
"#,
    )
    .unwrap();

    let output = clue(&["run", "app"], &root);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn build_loads_local_path_dependencies() {
    if c_compiler().is_none() {
        eprintln!("skipping native dependency build test: no C compiler found");
        return;
    }
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
