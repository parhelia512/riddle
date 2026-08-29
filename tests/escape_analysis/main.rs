//! Escape-analysis behavior tests: values that stay on the stack vs values
//! promoted to the conservative GC heap, and the `E0310` diagnostic that
//! fires when a reference would escape in a no-GC build.

use riddlec::pipeline::{self, CompileOptions};
use type_checker::Diagnostic;

fn escape_diagnostics(source: &str, gc: bool) -> Vec<Diagnostic> {
    let result =
        pipeline::compile_with_options_and_gc(source, CompileOptions { use_std: false }, gc);
    result.analysis_diagnostics
}

#[test]
fn local_scalar_stays_on_the_stack() {
    let diagnostics = escape_diagnostics(
        r"
        fun compute() -> i32 {
            let value = 7i32;
            value * 2i32
        }
        ",
        true,
    );
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:#?}"
    );
}

#[test]
fn reference_returned_from_function_escapes() {
    let diagnostics = escape_diagnostics(
        r"
        struct Data { value: i32 }

        fun escaped() -> &Data {
            let local = Data { value: 1 };
            &local
        }
        ",
        true,
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "E0310"),
        "GC builds must promote the value instead of rejecting: {diagnostics:#?}"
    );
}

#[test]
fn reference_stored_in_struct_field_escapes() {
    let diagnostics = escape_diagnostics(
        r"
        struct Inner { value: i32 }
        struct Holder { reference: &Inner }

        fun make() -> Holder {
            let inner = Inner { value: 2 };
            Holder { reference: &inner }
        }
        ",
        true,
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "E0310"),
        "GC builds must promote through fields: {diagnostics:#?}"
    );
}

#[test]
fn no_gc_rejects_returned_reference_to_local() {
    let diagnostics = escape_diagnostics(
        r"
        struct Data { value: i32 }

        fun escaped() -> &Data {
            let local = Data { value: 1 };
            &local
        }
        ",
        false,
    );
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E0310")
        .expect("no-GC escape should report E0310");
    assert!(diagnostic.message.contains("GC is disabled"));
}

#[test]
fn no_gc_rejects_reference_stored_in_field() {
    let diagnostics = escape_diagnostics(
        r"
        struct Inner { value: i32 }
        struct Holder { reference: &Inner }

        fun make() -> Holder {
            let inner = Inner { value: 2 };
            Holder { reference: &inner }
        }
        ",
        false,
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0310"),
        "field escape should report E0310 under no-GC: {diagnostics:#?}"
    );
}

#[test]
fn borrowing_within_a_frame_does_not_escape() {
    let diagnostics = escape_diagnostics(
        r"
        struct Data { value: i32 }

        fun read(data: &Data) -> i32 {
            data.value
        }

        fun compute() -> i32 {
            let local = Data { value: 3 };
            read(&local)
        }
        ",
        false,
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "E0310"),
        "same-frame borrows must stay on the stack under no-GC: {diagnostics:#?}"
    );
}
