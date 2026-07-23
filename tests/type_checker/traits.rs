use crate::{check, check_with_package_ranges, messages};

#[test]
fn accepts_matching_trait_impl_required_items() {
    let result = check(
        r#"
        trait Show {
            fun show(value: i32) -> &str;
            type Output;
            type Default = bool;
        }

        struct Widget {}

        impl Show for Widget {
            fun show(value: i32) -> &str {
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

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("parameter 1 type mismatch"))
    );
    assert!(msgs.iter().any(|msg| msg.contains("return type mismatch")));
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("missing associated type `Item`"))
    );
}

#[test]
fn accepts_inherent_method_call_with_self_receiver() {
    let result = check(
        r#"
        enum Foo {
            A,
            B,
        }

        #[lang = "partial_eq"]
        trait PartialEq {}

        impl PartialEq for Foo {}

        impl Foo {
            fun get(&self) -> &str {
                if *self == Foo::A {
                    "A"
                } else {
                    "B"
                }
            }
        }

        fun main() {
            let x = Foo::A;
            let t = x.get();
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn mutable_self_method_requires_mutable_receiver_binding() {
    let result = check(
        r#"
        struct Cell {
            value: i32,
        }

        impl Cell {
            fun set(&mut self, value: i32) {
                self.value = value;
            }
        }

        fun main() {
            let cell = Cell { value: 1 };
            cell.set(42);
        }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diag| diag.code == "E0031"));
}

#[test]
fn mutable_reference_requires_mutable_binding() {
    let result = check(
        r#"
        struct Cell {
            value: i32,
        }

        fun main() {
            let cell = Cell { value: 1 };
            let ref_cell = &mut cell;
        }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diag| diag.code == "E0031"));
}

#[test]
fn resolves_impl_associated_type_path() {
    let result = check(
        r#"
        struct Foo {}

        trait Bar {
            type X;
        }

        impl Bar for Foo {
            type X = i32;
        }

        fun main() {
            let r = 10 as Foo::X;
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert!(
        result
            .expr_types
            .values()
            .any(|ty| matches!(ty, type_checker::Type::Int(type_checker::IntTy::I32)))
    );
}

#[test]
fn accepts_add_operator_impl_call() {
    let result = check(
        r#"
        #[lang = "add"]
        trait Add {
            type Output;
            fun add(self, rhs: Self) -> Self::Output;
        }

        struct Box<T> {
            value: T,
        }

        impl Add for Box<i32> {
            type Output = i32;

            fun add(self, rhs: Self) -> Self::Output {
                self.value + rhs.value
            }
        }

        fun main() {
            let a: Box<i32> = Box { value: 1 };
            let b: Box<i32> = Box { value: 2 };
            let sum: i32 = a + b;
            let c: Box<i32> = Box { value: 3 };
            let d: Box<i32> = Box { value: 4 };
            let direct: i32 = c.add(d);
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.operator_calls.len(), 1);
}

#[test]
fn operator_traits_accept_defaulted_and_heterogeneous_rhs() {
    let result = check(
        r#"
        #[lang = "add"]
        trait Add<Rhs = Self> {
            type Output;
            fun add(self, rhs: Rhs) -> Self::Output;
        }

        struct Number { value: i32 }
        struct Delta { value: i32 }

        impl Add for Number {
            type Output = Number;
            fun add(self, rhs: Self) -> Self::Output {
                Number { value: self.value + rhs.value }
            }
        }

        impl Add<Delta> for Number {
            type Output = i32;
            fun add(self, rhs: Delta) -> Self::Output {
                self.value + rhs.value
            }
        }

        fun main() {
            let same: Number = Number { value: 1 } + Number { value: 2 };
            let mixed: i32 = Number { value: 3 } + Delta { value: 4 };
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.operator_calls.len(), 2);
}

#[test]
fn trait_type_arguments_respect_required_and_defaulted_parameters() {
    let result = check(
        r#"
        trait Required<T> {}
        trait Defaulted<T = i32> {}
        struct Item {}

        impl Required for Item {}
        impl Required<i32, bool> for bool {}
        impl Defaulted for Item {}
        "#,
    );

    let arity_errors = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0032")
        .collect::<Vec<_>>();
    assert_eq!(arity_errors.len(), 2, "{:?}", result.diagnostics);
    assert!(
        arity_errors
            .iter()
            .any(|diagnostic| diagnostic.message.contains("got 0"))
    );
    assert!(
        arity_errors
            .iter()
            .any(|diagnostic| diagnostic.message.contains("got 2"))
    );
}

#[test]
fn supertrait_impl_requires_matching_trait_arguments() {
    let result = check(
        r#"
        trait Parent<Rhs = Self> {}
        trait Child<Rhs = Self>: Parent<Rhs> {}

        struct Left {}
        struct Right {}
        struct Other {}

        impl Parent<Other> for Left {}
        impl Child<Right> for Left {}
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0036"),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn generic_supertrait_bound_substitutes_parent_arguments() {
    let result = check(
        r#"
        trait Parent<X> {
            fun parent(&self, value: X) -> X;
        }

        trait Child<Y>: Parent<Y> {}

        struct Item {}

        impl Parent<i32> for Item {
            fun parent(&self, value: i32) -> i32 { value }
        }

        impl Child<i32> for Item {}

        fun use_parent<T: Child<i32>>(value: T) -> i32 {
            value.parent(1)
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn overloaded_operator_checks_rhs_once() {
    let result = check(
        r#"
        #[lang = "add"]
        trait Add<Rhs = Self> {
            type Output;
            fun add(self, rhs: Rhs) -> Self::Output;
        }

        struct Number { value: i32 }

        impl Add for Number {
            type Output = Number;
            fun add(self, rhs: Self) -> Self::Output { self }
        }

        fun main() {
            let value = Number { value: 1 } + 1();
        }
        "#,
    );

    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "E0004")
            .count(),
        1
    );
}

#[test]
fn rejects_duplicate_trait_impls() {
    let result = check(
        r#"
        #[lang = "add"]
        trait Add {
            type Output;
            fun add(self, rhs: Self) -> Self::Output;
        }

        struct Number { value: i32 }

        impl Add for Number {
            type Output = Number;
            fun add(self, rhs: Self) -> Self::Output { self }
        }

        impl Add for Number {
            type Output = Number;
            fun add(self, rhs: Self) -> Self::Output { rhs }
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0047")
    );
}

#[test]
fn rejects_overlapping_trait_impls() {
    let result = check(
        r#"
        #[lang = "add"]
        trait Add<Rhs = Self> {
            type Output;
            fun add(self, rhs: Rhs) -> Self::Output;
        }

        struct Number { value: i32 }
        struct Delta { value: i32 }

        impl<R> Add<R> for Number {
            type Output = Number;
            fun add(self, rhs: R) -> Self::Output { self }
        }

        impl Add<Delta> for Number {
            type Output = Number;
            fun add(self, rhs: Delta) -> Self::Output { self }
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0047")
    );
}

#[test]
fn accepts_disjoint_trait_impls() {
    let result = check(
        r#"
        #[lang = "add"]
        trait Add<Rhs = Self> {
            type Output;
            fun add(self, rhs: Rhs) -> Self::Output;
        }

        struct Number { value: i32 }
        struct Delta { value: i32 }
        struct Offset { value: i32 }

        impl Add<Delta> for Number {
            type Output = Number;
            fun add(self, rhs: Delta) -> Self::Output { self }
        }

        impl Add<Offset> for Number {
            type Output = Number;
            fun add(self, rhs: Offset) -> Self::Output { self }
        }
        "#,
    );

    assert!(
        !result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0047")
    );
}

#[test]
fn accepts_disjoint_impls_with_shared_generic_relationship() {
    let result = check(
        r#"
        trait Marker<A> {}

        impl<T> Marker<T> for T {}
        impl Marker<i32> for bool {}
        "#,
    );

    assert!(
        !result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0047")
    );
}

#[test]
fn enforces_orphan_rules_per_package() {
    let foreign = r#"
        trait Foreign<T = i32> {}
        struct ForeignType {}
        struct ForeignBox<T> { value: T }
        #[fundamental]
        struct FundBox<T> { value: T }
    "#;
    let local = r#"
        struct Local<T> { value: T }
        trait LocalTrait {}

        impl Foreign for ForeignType {}
        impl<T> Foreign<T> for Local<T> {}
        impl LocalTrait for ForeignType {}
        impl<T> Foreign<Local<T>> for T {}
        impl Foreign for &Local<i32> {}
        impl Foreign for ForeignBox<Local<i32>> {}
        impl<T> Foreign<Local<i32>> for ForeignBox<T> {}
        impl Foreign for FundBox<Local<i32>> {}
    "#;
    let source = format!("{foreign}{local}");
    let result =
        check_with_package_ranges(&source, &[0..foreign.len(), foreign.len()..source.len()]);
    let orphan_errors = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0048")
        .collect::<Vec<_>>();

    assert_eq!(orphan_errors.len(), 4, "{:?}", result.diagnostics);
    assert!(
        orphan_errors
            .iter()
            .any(|diagnostic| diagnostic.message.contains("foreign type `ForeignType`"))
    );
    assert!(
        orphan_errors
            .iter()
            .any(|diagnostic| diagnostic.message.contains("type parameter `T`"))
    );
    assert!(
        orphan_errors
            .iter()
            .any(|diagnostic| diagnostic.message.contains("foreign type `ForeignBox"))
    );
}

#[test]
fn rejects_arbitrary_composite_trait_bound() {
    let result = check(
        r#"
        trait Marker {}

        impl Marker for i32 {}

        fun accepts_marker<T: Marker>(value: T) {}

        fun main() {
            accepts_marker((1i32, 2i32));
            accepts_marker([1i32, 2i32]);
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0035"),
        "expected tuple to be rejected for Marker: {:?}",
        result.diagnostics
    );
}

#[test]
fn comparison_traits_accept_heterogeneous_rhs() {
    let result = check(
        r#"
        #[lang = "partial_eq"]
        trait PartialEq<Rhs = Self> {
            fun eq(&self, other: &Rhs) -> bool;
            fun ne(&self, other: &Rhs) -> bool { !self.eq(other) }
        }

        struct Left { value: i32 }
        struct Right { value: i32 }

        impl PartialEq<Right> for Left {
            fun eq(&self, other: &Right) -> bool {
                self.value == other.value
            }
        }

        fun main() {
            let left = Left { value: 1 };
            let right = Right { value: 1 };
            let equal: bool = left == right;
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.operator_calls.len(), 1);
}

#[test]
fn composite_comparison_preserves_heterogeneous_rhs() {
    let result = check(
        r#"
        #[lang = "partial_eq"]
        trait PartialEq<Rhs = Self> {
            fun eq(&self, other: &Rhs) -> bool;
        }

        struct Left { value: i32 }
        struct Right { value: i32 }

        impl PartialEq for i32 {
            fun eq(&self, other: &i32) -> bool {
                *self == *other
            }
        }

        impl PartialEq<Right> for Left {
            fun eq(&self, other: &Right) -> bool {
                self.value == other.value
            }
        }

        fun main() {
            let left: (Left, i32) = (Left { value: 1 }, 2);
            let right: (Right, i32) = (Right { value: 1 }, 2);
            let equal: bool = left == right;
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_binary_unary_and_assign_operator_impls() {
    let result = check(
        r#"
        #[lang = "sub"]
        trait Sub { type Output; fun sub(self, rhs: Self) -> Self::Output; }
        #[lang = "neg"]
        trait Neg { type Output; fun neg(self) -> Self::Output; }
        #[lang = "add_assign"]
        trait AddAssign { fun add_assign(&mut self, rhs: Self); }

        struct Number { value: i32 }

        impl Sub for Number {
            type Output = Number;
            fun sub(self, rhs: Self) -> Self::Output {
                Number { value: self.value - rhs.value }
            }
        }
        impl Neg for Number {
            type Output = Number;
            fun neg(self) -> Self::Output {
                Number { value: -self.value }
            }
        }
        impl AddAssign for Number {
            fun add_assign(&mut self, rhs: Self) {
                self.value += rhs.value;
            }
        }

        fun main() {
            let left = Number { value: 7 };
            let right = Number { value: 2 };
            let difference = left - right;
            let negated = -difference;
            let mut total = Number { value: 10 };
            total += negated;
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.operator_calls.len(), 3);
}

#[test]
fn rejects_add_impl_missing_add_method() {
    let result = check(
        r#"
        #[lang = "add"]
        trait Add {
            type Output;
            fun add(self, rhs: Self) -> Self::Output;
        }

        struct Box<T> {
            value: T,
        }

        impl Add for Box<i32> {
            type Output = i32;
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter().any(|msg| msg.contains("missing method `add`")),
        "{msgs:?}"
    );
}

#[test]
fn accepts_generic_add_impl_with_output_bound() {
    let result = check(
        r#"
        #[lang = "add"]
        trait Add {
            type Output;
            fun add(self, rhs: Self) -> Self::Output;
        }

        impl Add for i32 {
            type Output = i32;
            fun add(self, rhs: Self) -> Self::Output {
                self + rhs
            }
        }

        struct Box<T> {
            value: T,
        }

        impl<T: Add<Output = T>> Add for Box<T> {
            type Output = T;

            fun add(self, rhs: Self) -> Self::Output {
                self.value + rhs.value
            }
        }

        fun main() {
            let a: Box<i32> = Box { value: 1 };
            let b: Box<i32> = Box { value: 2 };
            let sum: i32 = a + b;
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.operator_calls.len(), 2);
}

#[test]
fn generic_add_impl_respects_type_argument_bound() {
    let result = check(
        r#"
        #[lang = "add"]
        trait Add {
            type Output;
            fun add(self, rhs: Self) -> Self::Output;
        }

        impl Add for i32 {
            type Output = i32;
            fun add(self, rhs: Self) -> Self::Output {
                self + rhs
            }
        }

        struct Box<T> {
            value: T,
        }

        impl<T: Add<Output = T>> Add for Box<T> {
            type Output = T;

            fun add(self, rhs: Self) -> Self::Output {
                self.value + rhs.value
            }
        }

        fun main() {
            let a: Box<bool> = Box { value: true };
            let b: Box<bool> = Box { value: false };
            let sum = a + b;
        }
        "#,
    );

    assert!(
        !result.diagnostics.is_empty(),
        "Box<bool> must not satisfy Box<T>'s Add impl when bool lacks Add"
    );
}

#[test]
fn rejects_generic_add_impl_without_add_bound() {
    let result = check(
        r#"
        #[lang = "add"]
        trait Add {
            type Output;
            fun add(self, rhs: Self) -> Self::Output;
        }

        struct Box<T> {
            value: T,
        }

        impl<T> Add for Box<T> {
            type Output = T;

            fun add(self, rhs: Self) -> Self::Output {
                self.value + rhs.value
            }
        }
        "#,
    );

    assert!(
        !result.diagnostics.is_empty(),
        "generic add without a T: Add bound should be rejected"
    );
}

#[test]
fn checks_partial_eq_for_user_equality() {
    let result = check(
        r#"
        #[lang = "partial_eq"]
        trait PartialEq {}

        enum Foo {
            A,
            B,
        }

        fun main() {
            let a = Foo::A;
            let b = Foo::B;
            let same = a == b;
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("must implement `PartialEq`")),
        "{msgs:?}"
    );

    let result = check(
        r#"
        #[lang = "partial_eq"]
        trait PartialEq {}

        enum Foo {
            A,
            B,
        }

        impl PartialEq for Foo {}

        fun main() {
            let a = Foo::A;
            let b = Foo::B;
            let same = a == b;
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn checks_eq_marker_dependencies() {
    let result = check(
        r#"
        #[lang = "partial_eq"]
        trait PartialEq {}

        #[lang = "eq"]
        trait Eq: PartialEq {}

        #[lang = "partial_ord"]
        trait PartialOrd: PartialEq {}

        #[lang = "ord"]
        trait Ord: Eq + PartialOrd {}

        struct MissingEq {}
        struct MissingPartialOrd {}
        struct MissingOrdDeps {}

        impl Eq for MissingEq {}
        impl PartialOrd for MissingPartialOrd {}
        impl Ord for MissingOrdDeps {}
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter().any(|msg| msg.contains("requires `PartialEq`")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter().any(|msg| msg.contains("requires `Eq`")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter().any(|msg| msg.contains("requires `PartialOrd`")),
        "{msgs:?}"
    );
}

#[test]
fn checks_generic_trait_bounds() {
    let result = check(
        r#"
        trait Marker {}

        struct Good {}
        struct Bad {}

        impl Marker for Good {}

        fun accept<T: Marker>(value: T) -> T {
            value
        }

        fun main() {
            let good = Good {};
            let bad = Bad {};
            let ok = accept(good);
            let nope = accept(bad);
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("does not satisfy bound `Marker`")),
        "{msgs:?}"
    );
}

#[test]
fn checks_struct_and_enum_where_clause_bounds() {
    let result = check(
        r#"
        trait Marker {}

        struct Good {}
        struct Bad {}

        impl Marker for Good {}

        struct Box<T> where T: Marker {
            value: T,
        }

        enum Slot<T> where T: Marker {
            Some(T),
            None,
        }

        fun takes_marker<T: Marker>(value: T) {
            let ok: Box<T> = Box { value: value };
        }

        fun main() {
            let good_box = Box { value: Good {} };
            let good_slot = Slot::Some(Good {});
            let bad_box = Box { value: Bad {} };
            let bad_slot = Slot::Some(Bad {});
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("type `Bad` does not satisfy bound `Marker` for `Box`")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("type `Bad` does not satisfy bound `Marker` for `Slot`")),
        "{msgs:?}"
    );
}

#[test]
fn allows_trait_bound_method_call_in_generic_body() {
    let result = check(
        r#"
        trait Named {
            fun name(&self) -> i32;
        }

        struct User { id: i32 }

        impl Named for User {
            fun name(&self) -> i32 {
                self.id
            }
        }

        fun read<T: Named>(value: T) -> i32 {
            value.name()
        }

        fun main() {
            let user = User { id: 1 };
            let id = read(user);
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_iterator_next_protocol() {
    let result = check(
        r#"
        enum Option<T> {
            Some(T),
            None,
        }

        trait Iterator {
            type Item;
            fun next(&mut self) -> Option<Self::Item>;
        }

        struct Counter {
            current: i32,
        }

        impl Iterator for Counter {
            type Item = i32;

            fun next(&mut self) -> Option<Self::Item> {
                if self.current < 10 {
                    Option::Some(self.current)
                } else {
                    Option::None
                }
            }
        }

        fun main() {
            let mut counter = Counter { current: 0 };
            let value = counter.next();
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_for_loop_over_into_iterator() {
    let result = check(
        r#"
        enum Option<T> {
            Some(T),
            None,
        }

        trait Iterator {
            type Item;
            fun next(&mut self) -> Option<Self::Item>;
        }

        trait IntoIterator {
            type Item;
            type IntoIter;
            fun into_iter(self) -> Self::IntoIter;
        }

        struct Counter {
            current: i32,
        }

        impl Iterator for Counter {
            type Item = i32;

            fun next(&mut self) -> Option<Self::Item> {
                if self.current < 10 {
                    Option::Some(self.current)
                } else {
                    Option::None
                }
            }
        }

        impl IntoIterator for Counter {
            type Item = i32;
            type IntoIter = Counter;

            fun into_iter(self) -> Self::IntoIter {
                self
            }
        }

        fun main() {
            let counter = Counter { current: 0 };
            for item in counter {
                let next = item + 1;
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_for_loop_over_array() {
    let result = check(
        r#"
        fun main() {
            let values = [1, 2, 3];
            for item in values {
                let next = item + 1;
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn matches_const_generic_trait_impl_for_arrays() {
    let result = check(
        r#"
        trait Marker {}

        impl<T, const N: usize> Marker for [T; N] {}

        fun takes_marker<T: Marker>(value: T) {}

        fun main() {
            takes_marker([1, 2, 3]);
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn array_into_iterator_impl_type_checks_with_const_generics() {
    let result = check(
        r#"
        enum Option<T> {
            Some(T),
            None,
        }

        trait Iterator {
            type Item;
            fun next(&mut self) -> Option<Self::Item>;
        }

        trait IntoIterator {
            type Item;
            type IntoIter;
            fun into_iter(self) -> Self::IntoIter;
        }

        struct ArrayIter<T, const N: usize> {
            values: [T; N],
            index: usize,
        }

        impl<T, const N: usize> Iterator for ArrayIter<T, N> {
            type Item = T;

            fun next(&mut self) -> Option<Self::Item> {
                if self.index < N {
                    let value = self.values[self.index];
                    self.index += 1usize;
                    Option::Some(value)
                } else {
                    Option::None
                }
            }
        }

        impl<T, const N: usize> IntoIterator for [T; N] {
            type Item = T;
            type IntoIter = ArrayIter<T, N>;

            fun into_iter(self) -> Self::IntoIter {
                ArrayIter {
                    values: self,
                    index: 0usize,
                }
            }
        }

        struct Token {
            value: i32,
        }

        fun main() {
            let values = [Token { value: 1 }, Token { value: 2 }, Token { value: 3 }];
            let mut iter = values.into_iter();
            let first = iter.next();

            for item in [Token { value: 4 }, Token { value: 5 }] {
                let next = item.value + 1;
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn checks_multiple_generic_trait_bounds() {
    let result = check(
        r#"
        trait Named {
            fun name(&self) -> i32;
        }

        trait Tagged {
            fun tag(&self) -> i32;
        }

        struct Good { id: i32, tag_value: i32 }
        struct MissingTag { id: i32 }

        impl Named for Good {
            fun name(&self) -> i32 { self.id }
        }

        impl Tagged for Good {
            fun tag(&self) -> i32 { self.tag_value }
        }

        impl Named for MissingTag {
            fun name(&self) -> i32 { self.id }
        }

        fun read<T: Named + Tagged>(value: T) -> i32 {
            value.name() + value.tag()
        }

        fun main() {
            let good = Good { id: 1, tag_value: 2 };
            let missing = MissingTag { id: 3 };
            let ok = read(good);
            let err = read(missing);
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("does not satisfy bound `Tagged`")),
        "{msgs:?}"
    );
}

#[test]
fn accepts_where_clause_on_function_bound() {
    let result = check(
        r#"
        trait Named {
            fun name(&self) -> i32;
        }

        struct User { id: i32 }

        impl Named for User {
            fun name(&self) -> i32 {
                self.id
            }
        }

        fun read<T>(value: T) -> i32
        where T: Named
        {
            value.name()
        }

        fun main() {
            let user = User { id: 1 };
            let id = read(user);
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_where_clause_on_impl_bound() {
    let result = check(
        r#"
        trait Marker {}
        trait Wrap {}

        struct Box<T> { value: T }
        struct Bad {}

        impl Marker for i32 {}

        impl<T> Wrap for Box<T>
        where T: Marker
        {}

        fun takes_wrap<T: Wrap>(value: T) {}

        fun main() {
            takes_wrap(Box { value: 1 });
            takes_wrap(Box { value: Bad {} });
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("does not satisfy bound `Wrap`")),
        "{msgs:?}"
    );
}

#[test]
fn rejects_impl_where_clause_that_violates_paterson_condition() {
    let result = check(
        r#"
        trait Foo {}

        struct Vec<T> { value: T }

        impl<T> Foo for T
        where Vec<T>: Foo
        {}
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("not strictly smaller than implemented type")),
        "{msgs:?}"
    );
}

#[test]
fn reports_unknown_generic_trait_bound() {
    let result = check(
        r#"
        fun accept<T: Missing>(value: T) -> T {
            value
        }

        fun main() {
            let value = accept(1);
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("generic bound references unknown trait `Missing`")),
        "{msgs:?}"
    );
}

#[test]
#[should_panic(expected = "expected Greater")]
fn rejects_bounds_outside_function_generics_for_now() {
    let _ = check(
        r#"
        trait Marker {}
        struct Box<T: Marker> { value: T }
        "#,
    );
}

#[test]
fn accepts_for_loop_over_generic_into_iterator_bound() {
    let result = check(
        r#"
        enum Option<T> { Some(T), None }

        trait Iterator {
            type Item;
            fun next(&mut self) -> Option<Self::Item>;
        }

        trait IntoIterator {
            type Item;
            type IntoIter;
            fun into_iter(self) -> Self::IntoIter;
        }

        struct Counter { current: i32 }

        impl Iterator for Counter {
            type Item = i32;
            fun next(&mut self) -> Option<Self::Item> { Option::None }
        }

        impl IntoIterator for Counter {
            type Item = i32;
            type IntoIter = Counter;
            fun into_iter(self) -> Self::IntoIter { self }
        }

        fun consume<T: IntoIterator<Item = i32, IntoIter = Counter>>(values: T) {
            for value in values {
                let next = value + 1;
            }
        }

        fun main() {
            consume(Counter { current: 0 });
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn rejects_iterator_next_with_non_option_result() {
    let result = check(
        r#"
        enum Option<T> { Some(T), None }

        trait Iterator {
            type Item;
            fun next(&mut self) -> bool;
        }

        trait IntoIterator {
            type Item;
            type IntoIter;
            fun into_iter(self) -> Self::IntoIter;
        }

        struct Counter { current: i32 }

        impl Iterator for Counter {
            type Item = i32;
            fun next(&mut self) -> bool { false }
        }

        impl IntoIterator for Counter {
            type Item = i32;
            type IntoIter = Counter;
            fun into_iter(self) -> Self::IntoIter { self }
        }

        fun main() {
            let counter = Counter { current: 0 };
            for value in counter {
                let next = value + 1;
            }
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diag| diag.message.contains("Iterator::next"))
    );
}

#[test]
fn accepts_outer_attributes_on_common_ast_nodes() {
    let result = check(
        r#"
        #[item]
        struct Boxed {
            #[field]
            value: i32,
        }

        #[item]
        enum Option {
            #[variant]
            Some(#[variant_ty] i32),
            None,
        }

        #[item]
        fun id(#[param] value: #[ty] i32) -> i32 {
            let x = #[expr] value;
            match x {
                #[arm] #[pat] other => other,
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn supertrait_bound_exposes_parent_methods() {
    let result = check(
        r#"
        trait Named {
            fun name(&self) -> i32;
        }

        trait Tagged: Named {
            fun tag(&self) -> i32;
        }

        struct Item { value: i32 }

        impl Named for Item {
            fun name(&self) -> i32 { self.value }
        }

        impl Tagged for Item {
            fun tag(&self) -> i32 { self.value }
        }

        fun describe<T: Tagged>(value: T) -> i32 {
            value.name() + value.tag()
        }

        fun main() {
            let value = describe(Item { value: 1 });
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn supertrait_impl_requires_parent_impl() {
    let result = check(
        r#"
        trait Parent {}
        trait Child: Parent {}

        struct Item {}
        impl Child for Item {}
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("impl `Child` for `Item` requires `Parent`")),
        "{msgs:?}"
    );
}

#[test]
fn reports_invalid_supertraits() {
    let result = check(
        r#"
        trait MissingParent: Unknown {}
        trait First: Second {}
        trait Second: First {}
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("unknown supertrait `Unknown`")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter().any(|msg| msg.contains("supertrait cycle")),
        "{msgs:?}"
    );
}

#[test]
fn trait_default_method_can_call_required_method() {
    let result = check(
        r#"
        trait Value {
            fun base(&self) -> i32;
            fun value(&self) -> i32 {
                self.base() + 1
            }
        }

        struct Item {}
        impl Value for Item {
            fun base(&self) -> i32 { 6 }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}
