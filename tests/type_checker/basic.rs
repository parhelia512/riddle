use crate::check;
use type_checker::{CaptureMode, ClosureKind, FloatTy, IntTy, Type};

#[test]
fn accepts_basic_function_body() {
    let result = check(
        r#"
        fun add(left: i32, right: i32) -> i32 {
            let sum: i32 = left + right;
            sum
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert!(
        result
            .expr_types
            .values()
            .any(|ty| matches!(ty, Type::Int(IntTy::I32)))
    );
}

#[test]
fn supports_rust_style_scalar_numeric_types() {
    let result = check(
        r#"
        fun scalars(
            a: i8,
            b: i16,
            c: i32,
            d: i64,
            e: i128,
            f: isize,
            g: u8,
            h: u16,
            i: u32,
            j: u64,
            k: u128,
            l: usize,
            m: f16,
            n: f32,
            o: f64,
            p: f128,
            q: char,
            r: &str
        ) -> f128 {
            let a2: i8 = 1i8;
            let b2: i16 = 1i16;
            let c2: i32 = 1i32;
            let d2: i64 = 1i64;
            let e2: i128 = 1i128;
            let f2: isize = 1isize;
            let g2: u8 = 1u8;
            let h2: u16 = 1u16;
            let i2: u32 = 1u32;
            let j2: u64 = 1u64;
            let k2: u128 = 1u128;
            let l2: usize = 1usize;
            let m2: f16 = 1.0f16;
            let n2: f32 = 1.0f32;
            let o2: f64 = 1.0f64;
            let p2: f128 = 1.0f128;
            let q2: char = 'x';
            let r2: &str = "text";
            p2
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert!(
        result
            .expr_types
            .values()
            .any(|ty| matches!(ty, Type::Float(FloatTy::F128)))
    );
}

#[test]
fn accepts_compound_assignment_ops() {
    let result = check(
        r#"
        fun main() {
            let mut n: i32 = 1;
            n += 2;
            n -= 1;
            n *= 3;
            n /= 2;
            n %= 2;
            n &= 1;
            n |= 2;
            n ^= 3;
            n <<= 1;
            n >>= 1;

            let mut flag = true;
            flag &= false;
            flag |= true;
            flag ^= false;
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_rust_style_array_forms() {
    let result = check(
        r#"
        fun main() {
            let empty: [i32; 0] = [];
            let one: [i32; 1] = [1];
            let many: [i32; 3] = [1, 2, 3];
            let repeated: [i32; 3] = [7; 3];
            let nested: [[i32; 2]; 3] = [[1, 2]; 3];
            let trailing: [i32; 2] = [1, 2,];
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_array_repeat_for_explicit_copy_type() {
    let result = check(
        r#"
        #[lang = "copy"]
        trait Copy {}

        struct Point { x: i32 }
        impl Copy for Point {}

        fun main() {
            let point = Point { x: 1 };
            let points: [Point; 3] = [point; 3];
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn checks_generic_function_calls() {
    let result = check(
        r#"
        fun id<T>(value: T) -> T {
            value
        }

        fun main() -> i32 {
            let a = id(1);
            let b = id(true);
            a
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn infers_const_generic_array_length_from_struct_field() {
    let result = check(
        r#"
        struct Buffer<T, const N: usize> {
            data: [T; N],
        }

        fun main() {
            let b = Buffer { data: [1, 2, 3] };
            let x = b.data[0];
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_explicit_const_generic_argument() {
    let result = check(
        r#"
        struct Buffer<T, const N: usize> {
            data: [T; N],
        }

        fun main() {
            let b: Buffer<i32, 3> = Buffer { data: [1, 2, 3] };
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn infers_const_generic_array_length_for_function_call() {
    let result = check(
        r#"
        fun len<const N: usize>(values: [i32; N]) -> i32 {
            0
        }

        fun main() {
            let n = len([1, 2, 3]);
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn reports_uninferred_generic_function_type_arg() {
    let result = check(
        r#"
        fun make<T>() -> T {
            1
        }

        fun main() {
            let x = make();
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diag| diag.message.contains("cannot infer type argument"))
    );
}

#[test]
fn reports_growing_generic_recursion() {
    let result = check(include_str!("../../examples/generic_wrap_recursion.rid"));

    assert!(result.diagnostics.iter().any(|diag| {
        diag.message
            .contains("generic recursion grows type arguments")
    }));
}

#[test]
fn infers_and_calls_anonymous_function() {
    let result = check(
        r#"
        fun apply(f: fun(i32) -> i32, value: i32) -> i32 {
            f(value)
        }

        fun main() -> i32 {
            let inc = fun(x) { x + 1 };
            apply(inc, 41)
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn later_call_constrains_anonymous_parameter_type() {
    let result = check(
        r#"
        fun main() -> i32 {
            let identity = fun(value) { value };
            identity(42)
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn infers_shared_closure_capture() {
    let result = check(
        r#"
        fun main() -> i32 {
            let base = 1;
            let add = fun(x: i32) { x + base };
            add(41)
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    let info = result.lambda_infos.values().next().unwrap();
    assert_eq!(info.kind, ClosureKind::Fn);
    assert_eq!(info.captures.len(), 1);
    assert_eq!(info.captures[0].name, "base");
    assert_eq!(info.captures[0].mode, CaptureMode::Shared);
}

#[test]
fn infers_mutable_closure_capture() {
    let result = check(
        r#"
        fun main() -> i32 {
            let mut total = 0;
            let mut add = fun(value: i32) -> i32 {
                total += value;
                total
            };
            add(1)
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    let info = result.lambda_infos.values().next().unwrap();
    assert_eq!(info.kind, ClosureKind::FnMut);
    assert_eq!(info.captures[0].mode, CaptureMode::Mutable);
}

#[test]
fn mutable_closure_requires_mutable_binding() {
    let result = check("fun main() { let mut total = 0; let add = fun() { total += 1; }; add(); }");

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E0031" && diagnostic.message.contains("mutable closure")
    }));
}

#[test]
fn infers_once_closure_capture() {
    let result = check(
        r#"
        struct Token { value: i32 }
        fun consume(value: Token) {}
        fun main() {
            let token = Token { value: 1 };
            let once = fun() { consume(token); };
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    let info = result.lambda_infos.values().next().unwrap();
    assert_eq!(info.kind, ClosureKind::FnOnce);
    assert_eq!(info.captures[0].mode, CaptureMode::Value);
}

#[test]
fn nested_closure_does_not_capture_inner_parameters_in_outer_environment() {
    let result = check(
        r#"
        fun nested(base: i32) -> fun(i32) -> fun(i32) -> i32 {
            fun(first: i32) {
                fun(second: i32) { base + first + second }
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert!(
        result
            .lambda_infos
            .values()
            .all(|info| { info.captures.iter().all(|capture| capture.name != "second") })
    );
}

#[test]
fn reports_uninferred_anonymous_parameter() {
    let result = check("fun main() { let id = fun(x) { x }; }");

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E0045" && diagnostic.message.contains("parameter `x`")
    }));
}
