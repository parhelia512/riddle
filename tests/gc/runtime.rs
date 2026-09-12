use gc::{ARGS_RUNTIME_C, NO_GC_RUNTIME_C, RUNTIME_C};
use std::{fs, process::Command};

fn run_c(body: &str) -> std::process::Output {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQUENCE: AtomicU32 = AtomicU32::new(0);

    // Unique file names: the tests run in parallel threads and Windows
    // locks an executable while it runs, so a shared test.c/test.exe pair
    // makes one thread's compile race the other's run.
    let dir = std::env::temp_dir().join(format!(
        "riddle-gc-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::create_dir_all(&dir);
    let src = dir.join("test.c");
    let exe = dir.join(if cfg!(windows) { "test.exe" } else { "test" });
    // The scenario runs in a callee of `main` so every live pointer sits in
    // a frame below the stack-bottom anchor: the scan covers
    // [collect frame .. anchor], and same-frame locals allocated above the
    // anchor's slot would otherwise escape the conservative scan.
    let program = format!(
        "#include <stdint.h>\n#include <stdio.h>\nstatic void *g_root;\n{RUNTIME_C}\nstatic int scenario(void){{ {body} }}\nint main(void){{ void *bottom = &bottom; rgc_init(bottom); return scenario(); }}"
    );
    fs::write(&src, program).unwrap();
    let compiler = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let compile = Command::new(&compiler)
        .args(["-std=c11"])
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "C compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let output = Command::new(&exe).output().unwrap();
    let _ = fs::remove_dir_all(&dir);
    output
}

#[test]
fn exports_process_argument_runtime() {
    for symbol in [
        "GetCommandLineW",
        "riddle_parse_windows_args",
        "riddle_args_init",
        "riddle_argc",
        "riddle_argv_at",
        "riddle_argv_len",
    ] {
        assert!(ARGS_RUNTIME_C.contains(symbol), "missing {symbol}");
    }
}

#[test]
fn exports_runtime_api() {
    assert!(RUNTIME_C.contains("void rgc_init(void *stack_bottom)"));
    assert!(RUNTIME_C.contains("void *rgc_alloc(size_t size)"));
    assert!(RUNTIME_C.contains("void *rgc_realloc(void *ptr, size_t size)"));
    assert!(RUNTIME_C.contains("void rgc_free(void *ptr)"));
    assert!(RUNTIME_C.contains("void rgc_collect(void)"));
    assert!(!RUNTIME_C.contains("GC_MALLOC"));
    assert!(!RUNTIME_C.contains("<gc.h>"));
    assert!(!RUNTIME_C.contains("abort()"));
}

#[test]
fn exports_an_allocator_only_runtime() {
    for symbol in ["riddle_alloc", "riddle_realloc", "riddle_free"] {
        assert!(NO_GC_RUNTIME_C.contains(symbol), "missing {symbol}");
    }
    for forbidden in ["rgc_", "RgcHeader", "collect", "stack_bottom"] {
        assert!(
            !NO_GC_RUNTIME_C.contains(forbidden),
            "no-GC runtime contains {forbidden}"
        );
    }
}

#[test]
fn collection_preserves_live_objects_and_interior_roots() {
    // The interior pointer lives in a local that is read after the
    // collection, so it is guaranteed to sit in the scanned root set
    // (stack range or callee-saved register snapshot) at `rgc_collect`
    // time; the collector must resolve it back to the object header and
    // keep the object alive.
    let output = run_c(
        "unsigned char *p = rgc_alloc(32); p[3] = 77; unsigned char *interior = p + 3; for (int i=0;i<2000;i++) (void)rgc_alloc(4096); rgc_collect(); if (*interior != 77) return 1; return 0;",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn realloc_and_exact_free_keep_address_semantics() {
    // Realloc preserves content across collections, and `rgc_free` only
    // accepts the exact allocation address: `q + 1` must be a no-op, after
    // which `q` still survives a collection until the exact free.
    let output = run_c(
        "unsigned char *p = rgc_alloc(8); p[0] = 9; p = rgc_realloc(p, 4096); if (p[0] != 9) return 1; unsigned char *q = rgc_alloc(8); rgc_free(q + 1); q[0] = 5; rgc_collect(); if (q[0] != 5) return 2; rgc_free(q); return 0;",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
