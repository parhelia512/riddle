use crate::check;
use type_checker::{
    CaptureMode, CaptureSource, ClosureKind, FloatTy, IntTy, PatternBindingMode, Type,
};

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
fn accepts_never_returning_panic_in_value_position() {
    let result = check(
        r#"
        unsafe extern "C" {
            safe fun panic(message: &str) -> !;
        }

        fun choose(flag: bool) -> i32 {
            if flag { 1 } else { panic("bad") }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
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
            e: isize,
            f: u8,
            g: u16,
            h: u32,
            i: u64,
            j: usize,
            k: f32,
            l: f64,
            m: char,
            n: &str
        ) -> f64 {
            let a2: i8 = 1i8;
            let b2: i16 = 1i16;
            let c2: i32 = 1i32;
            let d2: i64 = 1i64;
            let e2: isize = 1isize;
            let f2: u8 = 1u8;
            let g2: u16 = 1u16;
            let h2: u32 = 1u32;
            let i2: u64 = 1u64;
            let j2: usize = 1usize;
            let k2: f32 = 1.0f32;
            let l2: f64 = 1.0f64;
            let m2: char = 'x';
            let n2: &str = "text";
            l2
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert!(
        result
            .expr_types
            .values()
            .any(|ty| matches!(ty, Type::Float(FloatTy::F64)))
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
fn rejects_impl_type_arguments_on_associated_function() {
    let result = check(
        r#"
        struct Container<T> {
            pointer: *const T,
        }

        impl<T> Container<T> {
            fun take(value: T) {}
        }

        fun main() {
            Container::take::<i32>(1);
        }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E0005"
            && diagnostic
                .message
                .contains("function `take` expects 0 type argument(s), got 1")
    }));
}

#[test]
fn infers_generic_method_arguments() {
    let result = check(
        r#"
        pub struct C {}

        impl C {
            fun test<T>(&self, f: T) {}
        }

        pub fun f1() {
            let c = C {};
            c.test(1);
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert!(
        result
            .generic_calls
            .values()
            .any(|call| call.args == [Type::InferInt]),
        "{:#?}",
        result.generic_calls
    );
}

#[test]
fn method_generic_can_shadow_impl_generic() {
    let result = check(
        r#"
        pub struct C<T> {}

        impl<T> C<T> {
            fun test<T>(&self, f: T) {}
        }

        pub fun f1() {
            let c = C {};
            c.test("1");
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert!(
        result
            .generic_calls
            .values()
            .any(|call| { call.args == [Type::Unknown, Type::Ref(Box::new(Type::Str), false)] }),
        "{:#?}",
        result.generic_calls
    );
}

#[test]
fn infers_generic_function_parameter_from_anonymous_function() {
    let result = check(
        r#"
        fun test<T>(value: T, f: impl Fn(T) -> T) { f(value); }

        fun main() {
            test(1, fun(x) { x + 1 });
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
    let result = check(
        r#"
        struct Wrap<T> {
            inner: T,
        }

        fun f<T>(x: T) -> T {
            return g(Wrap { inner: x });
        }

        fun g<T>(x: T) -> T {
            return f(Wrap { inner: x });
        }

        fun main() -> i32 {
            return f(0i32);
        }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diag| {
        diag.message
            .contains("generic recursion grows type arguments")
    }));
}

#[test]
fn infers_and_calls_anonymous_function() {
    let result = check(
        r#"
        fun apply(f: impl Fn(i32) -> i32, value: i32) -> i32 {
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
fn captures_pattern_bindings() {
    let result = check(
        r#"
        fun main() -> i32 {
            match 1 {
                base => {
                    let read = fun() { base };
                    read()
                }
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    let capture = &result.lambda_infos.values().next().unwrap().captures[0];
    assert_eq!(capture.name, "base");
    assert!(matches!(capture.place.source, CaptureSource::Pattern(_)));
    assert_eq!(capture.mode, CaptureMode::Shared);
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
    let source = "fun main() { let mut total = 0; let add = fun() { total += 1; }; add(); }";
    let result = check(source);

    let diagnostic = result
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == "E0031" && diagnostic.message.contains("mutable closure")
        })
        .unwrap();
    assert_eq!(
        diagnostic.notes,
        ["add `mut` to the closure binding because calling it may update captured state"]
    );
    assert_eq!(
        &source[diagnostic.labels[0].range], "add",
        "the primary label should point at the immutable binding"
    );
    assert_eq!(
        usize::from(diagnostic.labels[0].range.start()),
        source.find("add").unwrap()
    );
    assert_eq!(
        diagnostic.labels[1].style,
        type_checker::LabelStyle::Secondary
    );
    assert_eq!(&source[diagnostic.labels[1].range], "add");
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
fn by_value_method_receiver_makes_once_closure() {
    let result = check(
        r#"
        struct Token { value: i32 }
        impl Token {
            fun consume(self) -> i32 { self.value }
        }
        fun main() {
            let token = Token { value: 1 };
            let once = fun() { token.consume() };
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    let info = result.lambda_infos.values().next().unwrap();
    assert_eq!(info.kind, ClosureKind::FnOnce);
    assert_eq!(info.captures[0].mode, CaptureMode::Value);
}

#[test]
fn once_closure_is_rejected_at_fn_boundary() {
    let result = check(
        r#"
        struct Token { value: i32 }
        fun consume(value: Token) {}
        fun call(callback: impl Fn() -> ()) { callback(); }
        fun main() {
            let token = Token { value: 1 };
            let once = fun() { consume(token); };
            call(once);
        }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E0035"
            && diagnostic.message.contains("callable bound")
            && diagnostic.message.contains("FnOnce")
    }));
}

#[test]
fn nested_closure_does_not_capture_inner_parameters_in_outer_environment() {
    let result = check(
        r#"
        fun nested(base: i32) -> impl Fn(i32) -> impl Fn(i32) -> i32 {
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

#[test]
fn destructuring_let_binds_every_element() {
    let result = check(
        r#"
        struct Point { x: i32, y: bool }

        fun main() {
            let (a, b) = (1i32, true);
            let Point { x, y } = Point { x: 2i32, y: false };
            let flags: bool = b && y;
            let sum: i32 = a + x;
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn reference_patterns_use_rust_binding_modes() {
    let explicit = check("fun copy(value: &mut i32) -> i32 { let &mut copy = value; copy }");
    assert_eq!(explicit.diagnostics, vec![]);
    assert_eq!(
        explicit.pattern_binding_types.values().collect::<Vec<_>>(),
        vec![&Type::Int(IntTy::I32)]
    );
    assert_eq!(
        explicit.pattern_binding_modes.values().collect::<Vec<_>>(),
        vec![&PatternBindingMode::Move]
    );

    let ergonomic = check(
        r#"
        fun shared(value: &(i32, bool)) -> i32 {
            let (number, flag) = value;
            if *flag { *number } else { 0 }
        }
        fun mutable(value: &mut (i32, i32)) -> i32 {
            let (left, right) = value;
            *left + *right
        }
        "#,
    );
    assert_eq!(ergonomic.diagnostics, vec![]);
    let binding_types = ergonomic
        .pattern_binding_types
        .values()
        .cloned()
        .collect::<Vec<_>>();
    assert!(binding_types.contains(&Type::Ref(Box::new(Type::Int(IntTy::I32)), false)));
    assert!(binding_types.contains(&Type::Ref(Box::new(Type::Bool), false)));
    assert!(binding_types.contains(&Type::Ref(Box::new(Type::Int(IntTy::I32)), true)));
    assert!(
        ergonomic
            .pattern_binding_modes
            .values()
            .any(|mode| *mode == PatternBindingMode::Ref)
    );
    assert!(
        ergonomic
            .pattern_binding_modes
            .values()
            .any(|mode| *mode == PatternBindingMode::RefMut)
    );

    let bare = check("fun keep(value: &mut i32) -> i32 { let kept = value; *kept }");
    assert_eq!(bare.diagnostics, vec![]);
    assert_eq!(
        bare.pattern_binding_modes.values().collect::<Vec<_>>(),
        vec![&PatternBindingMode::Move]
    );
}

#[test]
fn rust_2024_rejects_binding_modifiers_inside_ergonomic_patterns() {
    let result = check(
        r#"
        fun mutable_binding(value: &(i32,)) {
            let (mut item,) = value;
        }
        fun explicit_inside_implicit(value: &(&mut i32,)) {
            let (&mut item,) = value;
        }
        fun mismatched_reference(value: &mut i32) {
            let &item = value;
        }
        "#,
    );

    let errors = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0010")
        .count();
    assert_eq!(errors, 3, "{:#?}", result.diagnostics);
}

#[test]
fn nested_and_named_patterns_inherit_reference_binding_modes() {
    let result = check(
        r#"
        struct Pair { left: i32, right: i32 }
        enum Maybe { Some(i32), None }

        fun nested_shared(value: & &mut (i32,)) -> i32 {
            let (item,) = value;
            *item
        }
        fun nested_explicit(value: & &mut i32) -> i32 {
            let &&mut item = value;
            item
        }
        fun fields(value: &mut Pair) -> i32 {
            let Pair { left, right } = value;
            *left + *right
        }
        fun variants(value: &Maybe) -> i32 {
            match value {
                Some(item) => *item,
                None => 0,
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert!(
        result
            .pattern_binding_modes
            .values()
            .any(|mode| { *mode == PatternBindingMode::Ref })
    );
    assert!(
        result
            .pattern_binding_modes
            .values()
            .filter(|mode| **mode == PatternBindingMode::RefMut)
            .count()
            >= 2
    );
    assert!(
        result
            .pattern_binding_types
            .values()
            .any(|ty| { *ty == Type::Ref(Box::new(Type::Int(IntTy::I32)), false) })
    );
}

#[test]
fn delayed_let_rejects_reference_destructuring() {
    let result = check(
        r#"
        fun delayed() {
            let (implicit,): &(i32,);
            let &(explicit,): &(i32,);
        }
        "#,
    );

    let errors = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0010")
        .count();
    assert_eq!(errors, 2, "{:#?}", result.diagnostics);
}

#[test]
fn destructuring_let_checks_element_types() {
    let result = check(
        r#"
        fun main() {
            let (a, b) = (1i32, true);
            let wrong: i32 = b;
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0001"),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn duplicate_bindings_in_one_pattern_are_rejected() {
    let result = check(
        r#"
        fun main() {
            let (value, value) = (1i32, 2i32);
        }
        "#,
    );

    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0058" && diagnostic.message.contains("bound more than once")
        }),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn duplicate_bindings_are_rejected_in_match_patterns() {
    let result = check(
        r#"
        fun main() {
            match (1i32, 2i32) {
                (value, value) => {},
            }
        }
        "#,
    );

    let duplicates = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0058")
        .count();
    assert_eq!(duplicates, 1, "{:#?}", result.diagnostics);
}

#[test]
fn mut_on_a_destructured_binding_allows_reassignment() {
    let result = check(
        r#"
        fun main() {
            let (mut a, b) = (1i32, 2i32);
            a = a + b;
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn immutable_destructured_binding_rejects_reassignment() {
    let result = check(
        r#"
        fun main() {
            let (a, b) = (1i32, 2i32);
            a = b;
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0031"),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn rejects_refutable_patterns_in_let_bindings() {
    let result = check(
        r#"
        enum Opt { None, Some(i32) }

        fun main() {
            let value = Opt::Some(1i32);
            let Opt::Some(inner) = value;
        }
        "#,
    );

    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0057" && diagnostic.message.contains("Opt::None")
        }),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn accepts_irrefutable_patterns_in_let_bindings() {
    let result = check(
        r#"
        struct Wrapper { inner: i32 }

        fun main() {
            let ((a, b), c) = ((1i32, 2i32), 3i32);
            let Wrapper { inner } = Wrapper { inner: a + b + c };
            let total: i32 = inner;
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn wrong_tuple_arity_in_let_reports_only_the_shape_error() {
    let result = check(
        r#"
        fun main() {
            let (a, b, c) = (1i32, 2i32);
        }
        "#,
    );

    let codes = result
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect::<Vec<_>>();
    assert_eq!(codes, vec!["E0010"], "{:#?}", result.diagnostics);
}

#[test]
fn accepts_let_bindings_with_delayed_initialization() {
    let result = check(
        r#"
        fun main() -> i32 {
            let value: i32;
            value = 7;
            value
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![], "{:#?}", result.diagnostics);
}

#[test]
fn infers_delayed_binding_type_from_first_assignment() {
    let result = check(
        r#"
        fun main() -> i32 {
            let value;
            value = 7;
            value
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![], "{:#?}", result.diagnostics);
}

#[test]
fn delayed_binding_without_a_type_constraint_is_rejected() {
    let result = check("fun main() { let value; }");

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E0045" && diagnostic.message.contains("delayed `let`")
    }));
}

#[test]
fn checks_const_initializer_type() {
    let result = check(
        r#"
        const ANSWER: i32 = true;
        fun main() {}
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0001"),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn rejects_non_constant_initializers_and_const_cycles() {
    let impure = check(
        r#"
        fun make() -> i32 { 1 }
        const BAD: i32 = make();
        "#,
    );
    assert!(
        impure
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0060"),
        "{:#?}",
        impure.diagnostics
    );

    let cycle = check(
        r#"
        const FIRST: i32 = SECOND;
        const SECOND: i32 = FIRST;
        "#,
    );
    assert!(
        cycle.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0060" && diagnostic.message.contains("initialization cycle")
        }),
        "{:#?}",
        cycle.diagnostics
    );
}

#[test]
fn resolves_top_level_aliases_qualified_types_and_imported_types() {
    let result = check(
        r#"
        type Number = i32;

        mod model {
            pub struct Point { pub value: i32 }
            pub type PublicPoint = Point;
        }

        use model::Point as ImportedPoint;

        fun read(first: model::Point, second: model::PublicPoint, third: ImportedPoint) -> Number {
            first.value + second.value + third.value
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn nested_private_types_do_not_leak_into_outer_type_scope() {
    let result = check(
        r#"
        mod hidden {
            struct Secret {}
            pub fun make() -> Secret { Secret {} }
        }

        fun expose(value: Secret) {}
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0034"),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn self_application_reports_an_infinite_type_without_overflowing() {
    let result = check(
        r#"
        fun main() {
            let id = fun(value) { value };
            id(id);
        }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E0046" && diagnostic.message == "cannot construct an infinite type"
    }));
}

#[test]
fn different_anonymous_function_expressions_have_different_types() {
    let result = check(
        r#"
        fun main() {
            let first = fun(value: i32) { value };
            let second = fun(value: i32) { value };
            let selected = if true { first } else { second };
        }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E0002" && diagnostic.message.contains("incompatible types")
    }));
}

#[test]
fn impl_fn_accepts_closures_and_safe_named_functions() {
    let result = check(
        r#"
        fun apply(f: impl Fn(i32) -> i32, value: i32) -> i32 { f(value) }
        fun increment(value: i32) -> i32 { value + 1 }

        fun main() -> i32 {
            let add_two = fun(value: i32) { value + 2 };
            apply(increment, 39) + apply(add_two, 0)
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn callable_generic_arguments_are_fully_resolved() {
    let result = check(
        r#"
        fun run_mut(mut f: impl FnMut(i32) -> i32, value: i32) -> i32 {
            f(value);
            f(value)
        }
        fun main() -> i32 {
            let mut total: i32 = 0;
            let add = fun(value: i32) { total += value; total };
            run_mut(add, 2)
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert!(
        result.generic_calls.values().any(|call| {
            call.args.iter().any(|arg| {
                matches!(
                    arg,
                    Type::Closure { signature, .. }
                        if signature.ret.as_ref() == &Type::Int(IntTy::I32)
                )
            })
        }),
        "{:#?}",
        result.generic_calls
    );
}

#[test]
fn explicit_callable_bound_can_name_one_callable_type() {
    let result = check(
        r#"
        fun call_twice<F>(mut f: F, value: i32) -> i32
        where F: FnMut(i32) -> i32
        {
            f(value);
            f(value)
        }
        fun main() -> i32 { call_twice(fun(value: i32) { value + 1 }, 1) }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn return_position_impl_fn_has_one_hidden_type() {
    let valid = check(
        r#"
        fun make(base: i32) -> impl Fn(i32) -> i32 {
            move fun(value: i32) { base + value }
        }
        fun main() -> i32 { make(40)(2) }
        "#,
    );
    assert_eq!(valid.diagnostics, vec![]);

    let invalid = check(
        r#"
        fun choose(flag: bool) -> impl Fn(i32) -> i32 {
            if flag {
                fun(value: i32) { value + 1 }
            } else {
                fun(value: i32) { value + 2 }
            }
        }
        "#,
    );
    assert!(
        invalid
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("opaque callable return") })
    );
}

#[test]
fn each_impl_fn_parameter_has_an_independent_hidden_type() {
    let result = check(
        r#"
        fun combine(
            first: impl Fn(i32) -> i32,
            second: impl Fn(i32) -> i32,
            value: i32
        ) -> i32 {
            first(value) + second(value)
        }
        fun main() -> i32 {
            combine(
                fun(value: i32) { value + 1 },
                fun(value: i32) { value + 2 },
                1,
            )
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn callable_capability_requirements_follow_the_fn_hierarchy() {
    let result = check(
        r#"
        struct Token { value: i32 }
        fun needs_fn(f: impl Fn() -> i32) -> i32 { f() }
        fun needs_mut(mut f: impl FnMut() -> i32) -> i32 { f() }
        fun needs_once(f: impl FnOnce() -> i32) -> i32 { f() }
        fun consume(value: Token) -> i32 { value.value }

        fun main() {
            let shared = fun() { 1 };
            let shared_for_mut = fun() { 1 };
            let shared_for_once = fun() { 1 };
            let mut total = 0;
            let mutable = fun() { total += 1; total };
            let mut other_total = 0;
            let mutable_for_once = fun() { other_total += 1; other_total };
            let token = Token { value: 2 };
            let once = fun() { consume(token) };
            needs_fn(shared);
            needs_mut(shared_for_mut);
            needs_once(shared_for_once);
            needs_mut(mutable);
            needs_once(mutable_for_once);
            needs_once(once);
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn move_capture_does_not_make_a_read_only_closure_fn_once() {
    let result = check(
        r#"
        struct Token { value: i32 }
        fun main() {
            let token = Token { value: 1 };
            let read = move fun() { token.value };
            read();
            read();
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    let info = result.lambda_infos.values().next().unwrap();
    assert_eq!(info.kind, ClosureKind::Fn);
    assert_eq!(info.captures[0].mode, CaptureMode::Value);
}

#[test]
fn mutable_callable_parameter_requires_mut() {
    let result = check(
        r#"
        fun run(f: impl FnMut() -> i32) -> i32 { f() }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E0031" && diagnostic.message.contains("immutable parameter")
    }));
}

#[test]
fn ordinary_mutable_parameter_can_be_assigned() {
    let result = check(
        r#"
        fun increment(mut value: i32) -> i32 {
            value += 1;
            value
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn wildcard_match_does_not_make_a_closure_fn_once() {
    let result = check(
        r#"
        struct Token { value: i32 }
        fun main() {
            let token = Token { value: 1 };
            let inspect = fun() { match token { _ => 0 } };
            inspect();
            inspect();
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(
        result.lambda_infos.values().next().unwrap().kind,
        ClosureKind::Fn
    );
}

#[test]
fn captures_only_the_referenced_struct_field() {
    let result = check(
        r#"
        struct Pair { left: i32, right: i32 }
        fun main() {
            let pair = Pair { left: 1, right: 2 };
            let read = fun() { pair.left };
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    let capture = &result.lambda_infos.values().next().unwrap().captures[0];
    assert_eq!(
        capture.place.projections.as_slice(),
        &[hir::place::Projection::Field(0)]
    );
}

#[test]
fn dynamic_index_and_deref_stop_capture_projection_at_their_base() {
    let indexed = check(
        r#"
        fun inspect(values: [i32; 2], index: usize) {
            let read = fun() { values[index] };
        }
        "#,
    );
    assert_eq!(indexed.diagnostics, vec![]);
    let indexed_info = indexed.lambda_infos.values().next().unwrap();
    let values = indexed_info
        .captures
        .iter()
        .find(|capture| capture.name == "values")
        .unwrap();
    assert!(values.place.projections.is_empty());

    let dereferenced = check(
        r#"
        fun inspect(value: &i32) {
            let read = fun() { *value };
        }
        "#,
    );
    assert_eq!(dereferenced.diagnostics, vec![]);
    let capture = &dereferenced.lambda_infos.values().next().unwrap().captures[0];
    assert!(capture.place.projections.is_empty());
    assert!(matches!(capture.ty, Type::Ref(_, false)));
}
