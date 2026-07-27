use crate::{analyze, messages};

#[test]
fn delayed_initialization_is_allowed_before_use() {
    let result = analyze(
        r#"
        fun f() -> i32 {
            let value: i32;
            value = 7;
            value
        }
        "#,
    );
    assert!(result.diagnostics.is_empty(), "{:#?}", result.diagnostics);
}

#[test]
fn delayed_tuple_bindings_are_initialized_independently() {
    let result = analyze(
        r#"
        fun f() -> i32 {
            let (first, second): (i32, i32);
            first = 1;
            second = 2;
            first + second
        }
        "#,
    );
    assert!(result.diagnostics.is_empty(), "{:#?}", result.diagnostics);
}

#[test]
fn delayed_initialization_use_before_assignment_is_rejected() {
    let result = analyze(
        r#"
        fun f() -> i32 {
            let value: i32;
            value
        }
        "#,
    );
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0059" && diagnostic.message.contains("uninitialized")
        }),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn delayed_initialization_requires_every_branch() {
    let complete = analyze(
        r#"
        fun f(flag: bool) -> i32 {
            let value: i32;
            if flag { value = 1; } else { value = 2; }
            value
        }
        "#,
    );
    assert!(
        complete.diagnostics.is_empty(),
        "{:#?}",
        complete.diagnostics
    );

    let incomplete = analyze(
        r#"
        fun f(flag: bool) -> i32 {
            let value: i32;
            if flag { value = 1; }
            value
        }
        "#,
    );
    assert!(
        incomplete
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0059"),
        "{:#?}",
        incomplete.diagnostics
    );
}

#[test]
fn immutable_delayed_binding_rejects_second_assignment() {
    let result = analyze(
        r#"
        fun f() {
            let value: i32;
            value = 1;
            value = 2;
        }
        "#,
    );
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0031" && diagnostic.message.contains("not declared as mutable")
        }),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn mutable_delayed_binding_can_be_assigned_repeatedly() {
    let result = analyze(
        r#"
        fun f() -> i32 {
            let mut value: i32;
            value = 1;
            value = 2;
            value
        }
        "#,
    );
    assert!(result.diagnostics.is_empty(), "{:#?}", result.diagnostics);
}

#[test]
fn delayed_compound_assignment_requires_initialization() {
    let result = analyze(
        r#"
        fun f() {
            let mut value: i32;
            value += 1;
        }
        "#,
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "E0059")
            .count(),
        1,
        "{:#?}",
        result.diagnostics
    );
}

// == Copy types: no errors ==

#[test]
fn ints_are_copy_no_move_errors() {
    let result = analyze(
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
    let result = analyze(
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
    let result = analyze(
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
    let result = analyze(
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
    let result = analyze(
        r#"
        fun f() {
            let a: i32 = 1;
            let mut b: i32 = 2;
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
    let result = analyze(
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
    let result = analyze(
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
    let result = analyze(
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
fn method_call_after_move_is_error() {
    let result = analyze(
        r#"
        struct Box<T> { value: T }

        impl<T> Box<T> {
            fun get(&self) -> &T {
                &self.value
            }
        }

        fun f() {
            let x = Box { value: 1 };
            let y = x;
            x.get();
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("x"))
    );
}

#[test]
fn moved_local_is_error_on_plain_use() {
    let result = analyze(
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
fn moved_local_is_error_on_method_receiver() {
    let result = analyze(
        r#"
        struct Box<T> { value: T }

        impl<T> Box<T> {
            fun get(&self) -> &T {
                &self.value
            }
        }

        fun f() {
            let x = Box { value: 1 };
            let y = x;
            x.get();
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("x"))
    );
}

#[test]
fn moved_field_blocks_parent_use() {
    let result = analyze(
        r#"
        struct Inner { value: i32 }
        struct Outer { inner: Inner, tag: i32 }

        fun f() {
            let outer = Outer{inner: Inner{value: 1}, tag: 2};
            let inner = outer.inner;
            let again = outer;
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("outer"))
    );
}

#[test]
fn moved_array_element_blocks_array_use() {
    let result = analyze(
        r#"
        struct Point { x: i32 }

        fun f() {
            let p = Point{x: 1};
            let arr = [p];
            let again = p;
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
fn copy_types_remain_usable_after_assignment() {
    let result = analyze(
        r#"
        fun f() {
            let x: i32 = 1;
            let y = x;
            let z = x;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn moved_parameter_is_error_on_second_plain_use() {
    let result = analyze(
        r#"
        struct Point { x: i32 }

        fun f(p: Point) {
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
fn moved_parameter_is_error_on_method_receiver() {
    let result = analyze(
        r#"
        struct Box<T> { value: T }

        impl<T> Box<T> {
            fun get(&self) -> &T {
                &self.value
            }
        }

        fun f(x: Box<i32>) {
            let y = x;
            x.get();
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("x"))
    );
}

#[test]
fn copy_parameter_remains_usable_after_assignment() {
    let result = analyze(
        r#"
        fun f(x: i32) {
            let y = x;
            let z = x;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn moved_whole_value_blocks_field_use() {
    let result = analyze(
        r#"
        struct Point { x: i32 }

        fun f() {
            let p = Point{x: 1};
            let q = p;
            let x = p.x;
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved field") && m.contains("x"))
    );
}

#[test]
fn moved_field_blocks_method_on_parent() {
    let result = analyze(
        r#"
        struct Inner { value: i32 }
        struct Outer { inner: Inner, tag: i32 }

        impl Outer {
            fun tag(&self) -> i32 {
                self.tag
            }
        }

        fun f() {
            let outer = Outer{inner: Inner{value: 1}, tag: 2};
            let inner = outer.inner;
            outer.tag();
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value") && m.contains("outer"))
    );
}

#[test]
fn moving_one_field_allows_sibling_field_use() {
    let result = analyze(
        r#"
        struct Inner { value: i32 }
        struct Outer { inner: Inner, tag: i32 }

        fun f() {
            let outer = Outer{inner: Inner{value: 1}, tag: 2};
            let inner = outer.inner;
            let tag = outer.tag;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn moved_array_blocks_index_use() {
    let result = analyze(
        r#"
        struct Point { x: i32 }

        fun f() {
            let a = Point{x: 1};
            let b = Point{x: 2};
            let arr = [a, b];
            let moved = arr;
            let first = arr[0];
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|m| m.contains("use of moved value from array"))
    );
}

#[test]
fn struct_move_in_return_then_use_is_error() {
    let result = analyze(
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
    let result = analyze(
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
    let result = analyze(
        r#"
        struct Point { x: i32 }

        fun f() {
            let a = Point{x: 1};
            let mut b = Point{x: 2};
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
fn match_copy_fields_keep_scrutinee_available() {
    let result = analyze(
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
    assert!(result.diagnostics.is_empty(), "{:#?}", result.diagnostics);
}

#[test]
fn match_move_keeps_unbound_sibling_field_available() {
    let result = analyze(
        r#"
        struct Token {}
        struct Pair { left: Token, right: Token }
        fun consume(value: Token) {}

        fun f() {
            let pair = Pair { left: Token {}, right: Token {} };
            match pair {
                Pair { left } => { consume(left); }
            }
            consume(pair.right);
        }
        "#,
    );
    assert!(result.diagnostics.is_empty(), "{:#?}", result.diagnostics);
}

#[test]
fn match_moved_field_remains_unavailable() {
    let result = analyze(
        r#"
        struct Token {}
        struct Pair { left: Token, right: Token }
        fun consume(value: Token) {}

        fun f() {
            let pair = Pair { left: Token {}, right: Token {} };
            match pair {
                Pair { left } => {}
            }
            consume(pair.left);
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|message| message.contains("use of moved field") && message.contains("left")),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn match_int_scrutinee_is_not_moved() {
    let result = analyze(
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

#[test]
fn cannot_move_field_out_of_drop_type_with_pattern() {
    let result = analyze(
        r#"
        #[lang = "drop"]
        trait Drop { fun drop(&mut self); }

        struct Guard {}
        struct Owner { guard: Guard }
        impl Drop for Owner { fun drop(&mut self) {} }

        fun main() {
            let owner = Owner { guard: Guard {} };
            match owner {
                Owner { guard } => {}
            }
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0305"),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn cannot_move_nested_field_out_of_drop_type_with_pattern() {
    let result = analyze(
        r#"
        #[lang = "drop"]
        trait Drop { fun drop(&mut self); }

        struct Guard {}
        struct Inner { guard: Guard }
        struct Outer { inner: Inner }
        impl Drop for Inner { fun drop(&mut self) {} }

        fun main() {
            let outer = Outer { inner: Inner { guard: Guard {} } };
            match outer {
                Outer { inner: Inner { guard } } => {}
            }
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0305"),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn cannot_move_pattern_binding_in_match_guard() {
    let result = analyze(
        r#"
        struct Token {}
        enum MaybeToken { Some(Token), None }
        fun consume(value: Token) -> bool { true }

        fun main(value: MaybeToken) {
            match value {
                MaybeToken::Some(token) if consume(token) => {},
                MaybeToken::Some(token) => {},
                MaybeToken::None => {},
            }
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0307"),
        "{:#?}",
        result.diagnostics
    );
}

// == Struct literal fields ==

#[test]
fn struct_literal_moves_fields() {
    let result = analyze(
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
    let result = analyze(
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

#[test]
fn dynamic_array_element_move_is_tracked() {
    let source = r#"
        struct Guard {}
        fun consume(value: Guard) {}

        fun test(index: usize) {
            let values = [Guard {}, Guard {}];
            consume(values[index]);
            consume(values[index]);
        }
    "#;
    let result = analyze(source);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0100"),
        "expected repeated dynamic element move to be rejected: {:#?}",
        result.diagnostics
    );
}

#[test]
fn array_repeat_copy_value_remains_usable() {
    let result = analyze(
        r#"
        fun f() {
            let x = 1;
            let arr = [x; 3];
            let y = x;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

// == Field access (borrow, not move) ==

#[test]
fn field_access_does_not_move() {
    let result = analyze(
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
    // &p does not move p — reading a field through the reference is fine.
    let result = analyze(
        r#"
        struct Point { x: i32 }

        fun f() {
            let p = Point{x: 1};
            let r = &p;
            let a = p.x;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn mutable_references_move_instead_of_copying() {
    let result = analyze(
        r#"
        fun f() {
            let mut value: i32 = 1;
            let first: &mut i32 = &mut value;
            let second = first;
            *first = 2;
            *second = 3;
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0100" && diagnostic.message.contains("first"))
    );
}

#[test]
fn mutable_reference_arguments_are_automatically_reborrowed() {
    let result = analyze(
        r#"
        fun touch(value: &mut i32) {
            *value += 1;
        }

        fun f() {
            let mut value: i32 = 1;
            let reference: &mut i32 = &mut value;
            touch(reference);
            touch(reference);
        }
        "#,
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn shared_reborrow_freezes_the_parent_mutable_reference() {
    let result = analyze(
        r#"
        fun touch(value: &mut i32) {
            *value += 1;
        }

        fun f() {
            let mut value: i32 = 1;
            let reference: &mut i32 = &mut value;
            let shared: &i32 = &*reference;
            touch(reference);
            *shared;
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0300")
    );
}

#[test]
fn mutable_borrow_ends_after_the_last_use() {
    let result = analyze(
        r#"
        struct Boxed { value: i32 }

        impl Boxed {
            fun set(&mut self) {
                self.value = 4;
            }
        }

        fun f() {
            let mut boxed = Boxed { value: 1 };
            let reference: &mut i32 = &mut boxed.value;
            *reference = 2;
            boxed.set();
        }
        "#,
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn method_return_reference_keeps_receiver_borrowed() {
    let result = analyze(
        r#"
        struct Boxed { value: i32 }

        impl Boxed {
            fun get_mut(&mut self) -> &mut i32 {
                &mut self.value
            }

            fun set(&mut self) {
                self.value = 4;
            }
        }

        fun f() {
            let mut boxed = Boxed { value: 1 };
            let reference = boxed.get_mut();
            boxed.set();
            *reference = 2;
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0302")
    );
}

#[test]
fn wrapped_method_return_preserves_receiver_provenance() {
    let result = analyze(
        r#"
        enum Maybe<T> { Some(T), None }

        impl<T> Maybe<T> {
            fun unwrap_or(self, fallback: T) -> T {
                match self {
                    Maybe::Some(value) => value,
                    Maybe::None => fallback,
                }
            }
        }

        struct Boxed { value: i32 }

        impl Boxed {
            fun get_mut(&mut self) -> Maybe<&mut i32> {
                Maybe::Some(&mut self.value)
            }

            fun set(&mut self) {
                self.value = 4;
            }
        }

        fun f() {
            let mut boxed = Boxed { value: 1 };
            let mut fallback = 0;
            let reference = boxed.get_mut().unwrap_or(&mut fallback);
            boxed.set();
            *reference = 2;
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0302")
    );
}

#[test]
fn generic_supertrait_shared_methods_do_not_move_receiver() {
    let result = analyze(
        r#"
        trait Named { fun name(&self) -> i32; }
        trait Tagged: Named { fun tag(&self) -> i32; }

        fun describe<T: Tagged>(value: T) -> i32 {
            value.name() + value.tag()
        }
        "#,
    );

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn non_generic_enum_preserves_reference_provenance() {
    let result = analyze(
        r#"
        enum Slot { Some(&mut i32), None }

        impl Slot {
            fun unwrap_or(self, fallback: &mut i32) -> &mut i32 {
                match self {
                    Slot::Some(value) => value,
                    Slot::None => fallback,
                }
            }
        }

        struct Boxed { value: i32 }

        impl Boxed {
            fun get_mut(&mut self) -> Slot {
                Slot::Some(&mut self.value)
            }

            fun set(&mut self) {
                self.value = 4;
            }
        }

        fun f() {
            let mut boxed = Boxed { value: 1 };
            let mut fallback = 0;
            let reference = boxed.get_mut().unwrap_or(&mut fallback);
            boxed.set();
            *reference = 2;
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0302")
    );
}

#[test]
fn mutable_borrows_of_disjoint_fields_can_coexist() {
    let result = analyze(
        r#"
        struct Pair { left: i32, right: i32 }

        fun f() {
            let mut pair = Pair { left: 1, right: 2 };
            let left: &mut i32 = &mut pair.left;
            let right: &mut i32 = &mut pair.right;
            *left = 3;
            *right = 4;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn move_while_borrowed_is_error() {
    // Moving p while a shared borrow exists is E0304.
    let result = analyze(
        r#"
        struct Point { x: i32 }

        fun f() {
            let p = Point{x: 1};
            let r = &p;
            let q = p;
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == "E0304" && d.message.contains("p"))
    );
}

#[test]
fn rejects_moving_a_field_out_of_explicit_drop_type() {
    let result = analyze(
        r#"
        #[lang = "drop"]
        trait Drop {
            fun drop(&mut self);
        }

        struct Token {}
        struct Guard { value: Token }

        impl Drop for Guard {
            fun drop(&mut self) {}
        }

        fun take(value: Token) {}

        fun f() {
            let guard = Guard { value: Token {} };
            take(guard.value);
        }
        "#,
    );

    assert!(
        result.diagnostics.iter().any(|d| d.code == "E0305"),
        "expected move-out-of-Drop diagnostic, got {:?}",
        result.diagnostics
    );
}

// == Explicit Copy impl ==

#[test]
fn explicit_copy_impl_makes_struct_copyable() {
    let result = analyze(
        r#"
        #[lang = "copy"]
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
fn namespaced_copy_impl_makes_struct_copyable() {
    let result = analyze(
        r#"
        pub mod std {
            pub mod marker {
                #[lang = "copy"]
                pub trait Copy {}
            }
        }

        struct Vec2 { x: i32, y: i32 }

        impl std::marker::Copy for Vec2 {}

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
fn generic_copy_impl_makes_instantiations_copyable() {
    let result = analyze(
        r#"
        #[lang = "copy"]
        trait Copy {}

        struct Box<T> { value: T }

        impl<T: Copy> Copy for Box<T> {}

        fun f() {
            let a: Box<i32> = Box{value: 1};
            let b = a;
            let c = a;
        }
        "#,
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn without_copy_impl_struct_is_not_copyable() {
    let result = analyze(
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

#[test]
fn unannotated_copy_trait_does_not_enable_copy_hook() {
    let result = analyze(
        r#"
        trait Copy {}

        struct Vec2 { x: i32 }

        impl Copy for Vec2 {}

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
    let result = analyze(
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
    let result = analyze(
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

#[test]
fn anonymous_function_parameters_follow_move_rules() {
    let result = analyze(
        r#"
        struct Point { x: i32 }

        fun main() {
            let consume_twice = fun(value: Point) {
                let first = value;
                let second = value;
            };
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|message| message.contains("use of moved value") && message.contains("value"))
    );
}

#[test]
fn value_capture_moves_non_copy_binding() {
    let result = analyze(
        r#"
        struct Token { value: i32 }
        fun consume(value: Token) {}

        fun main() {
            let token = Token { value: 1 };
            let once = fun() { consume(token); };
            let again = token;
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|message| message.contains("use of moved value") && message.contains("token"))
    );
}

#[test]
fn once_closure_cannot_be_called_twice() {
    let result = analyze(
        r#"
        struct Token { value: i32 }
        fun consume(value: Token) {}

        fun main() {
            let token = Token { value: 1 };
            let once = fun() { consume(token); };
            once();
            once();
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|message| message.contains("use of moved value") && message.contains("once"))
    );
}

#[test]
fn closure_value_moves_when_passed_by_value() {
    let result = analyze(
        r#"
        fun consume(f: fun(i32) -> i32) {}

        fun test() {
            let add_one = fun(value: i32) { value + 1 };
            consume(add_one);
            consume(add_one);
        }
        "#,
    );
    assert!(
        messages(&result)
            .iter()
            .any(|message| message.contains("use of moved value") && message.contains("add_one")),
        "closure values own their environment and must move by value: {:#?}",
        result.diagnostics
    );
}

#[test]
fn once_closure_stays_once_after_branch_join() {
    let result = analyze(
        r#"
        struct Token { value: i32 }
        fun consume(value: Token) {}

        fun main() {
            let left = Token { value: 1 };
            let right = Token { value: 2 };
            let once = if true {
                fun() { consume(left); }
            } else {
                fun() { consume(right); }
            };
            once();
            once();
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|message| message.contains("use of moved value") && message.contains("once"))
    );
}

#[test]
fn shared_pattern_capture_blocks_later_move() {
    let result = analyze(
        r#"
        struct Token { value: i32 }
        fun consume(value: Token) {}
        fun inspect(value: &Token) -> i32 { value.value }

        fun main() {
            let source = Token { value: 1 };
            match source {
                token => {
                    let read = fun() { inspect(&token) };
                    consume(token);
                    read();
                }
            }
        }
        "#,
    );

    let diagnostics = messages(&result);
    assert!(
        diagnostics.iter().any(|message| {
            message.contains("cannot move")
                && message.contains("token")
                && message.contains("borrowed")
        }),
        "{diagnostics:?}"
    );
}

#[test]
fn shared_capture_blocks_assignment_while_closure_is_live() {
    let result = analyze(
        r#"
        fun main() {
            let mut base = 1;
            let read = fun() { base };
            base = 2;
            read();
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|message| message.contains("cannot assign") && message.contains("base"))
    );
}

#[test]
fn destructured_bindings_are_tracked_independently() {
    let result = analyze(
        r#"
        struct Token { value: i32 }
        fun consume(value: Token) {}

        fun main() {
            let pair = (Token { value: 1 }, Token { value: 2 });
            let (first, second) = pair;
            consume(first);
            consume(second);
        }
        "#,
    );

    assert_eq!(messages(&result), Vec::<&str>::new());
}

#[test]
fn moving_a_destructured_binding_twice_is_rejected() {
    let result = analyze(
        r#"
        struct Token { value: i32 }
        fun consume(value: Token) {}

        fun main() {
            let (first, second) = (Token { value: 1 }, Token { value: 2 });
            consume(first);
            consume(first);
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|message| message.contains("use of moved value") && message.contains("first")),
        "{:?}",
        messages(&result)
    );
}
