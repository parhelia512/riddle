use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process;

use riddlec::{c_compiler, diagnostics, pipeline};

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
        let loaded = match pipeline::load_source_file(file) {
            Ok(loaded) => loaded,
            Err(e) => {
                eprintln!("riddlec: cannot read `{file}`: {e}");
                total_errors += 1;
                continue;
            }
        };
        let source = loaded.source;

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
            pipeline::generate_c(module).map_err(|e| Box::new(e) as Box<dyn std::fmt::Debug>)
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
                println!("riddlec {}", riddlec::GIT_HASH);
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

/// Compile generated C code to a native executable.
/// Returns the number of errors (0 on success).
fn compile_c(c_code: &str, output: Option<&str>, input_files: &[String]) -> usize {
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
                        .map(|f| {
                            Path::new(f)
                                .file_stem()
                                .unwrap_or_default()
                                .to_string_lossy()
                        })
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
    let exe_path = match output {
        Some(path) if !path.ends_with(".c") => Path::new(path).to_path_buf(),
        _ => c_compiler::executable_path(Path::new(&c_path)),
    };

    // Compile
    match c_compiler::compile_file(Path::new(&c_path), &exe_path) {
        Ok(cc) => {
            eprintln!("riddlec: compiled {} with `{cc}`", exe_path.display());
            0
        }
        Err(e) => {
            eprintln!("riddlec: {e}");
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
