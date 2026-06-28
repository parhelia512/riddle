mod diagnostics;
mod pipeline;

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{self, Command};

use mir::backend::{Backend, c::CBackend};

const USAGE: &str = "usage: riddlec [--verbose] [--backend c] [--output <file>] <file>...";

enum BackendKind {
    C,
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let opts = match parse_args(&args) {
        Ok(opts) => opts,
        Err(msg) => {
            eprintln!("riddlec: {msg}");
            eprintln!("{}", USAGE);
            process::exit(1);
        }
    };

    if opts.files.is_empty() {
        eprintln!("riddlec: no input files");
        eprintln!("{}", USAGE);
        process::exit(1);
    }

    let mut total_errors = 0;
    let mut generated_code = String::new();

    for file in &opts.files {
        let source = match fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("riddlec: cannot read `{file}`: {e}");
                total_errors += 1;
                continue;
            }
        };

        let result = pipeline::compile(&source);

        if opts.verbose {
            if opts.files.len() > 1 {
                println!("== {file} ==");
            }
            diagnostics::report_verbose(&result, Some(&source), file);
            println!();
        }

        total_errors += diagnostics::report(&result, Some(&source), file);

        if result.success() {
            if let Some(ref module) = result.mir_module {
                if let Some(ref backend) = opts.backend {
                    match generate(module, backend) {
                        Ok(code) => generated_code.push_str(&code),
                        Err(e) => {
                            eprintln!("riddlec: code generation error: {:?}", e);
                            total_errors += 1;
                        }
                    }
                }
            }
        }
    }

    if !generated_code.is_empty() {
        if matches!(opts.backend, Some(BackendKind::C)) {
            total_errors += compile_c(&generated_code, opts.output.as_deref(), &opts.files);
        } else {
            match opts.output {
                Some(ref path) => {
                    if let Err(e) = fs::write(path, &generated_code) {
                        eprintln!("riddlec: cannot write to `{path}`: {e}");
                        total_errors += 1;
                    }
                }
                None => {
                    let _ = io::stdout().write_all(generated_code.as_bytes());
                }
            }
        }
    }

    if total_errors > 0 {
        process::exit(1);
    }
}

fn generate(
    module: &mir::Module,
    backend: &BackendKind,
) -> Result<String, Box<dyn std::fmt::Debug>> {
    match backend {
        BackendKind::C => {
            let mut b = CBackend::new();
            b.compile(module)
                .map_err(|e| Box::new(e) as Box<dyn std::fmt::Debug>)
        }
    }
}

struct Opts {
    files: Vec<String>,
    verbose: bool,
    backend: Option<BackendKind>,
    output: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Opts, &'static str> {
    let mut files = Vec::new();
    let mut verbose = false;
    let mut backend = None;
    let mut output = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--verbose" | "-v" => verbose = true,
            "--backend" | "-b" => {
                i += 1;
                if i >= args.len() {
                    return Err("--backend requires a value: c");
                }
                backend = Some(match args[i].as_str() {
                    "c" => BackendKind::C,
                    other => {
                        eprintln!("riddlec: unknown backend '{other}'");
                        eprintln!("{USAGE}");
                        process::exit(1);
                    }
                });
            }
            "--output" | "-o" => {
                i += 1;
                if i >= args.len() {
                    return Err("--output requires a file path");
                }
                output = Some(args[i].clone());
            }
            "--help" | "-h" => {
                print_help();
                process::exit(0);
            }
            "--version" | "-V" => {
                println!("riddlec {}", env!("GIT_HASH"));
                process::exit(0);
            }
            other if other.starts_with('-') => {
                return Err("unknown flag");
            }
            other => files.push(other.to_string()),
        }
        i += 1;
    }

    Ok(Opts {
        files,
        verbose,
        backend,
        output,
    })
}

/// Search for an available C compiler: cc → gcc → clang.
fn find_cc() -> Option<String> {
    for cc in &["cc", "gcc", "clang"] {
        if Command::new(cc).arg("--version").output().is_ok() {
            return Some(cc.to_string());
        }
    }
    None
}

/// Compile generated C code to a native executable.
/// Returns the number of errors (0 on success).
fn compile_c(c_code: &str, output: Option<&str>, input_files: &[String]) -> usize {
    let cc = match find_cc() {
        Some(c) => c,
        None => {
            eprintln!("riddlec: no C compiler found (tried cc, gcc, clang)");
            return 1;
        }
    };

    // Write C code to a temp file
    let c_path = match output {
        Some(path) if path.ends_with(".c") || path.ends_with(".h") => {
            // User wants only the C source, no compilation
            if let Err(e) = fs::write(path, c_code) {
                eprintln!("riddlec: cannot write to `{path}`: {e}");
                return 1;
            }
            return 0;
        }
        _ => {
            let c_path = match output {
                Some(path) => format!("{}.c", path),
                None => {
                    let base = input_files
                        .first()
                        .map(|f| Path::new(f).file_stem().unwrap_or_default().to_string_lossy())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "riddle_out".into());
                    format!("{base}.c")
                }
            };
            if let Err(e) = fs::write(&c_path, c_code) {
                eprintln!("riddlec: cannot write to `{c_path}`: {e}");
                return 1;
            }
            c_path
        }
    };

    // Derive binary name
    let exe = match output {
        Some(path) if !path.ends_with(".c") => path.to_string(),
        _ => {
            let base = Path::new(&c_path).file_stem().unwrap().to_string_lossy();
            if cfg!(windows) {
                format!("{}.exe", base)
            } else {
                base.to_string()
            }
        }
    };

    // Compile
    eprintln!("riddlec: compiling C with `{cc}` → {exe}");
    let status = Command::new(&cc)
        .arg("-o")
        .arg(&exe)
        .arg(&c_path)
        .arg("-lgc")
        .status();

    match status {
        Ok(s) if s.success() => 0,
        Ok(s) => {
            eprintln!("riddlec: C compiler exited with {s}");
            1
        }
        Err(e) => {
            eprintln!("riddlec: failed to run `{cc}`: {e}");
            1
        }
    }
}

fn print_help() {
    println!("riddlec — the Riddle compiler (frontend)");
    println!();
    println!("usage: riddlec [flags] <file>...");
    println!();
    println!("flags:");
    println!("  --verbose, -v            print pass status for each file");
    println!("  --backend, -b <name>    generate code for target: c");
    println!("  --output, -o <file>     write generated code to file");
    println!("  --version, -V            print version (git commit hash)");
    println!("  --help, -h               show this help");
}
