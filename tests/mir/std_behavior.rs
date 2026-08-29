//! End-to-end behavior tests for the standard library surface: collection
//! `remove`, the iterator combinator functions, `?` with `From`/`Option`, and
//! `std::fs` file I/O. Each test compiles a Riddle program to C, links it with
//! the runtime, runs it, and asserts on the exit code and output.

use riddlec::pipeline;
use std::{fs, path::Path, process::Command};

/// Compiles `source` with the full pipeline, emits C, builds it with the
/// system C compiler plus the selected runtime, and runs it. Returns the
/// exit code and captured stdout.
fn compile_and_run(source: &str, gc: bool) -> (i32, String) {
    let result = pipeline::compile(source);
    assert!(
        result.success(),
        "riddle diagnostics: {:#?}",
        result.type_result.diagnostics
    );
    let generated = pipeline::generate_c_with_gc_and_source(
        result.mir_module.as_ref().unwrap(),
        gc,
        "src/main.rid",
    )
    .unwrap();

    let runtime = if gc {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/crates/gc/src/runtime.c"
        ))
    } else {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/crates/gc/src/no_gc_runtime.c"
        ))
    };
    let args_runtime = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/gc/src/args_runtime.c"
    ));

    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let compiler_name = Path::new(&compiler)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let is_msvc = compiler_name == "cl";

    let dir = std::env::temp_dir().join(format!(
        "riddle-std-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let c_source = dir.join("main.c");
    let executable = dir.join(if cfg!(windows) { "main.exe" } else { "main" });
    fs::write(&c_source, format!("{generated}\n{runtime}\n{args_runtime}")).unwrap();

    let mut command = Command::new(&compiler);
    if is_msvc {
        command
            .args(["/std:c11", "/W4"])
            .arg(&c_source)
            .arg(format!("/Fe{}", executable.display()));
    } else {
        command
            .args(["-std=c11"])
            .arg(&c_source)
            .arg("-o")
            .arg(&executable);
    }
    let compile_output = command.output().unwrap();
    assert!(
        compile_output.status.success(),
        "C compile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile_output.stdout),
        String::from_utf8_lossy(&compile_output.stderr)
    );

    let run = Command::new(&executable)
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let _ = fs::remove_dir_all(&dir);
    (run.status.code().unwrap_or(-1), stdout)
}

#[test]
fn std_collections_remove_elements_and_keep_lookups() {
    let (code, stdout) = compile_and_run(
        r#"
        use crate::std::collections::{HashMap, HashSet, TreeMap, TreeSet};

        fun main() -> i32 {
            let mut map: HashMap<i32, i32> = HashMap::new();
            map.insert(1i32, 10i32);
            map.insert(2i32, 20i32);
            map.insert(3i32, 30i32);
            let removed = map.remove(&2i32);
            let gone = map.remove(&2i32);
            let map_ok = map.len() == 2usize
                && map.contains_key(&1i32)
                && map.contains_key(&3i32)
                && !map.contains_key(&2i32)
                && removed.unwrap_or(0i32) == 20i32
                && gone.is_none();
            if !map_ok { return 1; }

            // Removing through a probe cluster keeps later lookups working.
            map.insert(4i32, 40i32);
            map.remove(&1i32);
            map.insert(5i32, 50i32);
            if map.get(&3i32).is_none() || map.get(&4i32).is_none() || map.get(&5i32).is_none() {
                return 2;
            }

            let mut set: HashSet<i32> = HashSet::new();
            set.insert(5i32);
            set.insert(6i32);
            if !set.remove(&5i32) || set.contains(&5i32) || !set.contains(&6i32) {
                return 3;
            }

            let mut tree: TreeMap<i32, i32> = TreeMap::new();
            tree.insert(1i32, 10i32);
            tree.insert(2i32, 20i32);
            tree.insert(3i32, 30i32);
            tree.insert(4i32, 40i32);
            if tree.remove(&2i32).unwrap_or(0i32) != 20i32 {
                return 4;
            }
            if tree.len() != 3usize
                || !tree.contains_key(&1i32)
                || !tree.contains_key(&3i32)
                || !tree.contains_key(&4i32)
                || tree.contains_key(&2i32)
            {
                return 5;
            }
            // The tree stays usable for lookups and further inserts.
            tree.insert(5i32, 50i32);
            if tree.get(&5i32).is_none() || tree.remove(&9i32).is_some() {
                return 6;
            }

            let mut tset: TreeSet<i32> = TreeSet::new();
            tset.insert(7i32);
            tset.insert(8i32);
            if !tset.remove(&7i32) || tset.contains(&7i32) || !tset.contains(&8i32) {
                return 7;
            }
            0
        }
        "#,
        true,
    );
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn std_iterator_combinators_and_eager_maps_run() {
    let (code, stdout) = compile_and_run(
        r#"
        use crate::std::iter::{Iterator, IntoIterator, map_into, filter_into};

        struct Counter {
            index: usize,
            limit: usize,
        }

        impl Iterator for Counter {
            type Item = i32;

            fun next(&mut self) -> Option<i32> {
                if self.index < self.limit {
                    self.index += 1usize;
                    Option::Some(self.index as i32)
                } else {
                    Option::None
                }
            }
        }

        fun main() -> i32 {
            let counter = Counter { index: 0usize, limit: 5usize };
            let sum = counter.fold(0i32, fun(acc: i32, value: i32) -> i32 { acc + value });
            if sum != 15i32 { return 1; }

            let mut counter2 = Counter { index: 0usize, limit: 5usize };
            if counter2.count() != 5usize { return 2; }

            let mut counter3 = Counter { index: 0usize, limit: 5usize };
            if counter3.nth(2usize).unwrap_or(0i32) != 3i32 { return 3; }

            let mut counter4 = Counter { index: 0usize, limit: 5usize };
            if !counter4.all(fun(v: i32) -> bool { v > 0i32 }) { return 4; }

            let mut counter5 = Counter { index: 0usize, limit: 5usize };
            if !counter5.any(fun(v: i32) -> bool { v > 3i32 }) { return 5; }

            let mut counter6 = Counter { index: 0usize, limit: 5usize };
            if counter6.find(fun(v: &i32) -> bool { *v == 4i32 }).unwrap_or(0i32) != 4i32 {
                return 6;
            }

            let mut counter7 = Counter { index: 0usize, limit: 5usize };
            if counter7.position(fun(v: &i32) -> bool { *v == 4i32 }).unwrap_or(9usize) != 3usize {
                return 7;
            }

            let mut counter8 = Counter { index: 0usize, limit: 4usize };
            let doubled = map_into(&mut counter8, fun(v: i32) -> i32 { v * 2i32 });
            let mut total = 0i32;
            for value in doubled {
                total += value;
            }
            if total != 20i32 { return 8; }

            let mut counter9 = Counter { index: 0usize, limit: 6usize };
            let evens = filter_into(&mut counter9, fun(v: &i32) -> bool { *v % 2i32 == 0i32 });
            let mut even_total = 0i32;
            for value in evens {
                even_total += value;
            }
            if even_total != 12i32 { return 9; }

            let mut counter10 = Counter { index: 0usize, limit: 4usize };
            let collected = Vector::from_iterator(&mut counter10);
            if collected.len() != 4usize { return 10; }

            // Lazy map / filter with chaining.
            let mapped = Counter { index: 0usize, limit: 4usize }
                .map(fun(v: i32) -> i32 { v * 10i32 });
            let mut mapped_total = 0i32;
            for value in mapped {
                mapped_total += value;
            }
            if mapped_total != 100i32 { return 11; }

            let chained = Counter { index: 0usize, limit: 5usize }
                .map(fun(v: i32) -> i32 { v + 1i32 })
                .filter(fun(v: &i32) -> bool { *v > 3i32 });
            let mut chained_total = 0i32;
            for value in chained {
                chained_total += value;
            }
            if chained_total != 15i32 { return 12; }
            0
        }
        "#,
        true,
    );
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn std_bracket_lambdas_run() {
    let (code, stdout) = compile_and_run(
        r#"
        use crate::std::iter::{Iterator, IntoIterator};

        struct Counter {
            index: usize,
            limit: usize,
        }

        impl Iterator for Counter {
            type Item = i32;

            fun next(&mut self) -> Option<i32> {
                if self.index < self.limit {
                    self.index += 1usize;
                    Option::Some(self.index as i32)
                } else {
                    Option::None
                }
            }
        }

        fun invoke(action: impl Fn() -> i32) -> i32 { action() }

        fun main() -> i32 {
            // Bracket lambda passed inside the argument list; parameter
            // types inferred from the expected callable signature.
            let sum = Counter { index: 0usize, limit: 5usize }
                .fold(0i32, [acc, v -> acc + v]);
            if sum != 15i32 { return 1; }

            // `it` convention on a reference parameter.
            let mut counter2 = Counter { index: 0usize, limit: 5usize };
            if counter2.find([it -> *it == 4i32]).unwrap_or(0i32) != 4i32 { return 2; }

            // Lazy chain with trailing method bracket lambdas.
            let chained = Counter { index: 0usize, limit: 5usize }
                .map [v -> v + 1i32]
                .filter [it -> *it > 3i32];
            let mut total = 0i32;
            for value in chained {
                total += value;
            }
            if total != 15i32 { return 3; }

            // Zero-parameter lambda with move capture.
            let base = 10i32;
            let offset = move [ -> base + 5i32];
            if invoke(offset) != 15i32 { return 4; }

            // Bracket lambda bound to a variable and called directly.
            let double = [it -> it * 2i32];
            if double(21i32) != 42i32 { return 5; }

            // Mutable capture through a zero-parameter bracket lambda (FnMut).
            let mut count = 0i32;
            let mut bump = [ -> { count += 1i32; count }];
            bump();
            bump();
            if count != 2i32 { return 6; }

            0
        }
        "#,
        true,
    );
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn std_question_operator_supports_from_and_option() {
    let (code, stdout) = compile_and_run(
        r#"
        use crate::std::convert::From;
        use crate::std::option::Option;
        use crate::std::result::Result;

        enum ParseError {
            Empty,
        }

        enum AppError {
            Wrapped(ParseError),
        }

        impl From<ParseError> for AppError {
            fun from(value: ParseError) -> AppError {
                AppError::Wrapped(value)
            }
        }

        fun parse(flag: bool) -> Result<i32, ParseError> {
            if flag {
                Result::Ok(40i32)
            } else {
                Result::Err(ParseError::Empty)
            }
        }

        fun run(flag: bool) -> Result<i32, AppError> {
            let value = parse(flag)?;
            Result::Ok(value + 2i32)
        }

        fun find(flag: bool) -> Option<i32> {
            if flag {
                Option::Some(8i32)
            } else {
                Option::None
            }
        }

        fun run_option(flag: bool) -> Option<i32> {
            let value = find(flag)?;
            Option::Some(value * 3i32)
        }

        fun main() -> i32 {
            let a = run(true).unwrap_or(0i32);
            let b = run_option(true).unwrap_or(0i32);
            if a == 42i32 && b == 24i32 {
                0
            } else {
                1
            }
        }
        "#,
        true,
    );
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn std_fs_roundtrips_file_content() {
    let source = r#"
        use crate::std::fs::{read_to_string, write, FsFile};
        use crate::std::result::Result;

        fun main() -> i32 {
            match write("riddle_fs_e2e.tmp", "hello fs") {
                Result::Ok(()) => {},
                Result::Err(_) => { return 1; },
            }
            let content = match read_to_string("riddle_fs_e2e.tmp") {
                Result::Ok(text) => text,
                Result::Err(_) => { return 2; },
            };
            if content.len() != 8usize {
                return 3;
            }
            match FsFile::open("riddle_fs_e2e.tmp") {
                Result::Ok(mut file) => {
                    let mut buffer = [0u8; 16];
                    let read = file.read(&mut buffer).unwrap_or(0usize);
                    if read != 8usize {
                        return 4;
                    }
                    if buffer[0usize] != 104u8 {
                        return 5;
                    }
                },
                Result::Err(_) => { return 6; },
            }
            0
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
}
