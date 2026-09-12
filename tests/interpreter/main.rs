//! Interpreter integration tests: compile Riddle sources through the full
//! pipeline and execute the MIR with the in-process interpreter — no C
//! toolchain required. The scenarios mirror `tests/mir/std_behavior.rs` so
//! interpreter semantics stay aligned with the C backend.

use interpreter::{self, Config};
use riddlec::pipeline;

/// Compiles `source` and interprets `main`. Returns the exit code (or trap
/// debug form), captured stdout, and captured stderr including trap output.
fn run(source: &str) -> (Result<i32, String>, String, String) {
    let result = pipeline::compile(source);
    assert!(
        result.success(),
        "riddle diagnostics: {:#?}
hir: {:#?}
macro: {:#?}
parse: {:#?}
analysis: {:#?}",
        result.type_result.diagnostics,
        result.hir_diagnostics,
        result.macro_diagnostics,
        result.parse_errors,
        result.analysis_diagnostics
    );
    let module = result.mir_module.expect("successful compile produced MIR");
    let config = Config {
        rng_seed: 0x5EED,
        ..Config::default()
    };
    let outcome = interpreter::run_with(&module, result.source_files, config);
    let mut stderr = String::from_utf8_lossy(&outcome.stderr).into_owned();
    if let Err(trap) = &outcome.result {
        let mut rendered = Vec::new();
        trap.render(&mut rendered);
        if !rendered.is_empty() {
            stderr.push_str(&String::from_utf8_lossy(&rendered));
        }
    }
    let code = outcome.result.clone().map_err(|trap| format!("{trap:?}"));
    (
        code,
        String::from_utf8_lossy(&outcome.stdout).into_owned(),
        stderr,
    )
}

fn assert_ok(source: &str, expected_stdout: &str) {
    let (code, stdout, stderr) = run(source);
    assert_eq!(code, Ok(0), "stdout: {stdout}stderr: {stderr}");
    assert_eq!(stdout, expected_stdout, "stderr: {stderr}");
}

#[test]
fn interpreter_runs_hello_world() {
    assert_ok(
        r#"
        fun main() -> i32 {
            println!("hello, world");
            0
        }
        "#,
        "hello, world\n",
    );
}

#[test]
fn interpreter_format_placeholders() {
    assert_ok(
        r#"
        fun main() -> i32 {
            let name = "riddle";
            let version = 42i32;
            println!("hello {name} v{version}!");
            println!("{0} then {1} then {0} and {}", 1i32, 2i32, 3i32);
            println!("debug: {version:?} hex {}", 7i32);
            println!("{}", format!("{}+{}={}", 1i32, 2i32, 3i32).as_str());
            0
        }
        "#,
        "hello riddle v42!\n1 then 2 then 1 and 1\ndebug: 42 hex 7\n1+2=3\n",
    );
}

#[test]
fn interpreter_integer_arithmetic_matches_c_semantics() {
    assert_ok(
        r#"
        fun main() -> i32 {
            println!("{}", 7i32 / 2i32);
            println!("{}", -7i32 / 2i32);
            println!("{}", -7i32 % 3i32);
            println!("{}", 1i64 << 40);
            println!("{}", -8i32 >> 1);
            println!("{}", 255u8 + 1u8);
            println!("{}", -128i8);
            0
        }
        "#,
        "3\n-3\n-1\n1099511627776\n-4\n0\n-128\n",
    );
}

#[test]
fn interpreter_float_arithmetic_rounds_through_f32() {
    assert_ok(
        r#"
        fun main() -> i32 {
            println!("{}", 3.5f64 + 1.5f64);
            println!("{}", 0.1f32 + 0.2f32);
            println!("{}", 2.5f64 * 4.0f64);
            0
        }
        "#,
        "5.000000\n0.300000\n10.000000\n",
    );
}

#[test]
fn interpreter_division_by_zero_aborts() {
    let (code, _stdout, stderr) = run("fun main() -> i32 { let x = 1i32 / 0i32; x }");
    assert!(
        code.as_ref().unwrap_err().contains("division by zero"),
        "code: {code:?} stderr: {stderr}"
    );
    assert!(
        stderr.contains("riddle: division by zero"),
        "stderr: {stderr}"
    );
}

#[test]
fn interpreter_signed_division_overflow_aborts() {
    let source = r#"
        fun main() -> i32 {
            let min = -9223372036854775808i64;
            let quotient = min / -1i64;
            quotient as i32
        }
        "#;
    let (code, _stdout, stderr) = run(source);
    assert!(
        code.as_ref()
            .unwrap_err()
            .contains("integer division overflow"),
        "code: {code:?}"
    );
    assert!(
        stderr.contains("riddle: integer division overflow"),
        "stderr: {stderr}"
    );
}

#[test]
fn interpreter_panic_reports_location() {
    let (code, _stdout, stderr) = run(r#"fun main() -> i32 { crate::std::panic::panic("boom") }"#);
    assert!(code.is_err());
    assert!(
        stderr.starts_with("thread 'main' panicked at "),
        "stderr: {stderr}"
    );
    assert!(stderr.ends_with(":\nboom\n"), "stderr: {stderr}");
}

#[test]
fn interpreter_panic_inside_std_reports_std_region() {
    // `Option::unwrap` panics inside the bundled std, whose code lives in
    // the region appended after the user source; the site must resolve to
    // the std segment instead of falling back to the module name.
    let source = r#"
        fun main() -> i32 {
            let value: crate::std::option::Option<i32> = crate::std::option::Option::None;
            value.unwrap();
            0
        }
    "#;
    let (_code, _stdout, stderr) = run(source);
    assert!(stderr.contains("at std:"), "stderr: {stderr}");
}

#[test]
fn interpreter_panic_after_macro_expansion_maps_to_user_source() {
    // `println!`/`assert!` rewrite the user source before lowering, so the
    // panic site's offset no longer matches the original text; the source
    // files must map it back to the user region.
    let source = r#"
        fun main() -> i32 {
            println!("before");
            assert!(1i32 == 2i32);
            0
        }
    "#;
    let (_code, stdout, stderr) = run(source);
    assert_eq!(stdout, "before\n", "stderr: {stderr}");
    assert!(stderr.contains("assertion failed"), "stderr: {stderr}");
    assert!(stderr.contains("at source:"), "stderr: {stderr}");
    // The site must be the `assert!` call in the user region, not the std
    // region or the fallback module name.
    assert!(!stderr.contains("at std:"), "stderr: {stderr}");
}

#[test]
fn interpreter_index_out_of_bounds_aborts() {
    let source = r#"
        fun main() -> i32 {
            let values = [1i32, 2i32, 3i32];
            println!("{}", values[3usize]);
            0
        }
        "#;
    let (code, _stdout, stderr) = run(source);
    assert!(
        code.as_ref().unwrap_err().contains("index out of bounds"),
        "code: {code:?}"
    );
    assert!(
        stderr.contains("riddle: index out of bounds"),
        "stderr: {stderr}"
    );
}

#[test]
fn interpreter_float_to_int_cast_saturates() {
    assert_ok(
        r#"
        fun main() -> i32 {
            println!("{}", 1e300 as i32);
            println!("{}", -1e300 as i32);
            println!("{}", (0.0 / 0.0) as i32);
            println!("{}", 2.9 as i32);
            0
        }
        "#,
        "2147483647\n-2147483648\n0\n2\n",
    );
}

#[test]
fn interpreter_recursion_and_match_guards() {
    assert_ok(
        r#"
        fun fib(n: i32) -> i32 {
            if n < 2i32 { return n; }
            fib(n - 1i32) + fib(n - 2i32)
        }

        fun classify(n: i32) -> &str {
            match n {
                0 => "zero",
                2 => "small",
                n if n < 10 => "medium",
                _ => "large",
            }
        }

        fun main() -> i32 {
            println!("{}", fib(10));
            println!("{}", classify(0));
            println!("{}", classify(2));
            println!("{}", classify(7));
            println!("{}", classify(100));
            0
        }
        "#,
        "55\nzero\nsmall\nmedium\nlarge\n",
    );
}

#[test]
fn interpreter_deep_recursion_is_supported() {
    assert_ok(
        r#"
        fun count(n: i64) -> i64 {
            if n == 0i64 { return 0i64; }
            count(n - 1i64)
        }

        fun main() -> i32 {
            println!("{}", count(20000i64));
            0
        }
        "#,
        "0\n",
    );
}

#[test]
fn interpreter_bracket_lambdas_run() {
    // Ported from tests/mir/std_behavior.rs `std_bracket_lambdas_run`.
    let source = r#"
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
            let sum = Counter { index: 0usize, limit: 5usize }
                .fold(0i32, [acc, v -> acc + v]);
            if sum != 15i32 { return 1; }

            let mut counter2 = Counter { index: 0usize, limit: 5usize };
            if counter2.find([it -> *it == 4i32]).unwrap_or(0i32) != 4i32 { return 2; }

            let chained = Counter { index: 0usize, limit: 5usize }
                .map [v -> v + 1i32]
                .filter [it -> *it > 3i32];
            let mut total = 0i32;
            for value in chained {
                total += value;
            }
            if total != 15i32 { return 3; }

            let base = 10i32;
            let offset = move [ -> base + 5i32];
            if invoke(offset) != 15i32 { return 4; }

            let double = [it -> it * 2i32];
            if double(21i32) != 42i32 { return 5; }

            let mut count = 0i32;
            let mut bump = [ -> { count += 1i32; count }];
            bump();
            bump();
            if count != 2i32 { return 6; }

            0
        }
        "#;
    assert_ok(source, "");
}

#[test]
fn interpreter_question_operator_supports_from_and_option() {
    // Ported from tests/mir/std_behavior.rs.
    let source = r#"
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
        "#;
    assert_ok(source, "");
}

#[test]
fn interpreter_vector_operations_roundtrip() {
    // Ported from tests/mir/std_behavior.rs
    // `vector_insert_remove_sort_contains_retain_roundtrip`.
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
    assert_ok(source, "");
}

#[test]
fn interpreter_hash_map_get_or_insert_counts_once() {
    // Ported from tests/mir/std_behavior.rs.
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
    assert_ok(source, "");
}

#[test]
fn interpreter_string_split_replace_and_ascii_case() {
    // Ported from tests/mir/std_behavior.rs
    // `string_split_replace_and_ascii_case_roundtrip`.
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
    assert_ok(source, "");
}

#[test]
fn interpreter_fs_roundtrip_in_temp_dir() {
    // Ported from tests/mir/std_behavior.rs `std_fs_roundtrips_file_content`,
    // redirected into a unique temp directory so parallel test runs never
    // collide.
    let dir = std::env::temp_dir().join(format!(
        "riddle-interp-fs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("riddle_fs_e2e.tmp");
    let target = target.display().to_string().replace('\\', "/");
    let source = format!(
        r#"
        use crate::std::fs::{{read_to_string, write, FsFile}};
        use crate::std::result::Result;

        fun main() -> i32 {{
            match write("{target}", "hello fs") {{
                Result::Ok(()) => {{}},
                Result::Err(_) => {{ return 1; }},
            }}
            let content = match read_to_string("{target}") {{
                Result::Ok(text) => text,
                Result::Err(_) => {{ return 2; }},
            }};
            if content.len() != 8usize {{
                return 3;
            }}
            match FsFile::open("{target}") {{
                Result::Ok(mut file) => {{
                    let mut buffer = [0u8; 16];
                    let read = file.read(&mut buffer).unwrap_or(0usize);
                    if read != 8usize {{
                        return 4;
                    }}
                    if buffer[0usize] != 104u8 {{
                        return 5;
                    }}
                }},
                Result::Err(_) => {{ return 6; }},
            }}
            0
        }}
        "#
    );
    assert_ok(&source, "");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn interpreter_drops_run_deterministically() {
    assert_ok(
        r#"
        struct Guard { label: &str }

        impl crate::std::ops::Drop for Guard {
            fun drop(&mut self) {
                println!("drop {}", self.label);
            }
        }

        fun make(label: &str) -> Guard {
            Guard { label }
        }

        fun main() -> i32 {
            let outer = make("outer");
            {
                let inner = make("inner");
                println!("mid");
            }
            println!("end");
            crate::std::mem::drop(outer);
            0
        }
        "#,
        "mid\ndrop inner\nend\ndrop outer\n",
    );
}

#[test]
fn interpreter_moves_and_borrowed_access() {
    assert_ok(
        r#"
        struct Point { x: i32, y: i32 }

        fun magnitude(point: &Point) -> i32 {
            point.x * point.x + point.y * point.y
        }

        fun main() -> i32 {
            let mut point = Point { x: 3, y: 4 };
            point.x = -3;
            println!("{}", magnitude(&point));
            let other = point;
            println!("{} {}", other.x, other.y);
            0
        }
        "#,
        "25\n-3 4\n",
    );
}

#[test]
fn interpreter_dyn_trait_dispatch() {
    assert_ok(
        r#"
        trait Shape {
            fun area(&self) -> i32;
        }

        struct Square { side: i32 }
        struct Rect { width: i32, height: i32 }

        impl Shape for Square {
            fun area(&self) -> i32 { self.side * self.side }
        }

        impl Shape for Rect {
            fun area(&self) -> i32 { self.width * self.height }
        }

        fun describe(shape: &dyn Shape) -> &str {
            if shape.area() > 15 { "big" } else { "small" }
        }

        fun main() -> i32 {
            let square = Square { side: 4 };
            let rect = Rect { width: 3, height: 5 };
            println!("{}", square.area());
            println!("{}", describe(&square));
            println!("{}", describe(&rect));
            0
        }
        "#,
        "16\nbig\nsmall\n",
    );
}

#[test]
fn interpreter_generics_monomorphize_and_call() {
    assert_ok(
        r#"
        fun pick<T>(a: T, b: T) -> T
        where T: crate::std::cmp::PartialOrd {
            if a < b { a } else { b }
        }

        struct Pair<A, B> { first: A, second: B }

        impl<A, B> Pair<A, B> {
            fun swap(self) -> Pair<B, A> {
                Pair { first: self.second, second: self.first }
            }
        }

        fun main() -> i32 {
            println!("{}", pick(3i32, 7i32));
            println!("{}", pick(2.5f64, 1.5f64));
            let pair = Pair { first: 1i32, second: "two" };
            let swapped = pair.swap();
            println!("{}", swapped.first);
            0
        }
        "#,
        "3\n1.500000\ntwo\n",
    );
}

#[test]
fn interpreter_for_loops_over_ranges() {
    assert_ok(
        r#"
        fun main() -> i32 {
            let mut sum = 0i32;
            for i in 0..10 {
                sum += i;
            }
            for _ in 0..0 {
                sum += 100;
            }
            for i in 1..=3 {
                sum *= i;
            }
            println!("{}", sum);
            0
        }
        "#,
        "270\n",
    );
}

#[test]
fn interpreter_process_exit_code() {
    let (code, _stdout, _stderr) = run("fun main() -> i32 { crate::std::process::exit(7) }");
    assert!(
        code.as_ref().unwrap_err().contains("ProcessExit(7)"),
        "code: {code:?}"
    );
}

#[test]
fn interpreter_unknown_extern_is_reported() {
    let source = r#"
        unsafe extern "C" {
            fun mysterious_runtime_call(value: i32) -> i32;
        }

        fun main() -> i32 {
            unsafe { mysterious_runtime_call(1i32) }
        }
        "#;
    let (code, _stdout, stderr) = run(source);
    assert!(
        code.as_ref().unwrap_err().contains("UnsupportedExtern"),
        "code: {code:?}"
    );
    assert!(
        stderr.contains("interpreter does not support extern"),
        "stderr: {stderr}"
    );
}

#[test]
fn interpreter_prints_uints_and_chars() {
    assert_ok(
        r#"
        fun main() -> i32 {
            println!("{}", 42u8);
            println!("{}", -1isize);
            println!("{}", 'r');
            println!("{}", true);
            0
        }
        "#,
        "42\n-1\nr\ntrue\n",
    );
}
