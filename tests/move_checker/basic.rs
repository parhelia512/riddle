use crate::{check_moves, messages};

// == Copy types: no errors ==

#[test]
fn ints_are_copy_no_move_errors() {
    let result = check_moves(
        r#"
        fun f() {
            let a: i32 = 1;
            let b = a;
            let c = a;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn bools_are_copy_no_move_errors() {
    let result = check_moves(
        r#"
        fun f() {
            let a: bool = true;
            let b = a;
            let c = a;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn floats_are_copy_no_move_errors() {
    let result = check_moves(
        r#"
        fun f() {
            let a: f64 = 3.14;
            let b = a;
            let c = a;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn references_are_copy() {
    let result = check_moves(
        r#"
        fun f() {
            let x: i32 = 42;
            let a: &i32 = &x;
            let b = a;
            let c = a;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn assignment_does_not_move_copy_types() {
    let result = check_moves(
        r#"
        fun f() {
            let a: i32 = 1;
            let b: i32 = 2;
            b = a;
            let c = a;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

// == Struct move (non-Copy) ==

#[test]
fn struct_let_binding_moves_source() {
    let result = check_moves(
        r#"
        struct Point { x: i32 }

        fun f() {
            let p = Point{x: 1};
            let q = p;
            let r = p;
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("p"))
    );
}

#[test]
fn struct_let_binding_first_use_is_ok() {
    let result = check_moves(
        r#"
        struct Point { x: i32 }

        fun f() {
            let a = Point{x: 1};
            let b = a;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn struct_move_in_function_call() {
    let result = check_moves(
        r#"
        struct Point { x: i32 }

        fun take(p: Point) -> bool { true }

        fun f() {
            let p = Point{x: 1};
            take(p);
            take(p);
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("p"))
    );
}

#[test]
fn struct_move_in_return_then_use_is_error() {
    let result = check_moves(
        r#"
        struct Point { x: i32 }

        fun consume(p: Point) {}

        fun f() {
            let p = Point{x: 1};
            consume(p);
            let q = p;
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("p"))
    );
}

#[test]
fn struct_move_in_return_no_reuse_is_ok() {
    let result = check_moves(
        r#"
        struct Point { x: i32 }

        fun f() -> Point {
            let p = Point{x: 1};
            return p;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

// == Assignment ==

#[test]
fn assignment_moves_rhs_struct() {
    let result = check_moves(
        r#"
        struct Point { x: i32 }

        fun f() {
            let a = Point{x: 1};
            let b = Point{x: 2};
            b = a;
            let c = a;
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("a"))
    );
}

// == Match scrutinee ==

#[test]
fn match_scrutinee_is_moved() {
    let result = check_moves(
        r#"
        struct Point { x: i32, y: i32 }

        fun f() {
            let p = Point{x: 1, y: 2};
            match p {
                Point { x, y } => { let tmp = x; }
            }
            let q = p;
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("p"))
    );
}

#[test]
fn match_int_scrutinee_is_not_moved() {
    let result = check_moves(
        r#"
        fun f() {
            let v: i32 = 42;
            match v {
                other => {}
            }
            let w = v;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

// == Struct literal fields ==

#[test]
fn struct_literal_moves_fields() {
    let result = check_moves(
        r#"
        struct Inner { value: i32 }
        struct Outer { inner: Inner }

        fun f() {
            let inner = Inner{value: 1};
            let outer = Outer{inner};
            let x = inner;
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("inner"))
    );
}

// == Array ==

#[test]
fn array_moves_elements() {
    let result = check_moves(
        r#"
        struct Point { x: i32 }

        fun f() {
            let p = Point{x: 1};
            let arr = [p];
            let q = p;
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("p"))
    );
}

// == Field access (borrow, not move) ==

#[test]
fn field_access_does_not_move() {
    let result = check_moves(
        r#"
        struct Point { x: i32, y: i32 }

        fun f() {
            let p = Point{x: 1, y: 2};
            let a = p.x;
            let b = p.y;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

// == Reference (borrow, not move) ==

#[test]
fn taking_reference_does_not_move() {
    let result = check_moves(
        r#"
        struct Point { x: i32 }

        fun f() {
            let p = Point{x: 1};
            let r = &p;
            let q = p;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

// == Explicit Copy impl ==

#[test]
fn explicit_copy_impl_makes_struct_copyable() {
    let result = check_moves(
        r#"
        trait Copy {}

        struct Vec2 { x: i32, y: i32 }

        impl Copy for Vec2 {}

        fun f() {
            let a = Vec2{x: 1, y: 2};
            let b = a;
            let c = a;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn without_copy_impl_struct_is_not_copyable() {
    let result = check_moves(
        r#"
        trait Copy {}

        struct Vec2 { x: i32 }

        fun f() {
            let a = Vec2{x: 1};
            let b = a;
            let c = a;
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("use of moved value"))
    );
}

// == Pattern bindings (whole scrutinee) ==

#[test]
fn match_binding_whole_scrutinee_is_consumed() {
    let result = check_moves(
        r#"
        struct Point { x: i32 }

        fun consume(p: Point) {}

        fun f() {
            let p = Point{x: 1};
            match p {
                val => { consume(val); consume(val); }
            }
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("val"))
    );
}

// == Block tail ==

#[test]
fn block_tail_moves_value() {
    let result = check_moves(
        r#"
        struct Point { x: i32 }

        fun f() {
            let p = Point{x: 1};
            let q = { p };
            let r = p;
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("p"))
    );
}
