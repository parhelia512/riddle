use crate::check;

#[test]
fn reports_ptr_deref_outside_unsafe() {
    let result = check(
        r"
        fun read(ptr: *const i32) -> i32 {
            *ptr
        }
        ",
    );
    assert!(
        result.diagnostics.iter().any(|diag| diag.code == "E0046"),
        "expected E0046 for raw pointer deref, got {:#?}",
        result.diagnostics
    );
}

#[test]
fn reports_ptr_index_outside_unsafe() {
    let result = check(
        r"
        fun read(ptr: *const i32) -> i32 {
            ptr[0]
        }
        ",
    );
    assert!(
        result.diagnostics.iter().any(|diag| diag.code == "E0046"),
        "expected E0046 for raw pointer index, got {:#?}",
        result.diagnostics
    );
}

#[test]
fn reports_mut_ptr_deref_outside_unsafe() {
    let result = check(
        r"
        fun write(ptr: *mut i32) {
            *ptr = 42;
        }
        ",
    );
    assert!(
        result.diagnostics.iter().any(|diag| diag.code == "E0046"),
        "expected E0046 for mutable raw pointer deref, got {:#?}",
        result.diagnostics
    );
}

#[test]
fn reports_mut_ptr_index_outside_unsafe() {
    let result = check(
        r"
        fun write(ptr: *mut i32) {
            ptr[0] = 42;
        }
        ",
    );
    assert!(
        result.diagnostics.iter().any(|diag| diag.code == "E0046"),
        "expected E0046 for mutable raw pointer index, got {:#?}",
        result.diagnostics
    );
}

#[test]
fn accepts_ptr_deref_inside_unsafe() {
    let result = check(
        r"
        fun read(ptr: *const i32) -> i32 {
            unsafe { *ptr }
        }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_ptr_index_inside_unsafe() {
    let result = check(
        r"
        fun read(ptr: *const i32) -> i32 {
            unsafe { ptr[0] }
        }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_mut_ptr_deref_inside_unsafe() {
    let result = check(
        r"
        fun write(ptr: *mut i32) {
            unsafe { *ptr = 42; }
        }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_mut_ptr_index_inside_unsafe() {
    let result = check(
        r"
        fun write(ptr: *mut i32) {
            unsafe { ptr[0] = 42; }
        }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_dst_layout_casts_inside_unsafe() {
    let result = check(
        r"
        unsafe fun slice_from_parts<T>(data: *const T, len: usize) -> &[T] {
            unsafe { (data, len) as &[T] }
        }

        unsafe fun str_from_bytes(bytes: &[u8]) -> &str {
            unsafe { bytes as &str }
        }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_slice_to_raw_parts_casts_inside_unsafe() {
    let result = check(
        r"
        unsafe fun parts<T>(values: &[T]) -> usize {
            match unsafe { values as (*const T, usize) } {
                (_, len) => len,
            }
        }

        unsafe fun mut_parts<T>(values: &mut [T]) -> *mut T {
            match unsafe { values as (*mut T, usize) } {
                (data, _) => data,
            }
        }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn rejects_mutable_raw_parts_from_shared_slice() {
    let result = check(
        r"
        unsafe fun mut_parts<T>(values: &[T]) -> *mut T {
            match unsafe { values as (*mut T, usize) } {
                (data, _) => data,
            }
        }
        ",
    );
    assert!(
        !result.diagnostics.is_empty(),
        "shared slice must not yield a *mut part"
    );
}

#[test]
fn slice_to_raw_parts_casts_require_unsafe() {
    let result = check(
        r"
        fun parts<T>(values: &[T]) -> usize {
            match values as (*const T, usize) {
                (_, len) => len,
            }
        }
        ",
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "E0046")
            .count(),
        1,
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn str_to_byte_slice_layout_cast_is_safe() {
    let result = check(
        r"
        fun as_bytes(value: &str) -> &[u8] {
            value as &[u8]
        }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn dst_layout_casts_require_unsafe() {
    let result = check(
        r"
        fun slice_from_parts<T>(data: *const T, len: usize) -> &[T] {
            (data, len) as &[T]
        }

        fun str_from_bytes(bytes: &[u8]) -> &str {
            bytes as &str
        }
        ",
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "E0046")
            .count(),
        2,
        "{:#?}",
        result.diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "E0012"),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn nested_unsafe_blocks_work() {
    let result = check(
        r"
        fun read(ptr: *const i32) -> i32 {
            unsafe {
                unsafe {
                    *ptr
                }
            }
        }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn unsafe_block_is_an_expression() {
    let result = check(
        r"
        fun read(ptr: *const i32) -> i32 {
            let x = unsafe { 42 };
            x
        }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn unsafe_block_does_not_disable_type_checking() {
    let result = check(
        r"
        fun bad() -> i32 {
            unsafe { true + 1 }
        }
        ",
    );
    assert!(
        result.diagnostics.iter().any(|diag| diag.code == "E0003"),
        "expected type error inside unsafe block, got {:#?}",
        result.diagnostics
    );
}

#[test]
fn unsafe_block_does_not_disable_mutability_check() {
    let result = check(
        r"
        fun bad() {
            let x = 1;
            unsafe { x = 2; }
        }
        ",
    );
    assert!(
        result.diagnostics.iter().any(|diag| diag.code == "E0031"),
        "expected mutability error inside unsafe block, got {:#?}",
        result.diagnostics
    );
}

#[test]
fn safe_ref_deref_does_not_require_unsafe() {
    let result = check(
        r"
        fun read(r: &i32) -> i32 {
            *r
        }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn multiple_unsafe_operations_in_one_block() {
    let result = check(
        r"
        fun read(a: *const i32, b: *const i32) -> i32 {
            unsafe {
                *a + *b
            }
        }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn unsafe_function_call_requires_unsafe_block() {
    let rejected = check(
        r"
        unsafe fun dangerous(value: i32) -> i32 { value }
        fun main() -> i32 { dangerous(1) }
        ",
    );
    assert!(rejected.diagnostics.iter().any(|diag| diag.code == "E0046"));

    let accepted = check(
        r"
        unsafe fun dangerous(value: i32) -> i32 { value }
        fun main() -> i32 { unsafe { dangerous(1) } }
        ",
    );
    assert_eq!(accepted.diagnostics, vec![]);
}

#[test]
fn unsafe_function_body_still_requires_unsafe_block() {
    let result = check(
        r"
        unsafe fun read(ptr: *const i32) -> i32 { *ptr }
        ",
    );
    assert!(result.diagnostics.iter().any(|diag| diag.code == "E0046"));
}

#[test]
fn safe_function_item_satisfies_callable_bound() {
    let result = check(
        r"
        fun apply(f: impl Fn(i32) -> i32, value: i32) -> i32 { f(value) }
        fun normal(value: i32) -> i32 { value }
        fun main() -> i32 { apply(normal, 1) }
        ",
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn unsafe_method_call_requires_unsafe_block() {
    let result = check(
        r"
        struct Reader {}
        impl Reader {
            unsafe fun read(&self) -> i32 { 1 }
        }
        fun main() -> i32 { Reader {}.read() }
        ",
    );
    assert!(result.diagnostics.iter().any(|diag| diag.code == "E0046"));
}

#[test]
fn extern_imports_default_to_unsafe_and_allow_safe_opt_out() {
    let result = check(
        r#"
        unsafe extern "C" {
            safe fun abs(value: i32) -> i32;
            fun malloc(size: usize) -> *mut u8;
        }
        fun main() -> i32 {
            let value = abs(-1);
            malloc(1);
            value
        }
        "#,
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|diag| diag.code == "E0046")
            .count(),
        1
    );
}

#[test]
fn extern_definition_safety_follows_modifier() {
    let safe = check(
        r#"
        extern "C" fun exported() -> i32 { 1 }
        fun main() -> i32 { exported() }
        "#,
    );
    assert_eq!(safe.diagnostics, vec![]);

    let unsafe_result = check(
        r#"
        unsafe extern "C" fun exported() -> i32 { 1 }
        fun main() -> i32 { exported() }
        "#,
    );
    assert!(
        unsafe_result
            .diagnostics
            .iter()
            .any(|diag| diag.code == "E0046")
    );
}

#[test]
fn trait_impl_method_safety_must_match() {
    let result = check(
        r"
        trait RawRead {
            unsafe fun read(&self) -> i32;
        }
        struct Reader {}
        impl RawRead for Reader {
            fun read(&self) -> i32 { 1 }
        }
        ",
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diag| { diag.code == "E0028" && diag.message.contains("safety mismatch") })
    );
}

#[test]
fn unsafe_function_item_does_not_satisfy_safe_fn_bound() {
    let result = check(
        r"
        fun apply(f: impl Fn(i32) -> i32, value: i32) -> i32 { f(value) }
        unsafe fun dangerous(value: i32) -> i32 { value }
        fun main() -> i32 { apply(dangerous, 1) }
        ",
    );
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E0001"
            && diagnostic.message.contains("unsafe function")
            && diagnostic.message.contains("Fn")
    }));
}
