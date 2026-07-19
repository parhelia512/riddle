use crate::check;

#[test]
fn reports_ptr_deref_outside_unsafe() {
    let result = check(
        r#"
        fun read(ptr: *const i32) -> i32 {
            *ptr
        }
        "#,
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
        r#"
        fun read(ptr: *const i32) -> i32 {
            ptr[0]
        }
        "#,
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
        r#"
        fun write(ptr: *mut i32) {
            *ptr = 42;
        }
        "#,
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
        r#"
        fun write(ptr: *mut i32) {
            ptr[0] = 42;
        }
        "#,
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
        r#"
        fun read(ptr: *const i32) -> i32 {
            unsafe { *ptr }
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_ptr_index_inside_unsafe() {
    let result = check(
        r#"
        fun read(ptr: *const i32) -> i32 {
            unsafe { ptr[0] }
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_mut_ptr_deref_inside_unsafe() {
    let result = check(
        r#"
        fun write(ptr: *mut i32) {
            unsafe { *ptr = 42; }
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_mut_ptr_index_inside_unsafe() {
    let result = check(
        r#"
        fun write(ptr: *mut i32) {
            unsafe { ptr[0] = 42; }
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn nested_unsafe_blocks_work() {
    let result = check(
        r#"
        fun read(ptr: *const i32) -> i32 {
            unsafe {
                unsafe {
                    *ptr
                }
            }
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn unsafe_block_is_an_expression() {
    let result = check(
        r#"
        fun read(ptr: *const i32) -> i32 {
            let x = unsafe { 42 };
            x
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn unsafe_block_does_not_disable_type_checking() {
    let result = check(
        r#"
        fun bad() -> i32 {
            unsafe { true + 1 }
        }
        "#,
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
        r#"
        fun bad() {
            let x = 1;
            unsafe { x = 2; }
        }
        "#,
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
        r#"
        fun read(r: &i32) -> i32 {
            *r
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn multiple_unsafe_operations_in_one_block() {
    let result = check(
        r#"
        fun read(a: *const i32, b: *const i32) -> i32 {
            unsafe {
                *a + *b
            }
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn unsafe_function_call_requires_unsafe_block() {
    let rejected = check(
        r#"
        unsafe fun dangerous(value: i32) -> i32 { value }
        fun main() -> i32 { dangerous(1) }
        "#,
    );
    assert!(rejected.diagnostics.iter().any(|diag| diag.code == "E0046"));

    let accepted = check(
        r#"
        unsafe fun dangerous(value: i32) -> i32 { value }
        fun main() -> i32 { unsafe { dangerous(1) } }
        "#,
    );
    assert_eq!(accepted.diagnostics, vec![]);
}

#[test]
fn unsafe_function_body_still_requires_unsafe_block() {
    let result = check(
        r#"
        unsafe fun read(ptr: *const i32) -> i32 { *ptr }
        "#,
    );
    assert!(result.diagnostics.iter().any(|diag| diag.code == "E0046"));
}

#[test]
fn unsafe_function_pointer_preserves_call_contract() {
    let result = check(
        r#"
        unsafe fun dangerous(value: i32) -> i32 { value }
        fun main() -> i32 {
            let callback: unsafe fun(i32) -> i32 = dangerous;
            callback(1)
        }
        "#,
    );
    assert!(result.diagnostics.iter().any(|diag| diag.code == "E0046"));
}

#[test]
fn function_pointer_safety_is_one_way() {
    let accepted = check(
        r#"
        fun normal(value: i32) -> i32 { value }
        fun main() -> i32 {
            let callback: unsafe fun(i32) -> i32 = normal;
            unsafe { callback(1) }
        }
        "#,
    );
    assert_eq!(accepted.diagnostics, vec![]);

    let rejected = check(
        r#"
        unsafe fun dangerous(value: i32) -> i32 { value }
        fun main() {
            let callback: fun(i32) -> i32 = dangerous;
        }
        "#,
    );
    assert!(rejected.diagnostics.iter().any(|diag| diag.code == "E0001"));
}

#[test]
fn unsafe_method_call_requires_unsafe_block() {
    let result = check(
        r#"
        struct Reader {}
        impl Reader {
            unsafe fun read(&self) -> i32 { 1 }
        }
        fun main() -> i32 { Reader {}.read() }
        "#,
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
fn legacy_single_extern_import_is_unsafe() {
    let result = check(
        r#"
        extern "C" fun external();
        fun main() { external(); }
        "#,
    );
    assert!(result.diagnostics.iter().any(|diag| diag.code == "E0046"));
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
        r#"
        trait RawRead {
            unsafe fun read(&self) -> i32;
        }
        struct Reader {}
        impl RawRead for Reader {
            fun read(&self) -> i32 { 1 }
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diag| { diag.code == "E0028" && diag.message.contains("safety mismatch") })
    );
}
