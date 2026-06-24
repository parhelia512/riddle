use ast::{self, support::AstNode};
use frontend::{
    incremental::{IncrementalParser, ReparseMode},
    tree_builder::Parse,
};
use hir::{HirFile, lower_root};
use scope_graph::{builder::build_scope_graph, resolve::resolve_hir};
use type_checker::{
    Diagnostic, FloatTy, IncrementalTypeChecker, IntTy, Type, TypeCheckResult, check_hir,
};

fn check(source: &str) -> TypeCheckResult {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let hir = lower_and_resolve(parse);
    check_hir(&hir)
}

fn lower_and_resolve(parse: &Parse) -> HirFile {
    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax.clone()).unwrap();
    let mut hir = lower_root(root);
    let sg = build_scope_graph(&hir, &syntax);
    resolve_hir(&mut hir, &sg);
    hir
}

fn messages(result: &TypeCheckResult) -> Vec<&str> {
    result
        .diagnostics
        .iter()
        .map(|Diagnostic { message }| message.as_str())
        .collect()
}

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

#[test]
fn reports_let_initializer_mismatch() {
    let result = check(
        r#"
        fun f() {
            let x: bool = 1;
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|msg| msg.contains("let initializer type mismatch"))
    );
}

#[test]
fn reports_return_type_mismatch() {
    let result = check(
        r#"
        fun f() -> bool {
            return 1;
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|msg| msg.contains("return value type mismatch"))
    );
}

#[test]
fn checks_function_call_arguments() {
    let result = check(
        r#"
        fun takes_bool(flag: bool) -> bool {
            flag
        }

        fun main() {
            takes_bool(1);
            takes_bool(true, false);
        }
        "#,
    );

    let messages = messages(&result);
    assert!(
        messages
            .iter()
            .any(|msg| msg.contains("function argument type mismatch"))
    );
    assert!(
        messages
            .iter()
            .any(|msg| msg.contains("expects 1 argument(s), got 2"))
    );
}

#[test]
fn checks_struct_literals_and_field_access() {
    let result = check(
        r#"
        struct Point {
            x: i32,
            y: bool,
        }

        fun main() -> i32 {
            let p = Point{x: 1, y: false};
            p.x
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn checks_struct_literal_shorthand_and_associated_function_call() {
    let result = check(
        r#"
        struct Point {
            x: i32,
            y: i64,
            label: str,
        }

        impl Point {
            fun new(x: i32, y: i64, label: str) -> Point {
                Point{x, y, label}
            }
        }

        fun main() -> i32 {
            let p = Point::new(1, 2, "origin");
            p.x
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn reports_struct_literal_field_errors() {
    let result = check(
        r#"
        struct Point {
            x: i32,
            y: bool,
        }

        fun main() {
            Point{x: true, z: 1};
        }
        "#,
    );

    let messages = messages(&result);
    assert!(
        messages
            .iter()
            .any(|msg| msg.contains("struct field type mismatch"))
    );
    assert!(messages.iter().any(|msg| msg.contains("unknown field `z`")));
    assert!(messages.iter().any(|msg| msg.contains("missing field `y`")));
}

#[test]
fn reports_invalid_field_access() {
    let result = check(
        r#"
        struct Point {
            x: i32,
        }

        fun main() {
            let p = Point{x: 1};
            p.y;
            (1).x;
        }
        "#,
    );

    let messages = messages(&result);
    assert!(messages.iter().any(|msg| msg.contains("unknown field `y`")));
    assert!(
        messages
            .iter()
            .any(|msg| msg.contains("cannot access field `x` on type i32"))
    );
}

#[test]
fn accepts_matching_trait_impl_required_items() {
    let result = check(
        r#"
        trait Show {
            fun show(value: i32) -> str;
            type Output;
            type Default = bool;
        }

        struct Widget {}

        impl Show for Widget {
            fun show(value: i32) -> str {
                "ok"
            }

            type Output = i32;

            fun helper() -> i32 {
                1
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn reports_trait_impl_contract_mismatches() {
    let result = check(
        r#"
        trait Convert {
            fun value(input: i32) -> bool;
            type Item;
        }

        struct Source {}

        impl Convert for Source {
            fun value(input: bool) -> i32 {
                1
            }
        }
        "#,
    );

    let messages = messages(&result);
    assert!(
        messages
            .iter()
            .any(|msg| msg.contains("parameter 1 type mismatch"))
    );
    assert!(
        messages
            .iter()
            .any(|msg| msg.contains("return type mismatch"))
    );
    assert!(
        messages
            .iter()
            .any(|msg| msg.contains("missing associated type `Item`"))
    );
}

#[test]
fn incremental_type_checker_reuses_unchanged_bodies() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(
        r#"
        fun stable() -> i32 {
            1
        }

        fun edited() -> bool {
            let value: bool = true;
            value
        }
        "#,
    );
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let mut checker = IncrementalTypeChecker::new();
    let hir = lower_and_resolve(parse);
    let first = checker.check(&hir);
    assert_eq!(first.result.diagnostics, vec![]);
    assert_eq!(first.stats.checked_bodies, 2);
    assert_eq!(first.stats.reused_bodies, 0);

    let offset = parser.source().find("true").unwrap();
    parser.apply_edit(offset, "true".len(), "1");
    assert!(matches!(
        parser.last_reparse_mode(),
        ReparseMode::Incremental(_)
    ));
    let parse = parser.current_parse().unwrap();
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let hir = lower_and_resolve(parse);
    let second = checker.check(&hir);
    assert_eq!(second.stats.checked_bodies, 1);
    assert_eq!(second.stats.reused_bodies, 1);
    assert!(
        messages(&second.result)
            .iter()
            .any(|msg| msg.contains("let initializer type mismatch"))
    );
}

#[test]
fn incremental_trait_impl_edit_updates_contract_diagnostics() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(
        r#"
        trait Flag {
            fun value() -> bool;
        }

        struct Marker {}

        impl Flag for Marker {
            fun value() -> bool { 1 == 1 }
        }
        "#,
    );
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let mut checker = IncrementalTypeChecker::new();
    let hir = lower_and_resolve(parse);
    let first = checker.check(&hir);
    assert_eq!(first.result.diagnostics, vec![]);

    let offset = parser.source().find("bool").unwrap();
    parser.apply_edit(offset, "bool".len(), "i32");
    assert!(matches!(
        parser.last_reparse_mode(),
        ReparseMode::Incremental(_)
    ));
    let parse = parser.current_parse().unwrap();
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let hir = lower_and_resolve(parse);
    let second = checker.check(&hir);
    assert!(
        messages(&second.result)
            .iter()
            .any(|msg| msg.contains("impl method `value` for trait `Flag` return type mismatch"))
    );
}
