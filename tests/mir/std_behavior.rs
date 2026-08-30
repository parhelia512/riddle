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
            .arg(format!("/Fo{}.obj", executable.display()))
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
fn std_iterator_lazy_helpers_chain_and_short_circuit() {
    let (code, stdout) = compile_and_run(
        r#"
        use crate::std::iter::{Iterator, IntoIterator};

        fun main() -> i32 {
            let chained = crate::std::ops::range(0, 3).into_iter()
                .chain(crate::std::ops::range(3, 5).into_iter());
            let chained_total = chained.fold(0i32, fun(acc: i32, value: i32) -> i32 {
                acc + value
            });
            if chained_total != 10i32 { return 1; }

            let taken = crate::std::ops::range(0, 10).into_iter()
                .take_while(fun(value: &i32) -> bool { *value < 4i32 });
            if taken.fold(0i32, fun(acc: i32, value: i32) -> i32 {
                acc + value
            }) != 6i32 { return 2; }

            let skipped = crate::std::ops::range(0, 6).into_iter()
                .skip_while(fun(value: &i32) -> bool { *value < 3i32 });
            if skipped.fold(0i32, fun(acc: i32, value: i32) -> i32 {
                acc + value
            }) != 12i32 { return 3; }

            let mut inspected_total = 0i32;
            let inspected = crate::std::ops::range(1, 4).into_iter()
                .inspect(fun(value: &i32) -> () { inspected_total += *value; });
            let copied_total = inspected.fold(0i32, fun(acc: i32, value: i32) -> i32 {
                acc + value
            });
            if inspected_total != 6i32 || copied_total != 6i32 { return 4; }
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

#[test]
fn std_option_result_combinators_compile_and_run() {
    let source = r#"
        use crate::std::option::Option;
        use crate::std::result::Result;

        enum AppError {
            Bad,
        }

        fun main() -> i32 {
            let fallback = Option::Some(5i32).unwrap_or_else([ -> 0i32 ]);
            let none: Option<i32> = Option::None;
            let mapped = none.map_or(9i32, [v -> v * 2i32]);
            let recovered = none.or_else([ -> Option::Some(3i32) ]).unwrap();
            let chained = Option::Some(1i32).and(Option::Some(2i32)).unwrap();
            let converted = match Result::Err(AppError::Bad).map_err([_ -> 7i32]) {
                Result::Ok(_) => 0i32,
                Result::Err(code) => code,
            };
            let errored = Result::Ok(4i32).map_or(0i32, [v -> v + 1i32]);
            if fallback == 5i32 && mapped == 9i32 && recovered == 3i32
                && chained == 2i32 && converted == 7i32 && errored == 5i32
            { 0 } else { 1 }
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn range_syntax_iterates_exclusive_and_inclusive() {
    let source = r#"
        fun main() -> i32 {
            let mut total = 0;
            for i in 0..5 {
                total += i;
            }
            for i in 0..=4 {
                total += i * 10;
            }
            let mut parts = 0;
            for i in 1 + 1..4 + 1 {
                parts += i;
            }
            let mut empty = 0;
            for _i in 5..5 {
                empty += 1;
            }
            let mut single = 0;
            for i in 3..=3 {
                single = i;
            }
            if total == 110 && parts == 9 && empty == 0 && single == 3 { 0 } else { 1 }
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn string_split_replace_and_ascii_case_roundtrip() {
    let source = r#"
        use crate::std::string::String;

        fun main() -> i32 {
            let csv = String::from_str("alpha,beta,,gamma");
            let parts = csv.split(",");
            if parts.len() != 4usize { return 1; }
            if parts.get(0usize).unwrap_or(&String::new()).as_str() != "alpha" { return 2; }
            if parts.get(2usize).unwrap_or(&String::new()).as_str() != "" { return 3; }
            if csv.replace(",", ";").as_str() != "alpha;beta;;gamma" { return 4; }
            if String::from_str("no-sep").replace(",", "x").as_str() != "no-sep" { return 5; }
            if String::from_str("MixEd123!").to_ascii_uppercase().as_str() != "MIXED123!" { return 6; }
            if String::from_str("MixEd123!").to_ascii_lowercase().as_str() != "mixed123!" { return 7; }
            0
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn vector_insert_remove_sort_contains_retain_roundtrip() {
    let source = r#"
        use crate::std::vector::Vector;

        fun main() -> i32 {
            let mut v = Vector::new();
            v.push(3i32);
            v.push(1i32);
            v.insert(1usize, 2i32);
            if *v.get(0usize).unwrap_or(&0) != 3i32 { return 1; }
            if *v.get(1usize).unwrap_or(&0) != 2i32 { return 2; }
            if *v.get(2usize).unwrap_or(&0) != 1i32 { return 3; }
            v.sort();
            if *v.get(0usize).unwrap_or(&0) != 1i32 { return 4; }
            if *v.get(2usize).unwrap_or(&0) != 3i32 { return 5; }
            if !v.contains(&2i32) { return 6; }
            let removed = v.remove(0usize);
            if removed != 1i32 || v.len() != 2usize { return 7; }
            v.retain([x -> *x >= 2i32]);
            if v.len() != 2usize { return 8; }
            0
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn iterator_collect_skip_min_max_roundtrip() {
    let source = r#"
        use crate::std::iter::{Iterator, IntoIterator};

        fun main() -> i32 {
            let collected = crate::std::ops::range(0, 10).collect();
            if collected.len() != 10usize { return 1; }
            if *collected.get(3usize).unwrap_or(&0) != 3i32 { return 2; }
            let mut skipped_sum = 0;
            for value in crate::std::iter::skip(crate::std::ops::range(0, 10).into_iter(), 8usize) {
                skipped_sum += value;
            }
            if skipped_sum != 17 { return 3; }
            let mut taken = crate::std::iter::take(crate::std::ops::range(0, 10).into_iter(), 2usize);
            let taken_count = taken.count();
            if taken_count != 2usize { return 4; }
            let smallest = crate::std::iter::min(crate::std::ops::range(0, 10).collect().into_iter());
            match smallest {
                Option::Some(0i32) => {},
                _ => { return 5; },
            }
            let largest = crate::std::iter::max(crate::std::ops::range(0, 10).collect().into_iter());
            match largest {
                Option::Some(9i32) => {},
                _ => { return 6; },
            }
            0
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn hash_map_get_or_insert_counts_once() {
    let source = r#"
        use crate::std::collections::hash_map::HashMap;

        fun main() -> i32 {
            let mut counts = HashMap::new();
            let slot = counts.get_or_insert(7i32, 0i32);
            *slot += 1i32;
            let slot2 = counts.get_or_insert(7i32, 100i32);
            if *slot2 != 1i32 { return 1; }
            if !counts.contains_key(&7i32) { return 2; }
            if counts.len() != 1usize { return 3; }
            0
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn parse_wide_integers_and_radix_roundtrip() {
    let source = r#"
        use crate::std::parse::{parse_i64, parse_u64, parse_usize, parse_with_radix};
        use crate::std::option::Option;

        fun main() -> i32 {
            match parse_i64("-9223372036854775808") {
                Option::Some(v) => { if v != -9223372036854775807i64 - 1i64 { return 1; } },
                Option::None => { return 2; },
            }
            match parse_i64("9223372036854775808") {
                Option::None => {},
                Option::Some(_) => { return 3; },
            }
            match parse_u64("18446744073709551615") {
                Option::Some(v) => { if v != 18446744073709551615u64 { return 4; } },
                Option::None => { return 5; },
            }
            match parse_with_radix("ff", 16) {
                Option::Some(v) => { if v != 255i64 { return 6; } },
                Option::None => { return 7; },
            }
            match parse_with_radix("-2a", 16) {
                Option::Some(v) => { if v != -42i64 { return 8; } },
                Option::None => { return 9; },
            }
            match parse_with_radix("1010", 2) {
                Option::Some(v) => { if v != 10i64 { return 10; } },
                Option::None => { return 11; },
            }
            match parse_with_radix("1", 37) {
                Option::None => {},
                Option::Some(_) => { return 12; },
            }
            match parse_usize("12345") {
                Option::Some(v) => { if v != 12345usize { return 13; } },
                Option::None => { return 14; },
            }
            0
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn format_placeholders_positional_named_and_debug() {
    let source = r#"
        fun main() -> i32 {
            let name = "riddle";
            let version = 42i32;
            println!("hello {name} v{version}!");
            println!("{0} then {1} then {0} and {}", 1i32, 2i32, 3i32);
            println!("debug: {version:?} hex {}", 7i32);
            0
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains("hello riddle v42!"), "stdout: {stdout}");
    assert!(stdout.contains("1 then 2 then 1 and 1"), "stdout: {stdout}");
    assert!(stdout.contains("debug: 42 hex 7"), "stdout: {stdout}");
}

#[test]
fn generic_bound_dispatch_compiles_and_runs() {
    // Regression: trait-method calls on a generic `T` inside a generic
    // function used to miscompile when the operand was a match payload
    // binding (its storage was lazily materialized inside one arm and read
    // from sibling arms that never executed it).
    let source = r#"
        use crate::std::iter::{Iterator, IntoIterator};
        use crate::std::option::Option;

        fun pick<I, T>(mut iterator: I) -> Option<T>
        where I: Iterator<Item = T>,
              T: crate::std::cmp::PartialOrd {
            let mut best: Option<T> = Option::None;
            loop {
                match iterator.next() {
                    Option::Some(value) => {
                        match best {
                            Option::Some(current) => {
                                let is_smaller = value.lt(&current);
                                if is_smaller {
                                    best = Option::Some(value);
                                }
                            },
                            Option::None => { best = Option::Some(value); },
                        }
                    },
                    Option::None => { break; },
                }
            }
            best
        }

        fun compare_only<T: crate::std::cmp::PartialOrd>(a: T, b: T) -> bool {
            a.lt(&b)
        }

        fun main() -> i32 {
            let smallest = pick(crate::std::ops::range(0, 10).collect().into_iter());
            match smallest {
                Option::Some(0i32) => {},
                _ => { return 1; },
            }
            if compare_only(3i32, 5i32) { 0 } else { 2 }
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn adapter_methods_resolve_and_run() {
    // Regression: method calls directly on adapter types (`taken.count()`)
    // used to report E0013 because the receiver type still contained a
    // pending inference variable at method-lookup time.
    let source = r#"
        use crate::std::iter::IntoIterator;

        fun main() -> i32 {
            let mut taken = crate::std::iter::take(crate::std::ops::range(0, 10).into_iter(), 2usize);
            let taken_count = taken.count();
            let mut skipped = crate::std::iter::skip(crate::std::ops::range(0, 10).into_iter(), 8usize);
            let skipped_count = skipped.count();
            let mut enumerated = crate::std::iter::enumerate(crate::std::ops::range(0, 10).into_iter());
            let enumerated_count = enumerated.count();
            if taken_count == 2usize
                && skipped_count == 2usize
                && enumerated_count == 10usize
            { 0 } else { 1 }
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
}

#[test]
fn vec_macro_builds_lists_repeats_and_empty_vectors() {
    let source = r#"
        fun main() -> i32 {
            let list = vec![1, 2, 3];
            let mut total = 0i32;
            for value in list {
                total += value;
            }
            if total != 6i32 { return 1; }

            // The repeat form clones a non-Copy element into every slot.
            let strings = vec![String::from_str("x"); 3usize];
            let mut joined = String::new();
            for value in strings {
                joined.push_str(value.as_str());
            }
            if joined.as_str() != "xxx" { return 2; }

            // The empty form infers its element type from the binding.
            let mut empty: Vector<String> = vec![];
            empty.push(String::from_str("first"));
            empty.push(String::from_str("second"));
            if empty.len() != 2usize { return 3; }
            if empty.get(1usize).unwrap_or(&String::new()).as_str() != "second" { return 4; }

            // Nested vectors and a zero-count repeat.
            let nested = vec![vec![1, 2], vec![3]];
            let mut nested_total = 0i32;
            for outer in nested {
                for value in outer {
                    nested_total += value;
                }
            }
            if nested_total != 6i32 { return 5; }
            if vec![9; 0usize].len() != 0usize { return 6; }
            0
        }
    "#;
    let (code, stdout) = compile_and_run(source, true);
    assert_eq!(code, 0, "stdout: {stdout}");
}
