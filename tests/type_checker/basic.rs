use crate::check;
use type_checker::{FloatTy, IntTy, Type};

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
            r: str
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
            let r2: str = "text";
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
