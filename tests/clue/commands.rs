use std::ffi::OsString;
use std::fs;
use std::io::Write;
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
        .env_remove("RIDDLE_TARGET")
        .output()
        .unwrap()
}

fn clue_with_cc(args: &[&str], root: &Path, cc: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clue"))
        .args(args)
        .current_dir(root)
        .env("CC", cc)
        .env_remove("RIDDLE_TARGET")
        .output()
        .unwrap()
}

fn clue_with_target_env(args: &[&str], root: &Path, target: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clue"))
        .args(args)
        .current_dir(root)
        .env("RIDDLE_TARGET", target)
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
    assert!(
        !String::from_utf8_lossy(&build.stdout)
            .to_ascii_lowercase()
            .contains("warning")
            && !String::from_utf8_lossy(&build.stderr)
                .to_ascii_lowercase()
                .contains("warning"),
        "unexpected compiler warning:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(project.join(".clue/build/hello.c").is_file());
    assert!(project.join(".clue/build/hello.runtime.c").is_file());
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
fn explicit_cc_is_strict_and_reports_an_unusable_compiler() {
    let root = temp_root("invalid-cc");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());

    let missing = root.join("missing-cc");
    let build = clue_with_cc(&["build", "app"], &root, &missing);
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(!build.status.success());
    assert!(
        stderr.contains("C compiler from CC") && stderr.contains("could not report its version"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn target_selection_prefers_cli_then_environment_then_manifest() {
    let root = temp_root("target-precedence");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    let project = root.join("app");
    let manifest = project.join("Clue.toml");
    fs::OpenOptions::new()
        .append(true)
        .open(&manifest)
        .unwrap()
        .write_all(b"\n[build]\ntarget = \"unsupported-triple\"\n")
        .unwrap();

    let invalid_manifest = clue(&["check", "app"], &root);
    assert!(!invalid_manifest.status.success());
    assert!(
        String::from_utf8_lossy(&invalid_manifest.stderr).contains("invalid build.target"),
        "{}",
        String::from_utf8_lossy(&invalid_manifest.stderr)
    );

    let host = riddlec::target::TargetTriple::host().unwrap().to_string();
    let environment = clue_with_target_env(&["check", "app"], &root, &host);
    assert!(
        environment.status.success(),
        "{}",
        String::from_utf8_lossy(&environment.stderr)
    );
    let explicit = clue(&["check", "app", "--target", &host], &root);
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cross_build_requires_an_installed_target_component() {
    let root = temp_root("missing-target-component");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    let host = riddlec::target::TargetTriple::host().unwrap();
    let cross = riddlec::target::TargetTriple::ALL
        .into_iter()
        .find(|target| *target != host)
        .unwrap()
        .to_string();

    let output = clue(&["build", "app", "--target", &cross], &root);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&format!(
            "target component `{cross}` is not installed; run `ridup target add {cross}`"
        )),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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

fun apply(f: impl Fn(i32) -> i32, value: i32) -> i32 { f(value) }

fun increment(value: i32) -> i32 { value + 1 }

fun run_mut(mut f: impl FnMut(i32) -> i32, value: i32) -> i32 {
    f(value);
    f(value)
}

fun make_callable(base: i32) -> impl Fn(i32) -> i32 {
    move fun(value: i32) { base + value }
}

fun make_adder(base: i32) -> impl Fn(i32) -> i32 {
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

fun nested(base: i32) -> impl Fn(i32) -> impl Fn(i32) -> i32 {
    fun(first: i32) {
        fun(second: i32) { base + first + second }
    }
}

fun match_capture() -> i32 {
    let read = match 42 { value => fun() { value } };
    read()
}

fun shadowed_pattern_capture() -> i32 {
    match 1 {
        value => {
            let outer = fun() { value };
            match 2 {
                value => {
                    let inner = fun() { value };
                    outer() + inner()
                }
            }
        }
    }
}

fun for_capture() -> i32 {
    let mut total = 0;
    for value in [1, 2, 3] {
        let read = fun() { value };
        total += read();
    }
    total
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
    let mut total = 0;
    let add_total = fun(value: i32) { total += value; total };
    let made = make_callable(40);
    if (*first).value == 42 && (*second).value == 7 && while_sum == 8 && for_sum == 8
        && add(2) == 42 && mutable_capture() == 3 && value_capture() == 7
        && inner(12) == 42 && match_capture() == 42
        && shadowed_pattern_capture() == 3 && for_capture() == 6
        && apply(increment, 1) == 2 && run_mut(add_total, 2) == 4 && made(2) == 42 {
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
    assert!(!generated.contains("struct RgcHeader"));
    let runtime = fs::read_to_string(project.join(".clue/build/app.runtime.c")).unwrap();
    assert!(runtime.contains("struct RgcHeader"));
    assert!(!runtime.contains("GC_MALLOC") && !runtime.contains("<gc.h>"));

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
fn custom_runtime_replaces_the_default_and_invalidates_the_build_cache() {
    if c_compiler().is_none() {
        eprintln!("skipping custom runtime test: no C compiler found");
        return;
    }
    let root = temp_root("custom-runtime");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    let project = root.join("app");
    fs::create_dir_all(project.join("runtime")).unwrap();
    let runtime_path = project.join("runtime/custom.c");
    let runtime = r#"#include <stddef.h>
#include <stdlib.h>

static size_t custom_allocations = 0;

void rgc_init(void *stack_bottom) { (void)stack_bottom; }

void *rgc_alloc(size_t size) {
    void *pointer = malloc(size ? size : 1);
    if (!pointer) exit(EXIT_FAILURE);
    custom_allocations += 1;
    return pointer;
}

void *rgc_realloc(void *pointer, size_t size) {
    void *next = realloc(pointer, size ? size : 1);
    if (!next) exit(EXIT_FAILURE);
    if (!pointer) custom_allocations += 1;
    return next;
}

void rgc_free(void *pointer) { free(pointer); }

void rgc_collect(void) {}

size_t custom_allocation_count(void) { return custom_allocations; }
"#;
    fs::write(&runtime_path, runtime).unwrap();
    let manifest_path = project.join("Clue.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    fs::write(
        &manifest_path,
        format!("{manifest}\n[runtime]\nsource = \"runtime/custom.c\"\n"),
    )
    .unwrap();
    fs::write(
        project.join("src/main.rid"),
        r#"struct Data { value: i32 }

unsafe extern "C" {
    safe fun custom_allocation_count() -> usize;
}

fun escaped() -> &Data {
    let value = Data { value: 42 };
    &value
}

fun main() -> i32 {
    let value = escaped();
    if (*value).value == 42 && custom_allocation_count() > 0 { 0 } else { 1 }
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
    assert!(!project.join(".clue/build/app.runtime.c").exists());
    let generated = fs::read_to_string(project.join(".clue/build/app.c")).unwrap();
    assert!(generated.contains("void *rgc_alloc(size_t size);"));
    assert!(!generated.contains("struct RgcHeader"));
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

    let fresh = clue(&["build", "app"], &root);
    assert!(String::from_utf8_lossy(&fresh.stdout).contains("clue: fresh"));
    fs::write(&runtime_path, format!("{runtime}\n")).unwrap();
    let rebuilt = clue(&["build", "app"], &root);
    assert!(rebuilt.status.success());
    assert!(String::from_utf8_lossy(&rebuilt.stdout).contains("clue: built"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn runtime_source_must_exist() {
    let root = temp_root("missing-runtime");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    let manifest_path = root.join("app/Clue.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    fs::write(
        &manifest_path,
        format!("{manifest}\n[runtime]\nsource = \"missing.c\"\n"),
    )
    .unwrap();

    let check = clue(&["check", "app"], &root);
    let stderr = String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success());
    assert!(
        stderr.contains("runtime source") && stderr.contains("does not exist"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn library_packages_cannot_select_a_runtime() {
    let root = temp_root("library-runtime");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "library", "--lib"], &root).status.success());
    let manifest_path = root.join("library/Clue.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    fs::write(
        &manifest_path,
        format!("{manifest}\n[runtime]\nsource = \"runtime.c\"\n"),
    )
    .unwrap();

    let check = clue(&["check", "library"], &root);
    let stderr = String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success());
    assert!(
        stderr.contains("only supported for binary packages"),
        "{stderr}"
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

fun lambda_ref() -> impl Fn(Data) -> &Data {
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
        r#"use std::io::print;

fun main() -> i32 {
    let value: Option<i32> = Some(2);
    let error: Result<i32, bool> = Err(true);
    print(&(-42));
    print(&0);
    if value.is_some() && value.unwrap_or(0) == 2
        && error.is_err() && error.err().is_some() {
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
    assert!(output.stdout.ends_with(b"-420"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn rust_style_display_fmt_compiles_and_runs() {
    if c_compiler().is_none() {
        eprintln!("skipping Display::fmt runtime test: no C compiler found");
        return;
    }
    let root = temp_root("display-fmt");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::fmt::{Display, Formatter};
use std::io::{print, println};

struct Label {
    text: &str,
}

impl Display for Label {
    fun fmt(&self, formatter: &mut Formatter) -> std::fmt::Result {
        match formatter.write_str(self.text) {
            Ok(()) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fun main() -> i32 {
    let label = Label { text: "value=" };
    print(&label);
    print(&label);
    println(&'中');
    0
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
    assert!(
        output.stdout.ends_with("value=value=中\r\n".as_bytes())
            || output.stdout.ends_with("value=value=中\n".as_bytes())
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn rust_style_print_macros_compile_and_run() {
    if c_compiler().is_none() {
        eprintln!("skipping print macro runtime test: no C compiler found");
        return;
    }
    let root = temp_root("print-macros");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::fmt::{Display, Formatter};

struct Label {
    value: i32,
}

impl Display for Label {
    fun fmt(&self, formatter: &mut Formatter) -> std::fmt::Result {
        self.value.fmt(formatter)
    }
}

fun next(value: &mut i32) -> i32 {
    *value += 1;
    *value
}

fun main() -> i32 {
    print!();
    print!("value={} {{ok}} ", Label { value: 7 });
    let mut calls = 0;
    println!("{} {}", next(&mut calls), next(&mut calls),);
    println!();
    if calls == 2 { 0 } else { 1 }
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
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .replace("\r\n", "\n")
            .ends_with("value=7 {ok} 1 2\n\n")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn standard_debug_derive_compiles_and_runs() {
    if c_compiler().is_none() {
        eprintln!("skipping Debug derive runtime test: no C compiler found");
        return;
    }
    let root = temp_root("debug-derive");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"#[derive(Debug)]
struct Point { x: i32, y: i32 }

#[derive(Debug)]
struct Wrapper<T> { value: T, active: bool }

#[derive(Debug)]
enum Message {
    Quit,
    Move(i32, i32),
    Paint { color: Point, visible: bool },
}

fun main() -> i32 {
    println!("{:?}", Point { x: 3, y: 4 });
    println!("{:?}", Wrapper {
        value: Point { x: 8, y: 9 },
        active: true,
    });
    println!("{:?}", Message::Quit);
    println!("{:?}", Message::Move(10, 20));
    println!("{:?}", Message::Paint {
        color: Point { x: 1, y: 2 },
        visible: false,
    });
    println!("{:?} {:?}", "line\n", '\t');
    0
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
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .replace("\r\n", "\n")
            .ends_with(
                "Point { x: 3, y: 4 }\n\
Wrapper { value: Point { x: 8, y: 9 }, active: true }\n\
Quit\n\
Move(10, 20)\n\
Paint { color: Point { x: 1, y: 2 }, visible: false }\n\
\"line\\n\" '\\t'\n"
            )
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn standard_containers_derive_debug_and_run() {
    if c_compiler().is_none() {
        eprintln!("skipping std container Debug test: no C compiler found");
        return;
    }
    let root = temp_root("std-container-debug");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::collections::{HashMap, HashSet, TreeMap, TreeSet};

fun main() -> i32 {
    let option = Some(1);
    let result: Result<i32, i32> = Ok(2);
    let text = String::from_str("three");

    let mut vector = Vector::new();
    vector.push(4);

    let mut hash_map = HashMap::new();
    hash_map.insert(5, 6);
    let mut hash_set = HashSet::new();
    hash_set.insert(7);

    let mut tree_map = TreeMap::new();
    tree_map.insert(8, 9);
    let mut tree_set = TreeSet::new();
    tree_set.insert(10);

    println!("{:?}", option);
    println!("{:?}", result);
    println!("{:?}", text);
    println!("{:?}", vector);
    println!("{:?}", hash_map);
    println!("{:?}", hash_set);
    println!("{:?}", tree_map);
    println!("{:?}", tree_set);
    0
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "Some(1)",
        "Ok(2)",
        "String { bytes: Vector { data:",
        "Vector { data:",
        "HashMap { buckets:",
        "HashSet { values:",
        "TreeMap { nodes:",
        "TreeSet { values:",
    ] {
        assert!(
            stdout.contains(expected),
            "missing `{expected}` in:\n{stdout}"
        );
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn tree_collections_compile_and_run() {
    if c_compiler().is_none() {
        eprintln!("skipping tree collection runtime test: no C compiler found");
        return;
    }
    let root = temp_root("tree-collections");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::collections::{TreeMap, TreeSet};
use std::io::print;

struct Payload { value: i32 }

impl Drop for Payload {
    fun drop(&mut self) { print(&self.value); }
}

fun main() -> i32 {
    let mut map: TreeMap<i32, i32> = TreeMap::new();
    let mut key = 0;
    while key < 64 {
        map.insert(key, key * 2);
        key += 1;
    }
    key = 127;
    while key >= 64 {
        map.insert(key, key * 2);
        key -= 1;
    }
    map.insert(32, 999);
    if map.len() != 128usize || map.is_empty() { return 1; }
    if !map.contains_key(&0) || !map.contains_key(&127) { return 2; }
    match map.get(&32) {
        Some(value) => { if *value != 999 { return 3; } },
        None => { return 4; },
    }

    let mut set: TreeSet<i32> = TreeSet::new();
    key = 127;
    while key >= 0 {
        set.insert(key % 17);
        key -= 1;
    }
    if set.len() != 17usize || !set.contains(&0) || !set.contains(&16) { return 5; }

    let mut payloads: TreeMap<i32, Payload> = TreeMap::new();
    payloads.insert(1, Payload { value: 10 });
    payloads.insert(1, Payload { value: 20 });
    if payloads.len() != 1usize { return 6; }
    match payloads.get(&1) {
        Some(payload) => { if payload.value != 20 { return 7; } },
        None => { return 8; },
    }
    0
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
    assert!(output.stdout.ends_with(b"1020"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn hash_collections_compile_and_run() {
    if c_compiler().is_none() {
        eprintln!("skipping hash collection runtime test: no C compiler found");
        return;
    }
    let root = temp_root("hash-collections");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::io::print;

struct Key { value: i32 }
struct Payload { value: i32 }

impl Drop for Payload {
    fun drop(&mut self) { print(&self.value); }
}

impl PartialEq for Key {
    fun eq(&self, other: &Self) -> bool { self.value == other.value }
}
impl Eq for Key {}
impl Hash for Key {
    fun hash(&self) -> usize { self.value as usize }
}

fun main() -> i32 {
    let mut map: HashMap<i32, i32> = HashMap::new();
    let mut key = 0;
    while key < 96 {
        map.insert(key * 8, key + 1);
        key += 1;
    }
    map.insert(8, 777);
    if map.len() != 96usize || map.is_empty() { return 1; }
    if !map.contains_key(&0) || !map.contains_key(&(95 * 8)) { return 2; }
    match map.get(&8) {
        Some(value) => { if *value != 777 { return 3; } },
        None => { return 4; },
    }

    let mut set: HashSet<i32> = HashSet::new();
    key = 0;
    while key < 96 {
        set.insert((key % 23) * 8);
        key += 1;
    }
    if set.len() != 23usize || !set.contains(&0) || !set.contains(&(22 * 8)) { return 5; }

    let mut owned: HashMap<Key, Payload> = HashMap::new();
    owned.insert(Key { value: 3 }, Payload { value: 9 });
    owned.insert(Key { value: 3 }, Payload { value: 11 });
    let query = Key { value: 3 };
    if owned.len() != 1usize { return 6; }
    match owned.get(&query) {
        Some(payload) => { if payload.value != 11 { return 7; } },
        None => { return 8; },
    }
    0
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
    assert!(output.stdout.ends_with(b"911"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn deterministic_drop_runs_in_native_binary() {
    if c_compiler().is_none() {
        eprintln!("skipping Drop runtime test: no C compiler found");
        return;
    }
    let root = temp_root("drop-runtime");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

struct Guard { id: i32 }

impl Drop for Guard {
    fun drop(&mut self) {
        print(&self.id);
    }
}

fun main() -> i32 {
    let first = Guard { id: 1 };
    {
        let second = Guard { id: 2 };
    }
    print(&0);
    0
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
    assert!(output.stdout.ends_with(b"201"), "{:?}", output.stdout);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn match_pattern_binding_drops_at_arm_end() {
    if c_compiler().is_none() {
        eprintln!("skipping match Drop runtime test: no C compiler found");
        return;
    }
    let root = temp_root("match-pattern-drop");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

struct Guard { id: i32 }

enum MaybeGuard {
    Some(Guard),
    None,
}

impl Drop for Guard {
    fun drop(&mut self) {
        print(&self.id);
    }
}

fun main() -> i32 {
    let value = MaybeGuard::Some(Guard { id: 1 });
    match value {
        MaybeGuard::Some(guard) => {},
        MaybeGuard::None => {},
    }
    print(&0);
    0
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
    assert!(output.stdout.ends_with(b"10"), "{:?}", output.stdout);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn moved_destructured_binding_is_not_dropped_twice() {
    if c_compiler().is_none() {
        eprintln!("skipping destructuring Drop test: no C compiler found");
        return;
    }
    let root = temp_root("destructured-let-drop");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

struct Guard { id: i32 }

impl Drop for Guard {
    fun drop(&mut self) {
        print(&self.id);
    }
}

fun consume(guard: Guard) {}

fun main() -> i32 {
    let (first, second) = (Guard { id: 1 }, Guard { id: 2 });
    consume(first);
    print(&0);
    0
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
    // `first` is dropped inside `consume`, `second` at the end of `main`.
    assert!(output.stdout.ends_with(b"102"), "{:?}", output.stdout);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn closures_capture_destructured_bindings() {
    if c_compiler().is_none() {
        eprintln!("skipping destructuring capture test: no C compiler found");
        return;
    }
    let root = temp_root("destructured-let-capture");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

fun main() -> i32 {
    let (a, b) = (10, 20);
    let sum = fun() -> i32 { a + b };
    let (mut c, d) = (1, 2);
    let mut bump = fun() { c = c + d; };
    bump();
    bump();
    print(&(sum() + c));
    0
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
    assert!(output.stdout.ends_with(b"35"), "{:?}", output.stdout);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn destructuring_let_binds_tuple_and_struct_elements() {
    if c_compiler().is_none() {
        eprintln!("skipping destructuring runtime test: no C compiler found");
        return;
    }
    let root = temp_root("destructured-let-values");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

struct Point { x: i32, y: i32 }

fun main() -> i32 {
    let ((a, b), c) = ((1, 2), 3);
    let Point { x, y } = Point { x: 10, y: 20 };
    let (mut total, step) = (0, 100);
    total = total + step;
    print(&(a + b + c + x + y + total));
    0
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
    assert!(output.stdout.ends_with(b"136"), "{:?}", output.stdout);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn reference_patterns_match_rust_binding_modes_at_runtime() {
    if c_compiler().is_none() {
        eprintln!("skipping reference pattern runtime test: no C compiler found");
        return;
    }
    let root = temp_root("reference-pattern-values");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

struct Point { x: i32, y: i32 }

enum Maybe {
    Some(i32),
    None,
}

fun escaped_first() -> &i32 {
    let pair = (5, 6);
    let (first, second) = &pair;
    first
}

fun main() -> i32 {
    let mut pair = (1, 2);
    let (left, right) = &mut pair;
    *left = 10;
    *right = 20;
    let &mut mut copy = left;
    copy = 99;
    print(&(*left + *right));

    let point = Point { x: 10, y: 20 };
    let Point { x, y } = &point;
    print(&(*x + *y));

    let maybe = Maybe::Some(7);
    let matched = match &maybe {
        Maybe::Some(value) => *value,
        Maybe::None => 0,
    };
    print(&matched);
    print(&(*escaped_first()));

    let mut original = 3;
    let (&mut copied, plain) = (&mut original, 4);
    original = 5;
    print(&(copied + plain + original));
    0
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
    assert!(output.stdout.ends_with(b"30307512"), "{:?}", output.stdout);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn moved_match_binding_is_not_dropped_twice() {
    if c_compiler().is_none() {
        eprintln!("skipping moved match binding Drop test: no C compiler found");
        return;
    }
    let root = temp_root("moved-match-pattern-drop");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

struct Guard { id: i32 }

enum MaybeGuard {
    Some(Guard),
    None,
}

impl Drop for Guard {
    fun drop(&mut self) {
        print(&self.id);
    }
}

fun consume(guard: Guard) {}

fun main() -> i32 {
    let value = MaybeGuard::Some(Guard { id: 1 });
    match value {
        MaybeGuard::Some(guard) => { consume(guard); },
        MaybeGuard::None => {},
    }
    print(&0);
    0
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
    assert!(output.stdout.ends_with(b"10"), "{:?}", output.stdout);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn match_partial_move_drops_each_field_once() {
    if c_compiler().is_none() {
        eprintln!("skipping match partial-move Drop test: no C compiler found");
        return;
    }
    let root = temp_root("match-partial-move-drop");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

struct Guard { id: i32 }

struct Pair { left: Guard, right: Guard }

impl Drop for Guard {
    fun drop(&mut self) {
        print(&self.id);
    }
}

fun main() -> i32 {
    let pair = Pair {
        left: Guard { id: 1 },
        right: Guard { id: 2 },
    };
    match pair {
        Pair { left } => {}
    }
    print(&pair.right.id);
    print(&0);
    0
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
    assert!(output.stdout.ends_with(b"1202"), "{:?}", output.stdout);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn array_for_break_drops_current_and_remaining_items_once() {
    if c_compiler().is_none() {
        eprintln!("skipping for Drop runtime test: no C compiler found");
        return;
    }
    let root = temp_root("array-for-drop");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

struct Guard { id: i32 }

impl Drop for Guard {
    fun drop(&mut self) {
        print(&self.id);
    }
}

fun main() -> i32 {
    let values = [
        Guard { id: 1 },
        Guard { id: 2 },
        Guard { id: 3 },
    ];
    for guard in values {
        if guard.id == 2 {
            break;
        }
    }
    print(&0);
    0
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
    assert!(output.stdout.ends_with(b"1230"), "{:?}", output.stdout);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn generic_for_break_drops_item_before_iterator() {
    if c_compiler().is_none() {
        eprintln!("skipping generic for Drop runtime test: no C compiler found");
        return;
    }
    let root = temp_root("generic-for-drop");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

struct Guard { id: i32 }
struct Once { yielded: bool }

impl Drop for Guard {
    fun drop(&mut self) {
        print(&self.id);
    }
}

impl Drop for Once {
    fun drop(&mut self) {
        print(&9);
    }
}

impl Iterator for Once {
    type Item = Guard;

    fun next(&mut self) -> Option<Self::Item> {
        if self.yielded {
            Option::None
        } else {
            self.yielded = true;
            Option::Some(Guard { id: 1 })
        }
    }
}

impl IntoIterator for Once {
    type Item = Guard;
    type IntoIter = Once;

    fun into_iter(self) -> Self::IntoIter {
        self
    }
}

fun main() -> i32 {
    let once = Once { yielded: false };
    for guard in once {
        break;
    }
    print(&0);
    0
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
    assert!(output.stdout.ends_with(b"190"), "{:?}", output.stdout);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unsigned_ordering_and_unicode_chars_compile_and_run() {
    if c_compiler().is_none() {
        eprintln!("skipping scalar semantics runtime test: no C compiler found");
        return;
    }
    let root = temp_root("scalar-semantics");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"fun main() -> i32 {
    let high: u8 = 255u8;
    if high > 127u8 && '中' > 'a' { 0 } else { 1 }
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
fn panic_aborts_without_compiler_helper() {
    if c_compiler().is_none() {
        eprintln!("skipping panic runtime test: no C compiler found");
        return;
    }
    let root = temp_root("panic");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        "fun main() { panic(\"boom\"); }\n",
    )
    .unwrap();

    let output = clue(&["run", "app"], &root);
    assert!(!output.status.success(), "{output:#?}");
    assert!(
        output.stderr.is_empty()
            || !String::from_utf8_lossy(&output.stderr).contains("riddle_panic"),
        "{output:#?}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn comparison_operators_dispatch_to_trait_methods() {
    if c_compiler().is_none() {
        eprintln!("skipping comparison operator runtime test: no C compiler found");
        return;
    }
    let root = temp_root("comparison-operators");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::cmp::Ordering;

struct Rank { value: i32 }

impl PartialEq for Rank {
    fun eq(&self, other: &Self) -> bool {
        self.value + other.value == 3
    }
}

impl PartialOrd for Rank {
    fun partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.value == other.value {
            Option::None
        } else if self.value < other.value {
            Option::Some(Ordering::Greater)
        } else {
            Option::Some(Ordering::Less)
        }
    }
}

fun main() -> i32 {
    let low = Rank { value: 1 };
    let high = Rank { value: 2 };
    let same = Rank { value: 1 };
    if !(low == high) { return 1; }
    if low != high { return 2; }
    if !(low > high) { return 3; }
    if !(low >= high) { return 4; }
    if low < high { return 5; }
    if low <= high { return 6; }
    if low < same { return 7; }
    if low <= same { return 8; }
    if low > same { return 9; }
    if low >= same { return 10; }
    0
}
"#,
    )
    .unwrap();

    let output = clue(&["run", "app"], &root);
    assert!(
        output.status.success(),
        "status: {}\nstdout: {}\nstderr: {}",
        output.status,
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
        r#"use std::io::print;

fun main() -> i32 {
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
    {
        let text_view = text.as_str();
        print(&text_view);
    }
    text.clear();
    let text_cleared = text.is_empty() && text.as_str() == "";
    let empty = String::new();
    let empty_view = empty.as_str() == "";
    let unicode = String::from_str("你好");
    let unicode_view = unicode.as_str().as_bytes();
    let unicode_fourth = match unicode_view.get(3usize) {
        Option::Some(value) => *value,
        Option::None => 0u8,
    };
    let unicode_bytes = unicode.len() == 6usize && unicode_fourth == 229u8;

    let mut cleared: Vector<i32> = Vector::new();
    cleared.push(1);
    cleared.clear();
    let vector_cleared = cleared.is_empty();
    cleared.push(2);
    let vector_reused = cleared.pop().unwrap_or(0) == 2 && cleared.is_empty();

    if first == 0 && last == 9 && missing && capacity_grew && sum == 55
        && text_matches && text_cleared && empty_view && unicode_bytes
        && vector_cleared && vector_reused {
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
    assert!(output.stdout.ends_with(b"hello world"), "{output:#?}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn vector_indexing_compiles_and_runs() {
    if c_compiler().is_none() {
        eprintln!("skipping Vector indexing runtime test: no C compiler found");
        return;
    }
    let root = temp_root("vector-indexing");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

fun main() -> i32 {
    let mut values: Vector<i32> = Vector::new();
    values.push(10);
    values.push(20);
    let index = 1;

    if values[0] != 10 { return 1; }
    values[index] = 7;
    values[index] += 5;
    {
        let first = &values[0];
        if *first != 10 { return 2; }
    }
    {
        let second = &mut values[index];
        *second += 1;
    }
    print(&values[index]);
    if values[index] == 13 { 0 } else { 3 }
}
"#,
    )
    .unwrap();

    let output = clue(&["run", "app"], &root);
    assert!(
        output.status.success(),
        "status: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.ends_with(b"13"), "{:?}", output.stdout);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn associated_type_turbofish_compile_and_run() {
    if c_compiler().is_none() {
        eprintln!("skipping associated type turbofish runtime test: no C compiler found");
        return;
    }
    let root = temp_root("associated-type-turbofish");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"struct Marker<T> {
    pointer: *const T,
}

impl<T> Marker<T> {
    fun sizes<U>() -> usize {
        5usize
    }
}

fun main() -> i32 {
    let mut values = Vector::<i32>::new();
    values.push(41);
    let sizes = Marker::<i32>::sizes::<u8>();
    if values.pop().unwrap_or(0) == 41 && sizes == 5usize { 0 } else { 1 }
}
"#,
    )
    .unwrap();

    let output = clue(&["run", "app"], &root);
    assert!(
        output.status.success(),
        "status: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn str_into_iterator_decodes_utf8() {
    if c_compiler().is_none() {
        eprintln!("skipping str iterator runtime test: no C compiler found");
        return;
    }
    let root = temp_root("str-iterator");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"fun main() -> i32 {
    let text: &str = "Aé中🙂";
    let mut index = 0;
    for ch in text {
        if index == 0 && ch != 'A' { return 1; }
        if index == 1 && ch != 'é' { return 2; }
        if index == 2 && ch != '中' { return 3; }
        if index == 3 && ch != '🙂' { return 4; }
        if index > 3 { return 5; }
        index += 1;
    }
    for unused in "" {
        return 6;
    }
    if index == 4 { 0 } else { 7 }
}
"#,
    )
    .unwrap();

    let output = clue(&["run", "app"], &root);
    assert!(
        output.status.success(),
        "status: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn vector_drops_owned_elements() {
    if c_compiler().is_none() {
        eprintln!("skipping Vector Drop runtime test: no C compiler found");
        return;
    }
    let root = temp_root("vector-drop");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"use std::io::print;

struct Guard { id: i32 }

impl Drop for Guard {
    fun drop(&mut self) {
        print(&self.id);
    }
}

fun main() -> i32 {
    {
        let mut values: Vector<Guard> = Vector::new();
        values.push(Guard { id: 1 });
        values.push(Guard { id: 2 });
    }
    print(&0);

    {
        let mut values: Vector<Guard> = Vector::new();
        values.push(Guard { id: 3 });
        values.push(Guard { id: 4 });
        values.clear();
    }
    print(&0);

    {
        let mut values: Vector<Guard> = Vector::new();
        values.push(Guard { id: 5 });
        values.push(Guard { id: 6 });
        values.push(Guard { id: 7 });
        for value in values {
            break;
        }
    }
    print(&0);
    0
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
    assert!(
        output.stdout.ends_with(b"1203405670"),
        "{:?}",
        output.stdout
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn vector_buffer_keeps_escaping_elements_alive_across_collections() {
    if c_compiler().is_none() {
        eprintln!("skipping Vector GC runtime test: no C compiler found");
        return;
    }
    let root = temp_root("vector-gc-trace");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    // The Vector buffer holds the only references to the escaping nodes while
    // `churn` allocates enough garbage to force collections; if the buffer were
    // invisible to the GC the nodes would be swept and the sum would corrupt.
    fs::write(
        root.join("app/src/main.rid"),
        r#"struct Node { value: i32 }

fun make(value: i32) -> &Node {
    let node = Node { value: value };
    &node
}

fun churn() {
    let mut index = 0;
    // rgc_live_bytes counts payload only: 600k x 4-byte nodes = 2.4 MB,
    // comfortably past the 1 MB collection threshold.
    while index < 600000 {
        let garbage = make(index);
        index += 1;
    }
}

fun main() -> i32 {
    let mut nodes: Vector<&Node> = Vector::new();
    let mut index = 0;
    while index < 10 {
        nodes.push(make(index));
        index += 1;
    }
    churn();
    // Reuse freshly swept chunks so stale reads observe overwritten payloads.
    let mut fresh: Vector<&Node> = Vector::new();
    let mut extra = 0;
    while extra < 64 {
        fresh.push(make(1000 + extra));
        extra += 1;
    }
    let mut ok = true;
    let mut expect = 0;
    for node in nodes {
        if node.value != expect {
            ok = false;
        }
        expect += 1;
    }
    if ok { 0 } else { 1 }
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
fn arrays_support_shared_and_mutable_borrowed_iteration() {
    if c_compiler().is_none() {
        eprintln!("skipping borrowed array iterator test: no C compiler found");
        return;
    }
    let root = temp_root("array-borrowed-iter");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "app"], &root).status.success());
    fs::write(
        root.join("app/src/main.rid"),
        r#"fun main() -> i32 {
    let mut values: [i32; 3] = [1, 2, 3];
    let mut sum = 0;
    for value in &values {
        sum += *value;
    }
    for value in &mut values {
        *value += 1;
    }
    if sum == 6 && values[0] == 2 && values[2] == 4 {
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
fn proc_macro_dependency_expands_and_runs() {
    if c_compiler().is_none() {
        eprintln!("skipping proc-macro runtime test: no C compiler found");
        return;
    }
    let root = temp_root("proc-macro");
    fs::create_dir_all(root.join("macros/src")).unwrap();
    fs::create_dir_all(root.join("app/src")).unwrap();
    fs::write(
        root.join("macros/Clue.toml"),
        r#"[package]
name = "macros"

[lib]
path = "src/lib.rid"
proc-macro = true

[dependencies]
"#,
    )
    .unwrap();
    fs::write(
        root.join("macros/src/lib.rid"),
        r####"use std::io::println;

#[proc_macro_derive(Answer, attributes(answer))]
pub fun derive_answer(input: TokenStream) -> TokenStream {
    let mut saw_struct = false;
    let mut saw_name = false;
    let mut saw_field = false;
    let mut saw_colon = false;
    let mut saw_text = false;
    let mut saw_array = false;
    let mut saw_number = false;
    let mut field_token_count_ok = false;
    for tree in &input {
        match tree {
            TokenTree::Ident(ident) => {
                if ident.as_str() == "struct" {
                    saw_struct = ident.span().end() > ident.span().start();
                }
                if ident.as_str() == "Marker" {
                    saw_name = ident.span().start() > 0usize;
                }
            },
            TokenTree::Group(group) => {
                match group.delimiter() {
                    Delimiter::Bracket => {
                        for attribute_tree in group.stream() {
                            match attribute_tree {
                                TokenTree::Group(arguments) => {
                                    for argument in arguments.stream() {
                                        match argument {
                                            TokenTree::Literal(literal) => {
                                                if literal.as_str() == "r#\"token text\"#" {
                                                    saw_text = literal.span().end() > literal.span().start();
                                                }
                                            },
                                            _ => {},
                                        }
                                    }
                                },
                                _ => {},
                            }
                        }
                    },
                    Delimiter::Brace => {
                        field_token_count_ok = group.stream().len() == 3usize;
                        for field_tree in group.stream() {
                            match field_tree {
                                TokenTree::Ident(ident) => {
                                    if ident.as_str() == "value" {
                                        saw_field = true;
                                    }
                                },
                                TokenTree::Punct(punct) => {
                                    if punct.as_char() == ':' {
                                        saw_colon = match punct.spacing() {
                                            Spacing::Alone => true,
                                            Spacing::Joint => false,
                                        };
                                    }
                                },
                                TokenTree::Group(field_type) => {
                                    match field_type.delimiter() {
                                        Delimiter::Bracket => {
                                            saw_array = true;
                                            for type_tree in field_type.stream() {
                                                match type_tree {
                                                    TokenTree::Literal(literal) => {
                                                        if literal.as_str() == "3" {
                                                            saw_number = true;
                                                        }
                                                    },
                                                    _ => {},
                                                }
                                            }
                                        },
                                        _ => {},
                                    }
                                },
                                _ => {},
                            }
                        }
                    },
                    _ => {},
                }
            },
            _ => {},
        }
    }
    if input.len() != 5usize {
        Diagnostic::error(Span::call_site(), "unexpected top-level token count").emit();
        return TokenStream::new();
    }
    if !saw_struct || !saw_name {
        Diagnostic::error(Span::call_site(), "missing item identifiers").emit();
        return TokenStream::new();
    }
    if !saw_field || !saw_colon || !field_token_count_ok {
        Diagnostic::error(Span::call_site(), "missing field tokens").emit();
        return TokenStream::new();
    }
    if !saw_text {
        Diagnostic::error(Span::call_site(), "missing attribute literal").emit();
        return TokenStream::new();
    }
    if !saw_array || !saw_number {
        Diagnostic::error(Span::call_site(), "missing nested type tokens").emit();
        return TokenStream::new();
    }
    let message = "macro log";
    println(&message);
    TokenStream::from_str("fun generated_answer() -> i32 { let text = \"token text\"; 42 }")
        .unwrap_or(TokenStream::new())
}

#[proc_macro]
pub fun answer(input: TokenStream) -> TokenStream {
    if input.to_string().as_str() != "1" {
        Diagnostic::error(Span::call_site(), "function macro received the wrong input").emit();
        return TokenStream::new();
    }
    let mut output = TokenStream::from_str("2").unwrap_or(TokenStream::new());
    let shared = output.clone();
    output.push(TokenTree::Punct(Punct::new(';', Spacing::Alone)));
    if output.len() != 2usize || shared.len() != 1usize {
        Diagnostic::error(Span::call_site(), "TokenStream clone is not copy-on-write").emit();
        return TokenStream::new();
    }
    match TokenStream::from_str("(") {
        Result::Ok(_) => {
            Diagnostic::error(Span::call_site(), "invalid tokens were accepted").emit();
            TokenStream::new()
        },
        Result::Err(_) => shared,
    }
}

#[proc_macro_attribute]
pub fun replace(args: TokenStream, item: TokenStream) -> TokenStream {
    if args.to_string().as_str() != "8" || item.is_empty() {
        Diagnostic::error(Span::call_site(), "attribute macro inputs were not separated").emit();
        return TokenStream::new();
    }
    TokenStream::from_str("fun attribute_answer() -> i32 { 8 }")
        .unwrap_or(TokenStream::new())
}
"####,
    )
    .unwrap();
    fs::write(
        root.join("app/Clue.toml"),
        r#"[package]
name = "app"

[[bin]]
path = "src/main.rid"

[dependencies]
macros = { path = "../macros" }
"#,
    )
    .unwrap();
    fs::write(
        root.join("app/src/main.rid"),
        r####"use macros::{Answer, answer, replace};

#[answer(r#"token text"#)]
#[derive(Answer)]
struct Marker {
    // comments are not token trees
    value: [i32; 3]
}

#[replace(8)]
fun removed() -> i32 { 0 }

fun main() -> i32 {
    if generated_answer() + answer!(1) + attribute_answer() == 52 { 0 } else { 1 }
}
"####,
    )
    .unwrap();

    let output = clue(&["run", "app"], &root);
    assert!(
        output.status.success(),
        "status: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        root.join(if cfg!(windows) {
            "macros/.clue/build/macros.proc-macro-host.exe"
        } else {
            "macros/.clue/build/macros.proc-macro-host"
        })
        .is_file()
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("macro log"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn proc_macro_packages_can_use_function_macros_from_dependencies() {
    if c_compiler().is_none() {
        eprintln!("skipping nested proc-macro test: no C compiler found");
        return;
    }
    let root = temp_root("nested-proc-macro");
    fs::create_dir_all(root.join("quote/src")).unwrap();
    fs::create_dir_all(root.join("macros/src")).unwrap();
    fs::create_dir_all(root.join("app/src")).unwrap();
    fs::write(
        root.join("quote/Clue.toml"),
        r#"[package]
name = "quote"

[lib]
path = "src/lib.rid"
proc-macro = true

[dependencies]
"#,
    )
    .unwrap();
    fs::write(
        root.join("quote/src/lib.rid"),
        r#"#[proc_macro]
pub fun make_answer(input: TokenStream) -> TokenStream {
    TokenStream::from_str("TokenStream::from_str(\"41\").unwrap_or(TokenStream::new())")
        .unwrap_or(TokenStream::new())
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("macros/Clue.toml"),
        r#"[package]
name = "macros"

[lib]
path = "src/lib.rid"
proc-macro = true

[dependencies]
quote = { path = "../quote" }
"#,
    )
    .unwrap();
    fs::write(
        root.join("macros/src/lib.rid"),
        r#"use quote::make_answer;

#[proc_macro]
pub fun answer(input: TokenStream) -> TokenStream { make_answer!() }
"#,
    )
    .unwrap();
    fs::write(
        root.join("app/Clue.toml"),
        r#"[package]
name = "app"

[[bin]]
path = "src/main.rid"

[dependencies]
macros = { path = "../macros" }
"#,
    )
    .unwrap();
    fs::write(
        root.join("app/src/main.rid"),
        "use macros::answer;\nfun main() -> i32 { if answer!() == 41 { 0 } else { 1 } }\n",
    )
    .unwrap();

    for package in ["quote", "macros"] {
        let checked = clue(&["check", package], &root);
        assert!(
            checked.status.success(),
            "checking {package} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&checked.stdout),
            String::from_utf8_lossy(&checked.stderr)
        );
    }

    let output = clue(&["run", "app"], &root);
    assert!(
        output.status.success(),
        "status: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn proc_macro_failures_point_at_the_derive() {
    if c_compiler().is_none() {
        eprintln!("skipping proc-macro failure test: no C compiler found");
        return;
    }
    let root = temp_root("proc-macro-failures");
    fs::create_dir_all(root.join("macros/src")).unwrap();
    fs::create_dir_all(root.join("app/src")).unwrap();
    fs::write(
        root.join("macros/Clue.toml"),
        r#"[package]
name = "macros"

[lib]
path = "src/lib.rid"
proc-macro = true

[dependencies]
"#,
    )
    .unwrap();
    fs::write(
        root.join("macros/src/lib.rid"),
        r#"#[proc_macro_derive(Reject)]
pub fun reject(_input: TokenStream) -> TokenStream {
    let diagnostic = Diagnostic::error(Span::call_site(), "rejected by macro");
    diagnostic.emit();
    TokenStream::new()
}

#[proc_macro_derive(RejectField)]
pub fun reject_field(input: TokenStream) -> TokenStream {
    let mut span = Span::call_site();
    for tree in &input {
        match tree {
            TokenTree::Group(group) => {
                for field in group.stream() {
                    match field {
                        TokenTree::Ident(ident) => {
                            if ident.as_str() == "bad" {
                                span = ident.span();
                            }
                        },
                        _ => {},
                    }
                }
            },
            _ => {},
        }
    }
    Diagnostic::error(span, "rejected field").emit();
    TokenStream::new()
}

#[proc_macro_derive(Invalid)]
pub fun invalid(_input: TokenStream) -> TokenStream {
    TokenStream::from_str("let generated = 1;").unwrap_or(TokenStream::new())
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("app/Clue.toml"),
        r#"[package]
name = "app"

[[bin]]
path = "src/main.rid"

[dependencies]
macros = { path = "../macros" }
"#,
    )
    .unwrap();

    for (derive, expected) in [
        ("Reject", "rejected by macro"),
        ("Invalid", "must contain only top-level items"),
        ("Missing", "unknown proc-macro derive"),
    ] {
        fs::write(
            root.join("app/src/main.rid"),
            format!("#[derive(macros::{derive})]\nstruct Value {{}}\nfun main() -> i32 {{ 0 }}\n"),
        )
        .unwrap();
        let output = clue(&["check", "app"], &root);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{derive} unexpectedly passed");
        assert!(stderr.contains(expected), "{derive}: {stderr}");
        assert!(stderr.contains("main.rid:1"), "{derive}: {stderr}");
    }

    fs::write(
        root.join("app/src/main.rid"),
        "#[derive(macros::RejectField)]\nstruct Value {\n    ok: i32,\n    bad: i32,\n}\nfun main() -> i32 { 0 }\n",
    )
    .unwrap();
    let output = clue(&["check", "app"], &root);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "RejectField unexpectedly passed");
    assert!(stderr.contains("rejected field"), "{stderr}");
    assert!(stderr.contains("main.rid:4"), "{stderr}");
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

#[test]
fn check_enforces_orphan_rules_inside_dependencies() {
    let root = temp_root("dependency-orphan-rule");
    fs::create_dir_all(&root).unwrap();
    assert!(clue(&["new", "base", "--lib"], &root).status.success());
    assert!(clue(&["new", "middle", "--lib"], &root).status.success());
    assert!(clue(&["new", "app"], &root).status.success());

    fs::write(
        root.join("base/src/lib.rid"),
        "pub trait Foreign {}\npub struct External {}\n",
    )
    .unwrap();
    fs::write(
        root.join("middle/Clue.toml"),
        r#"[package]
name = "middle"

[lib]
path = "src/lib.rid"

[dependencies]
base = { path = "../base" }
"#,
    )
    .unwrap();
    fs::write(
        root.join("middle/src/lib.rid"),
        "use base::{Foreign, External};\nimpl Foreign for External {}\n",
    )
    .unwrap();
    fs::write(
        root.join("app/Clue.toml"),
        r#"[package]
name = "app"

[[bin]]
path = "src/main.rid"

[dependencies]
middle = { path = "../middle" }
"#,
    )
    .unwrap();

    let rejected = clue(&["check", "app"], &root);
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(!rejected.status.success());
    assert!(stderr.contains("E0048"), "{stderr}");
    assert!(
        stderr.contains("middle\\src\\lib.rid:2") || stderr.contains("middle/src/lib.rid:2"),
        "{stderr}"
    );

    fs::write(
        root.join("middle/src/lib.rid"),
        "use base::Foreign;\npub struct Local {}\nimpl Foreign for Local {}\n",
    )
    .unwrap();
    let accepted = clue(&["check", "app"], &root);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let _ = fs::remove_dir_all(root);
}
