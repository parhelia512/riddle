use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "riddlec-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn run(args: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riddlec"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn accepts_no_std() {
    let root = temp_root("no-std");
    fs::create_dir_all(&root).unwrap();
    let input = root.join("main.rid");
    fs::write(&input, "fun main() {}\n").unwrap();

    let output = run(&[Path::new("--no-std"), &input]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn expands_standard_print_macros() {
    let root = temp_root("print-macros");
    fs::create_dir_all(&root).unwrap();
    let input = root.join("main.rid");
    fs::write(
        &input,
        "fun main() -> i32 { print!(\"value={}\", 7); println!(); 0 }\n",
    )
    .unwrap();

    let output = run(&[&input]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn accepts_supported_target_and_rejects_unknown_target() {
    let root = temp_root("target");
    fs::create_dir_all(&root).unwrap();
    let input = root.join("main.rid");
    fs::write(&input, "fun main() {}\n").unwrap();

    let accepted = run(&[
        Path::new("--target"),
        Path::new("aarch64-unknown-linux-gnu"),
        &input,
    ]);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    let rejected = run(&[
        Path::new("--target"),
        Path::new("x86_64-unknown-linux-musl"),
        &input,
    ]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("unsupported target"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn c_backend_rejects_multiple_inputs() {
    let root = temp_root("multiple-inputs");
    fs::create_dir_all(&root).unwrap();
    let first = root.join("first.rid");
    let second = root.join("second.rid");
    let generated = root.join("combined.c");
    fs::write(&first, "fun first() {}\n").unwrap();
    fs::write(&second, "fun second() {}\n").unwrap();

    let output = run(&[
        Path::new("--backend"),
        Path::new("c"),
        Path::new("--output"),
        &generated,
        &first,
        &second,
    ]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("C backend accepts exactly one input file"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!generated.exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn diagnostics_keep_rust_style_hierarchy() {
    let root = temp_root("diagnostics");
    fs::create_dir_all(&root).unwrap();
    let input = root.join("main.rid");
    fs::write(
        &input,
        "struct Foo {}\nfun main() {\n    let a = Foo {};\n    let b = a;\n    let c = a;\n}\n",
    )
    .unwrap();

    let output = run(&[Path::new("--no-std"), &input]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        stderr.starts_with("error[E0100]: use of moved value: `a`\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains(" 4 |     let b = a;\n   |             - value moved here\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains(" 5 |     let c = a;\n   |             ^\n"),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("error: aborting due to 1 previous error\n"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn debug_format_bound_diagnostic_points_to_user_source() {
    let root = temp_root("debug-format-diagnostic");
    fs::create_dir_all(&root).unwrap();
    let input = root.join("main.rid");
    fs::write(
        &input,
        "enum Foo { A() }\nfun main() {\n    let value = Foo::A();\n    println!(\"{:?}\", value);\n}\n",
    )
    .unwrap();

    let output = run(&[&input]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let open = '{';
    let close = '}';
    let debug_placeholder = format!("{open}:?{close}");

    assert!(!output.status.success());
    assert!(
        stderr.starts_with("error[E0035]: `Foo` doesn't implement `Debug`\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("println!(\"{debug_placeholder}\", value);")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "`Foo` cannot be formatted using `{debug_placeholder}` because it doesn't implement `Debug`"
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains("required by this formatting parameter"),
        "{stderr}"
    );
    assert!(
        stderr.contains("consider annotating `Foo` with `#[derive(Debug)]`"),
        "{stderr}"
    );
    assert!(!stderr.contains("print_debug"), "{stderr}");
    assert!(!stderr.contains(r"\\?\"), "{stderr}");
    let _ = fs::remove_dir_all(root);
}
