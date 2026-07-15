use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process;

use riddlec::{diagnostics, pipeline};

const USAGE: &str =
    "usage: riddlec [--verbose] [--no-std] [--backend c] [--output <file>] <file>...";

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

        let result = pipeline::compile_with_options(
            &source,
            pipeline::CompileOptions {
                use_std: opts.use_std,
            },
        );

        if opts.verbose {
            if opts.files.len() > 1 {
                println!("== {file} ==");
            }
            diagnostics::report_verbose(&result, Some(&source), file);
            println!();
        }

        total_errors += diagnostics::report(&result, Some(&source), file);

        if result.success()
            && let Some(ref module) = result.mir_module
            && let Some(ref backend) = opts.backend
        {
            match generate(module, backend) {
                Ok(code) => generated_code.push_str(&code),
                Err(e) => {
                    eprintln!("riddlec: code generation error: {:?}", e);
                    total_errors += 1;
                }
            }
        }
    }

    if !generated_code.is_empty() {
        if matches!(opts.backend, Some(BackendKind::C)) {
            total_errors += write_c(&generated_code, opts.output.as_deref(), &opts.files);
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
    use_std: bool,
    backend: Option<BackendKind>,
    output: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Opts, &'static str> {
    let mut files = Vec::new();
    let mut verbose = false;
    let mut use_std = true;
    let mut backend = None;
    let mut output = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--verbose" | "-v" => verbose = true,
            "--no-std" => use_std = false,
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
                println!(
                    "riddlec {} ({})",
                    env!("CARGO_PKG_VERSION"),
                    riddlec::GIT_HASH
                );
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
        use_std,
        backend,
        output,
    })
}

/// Write generated C code to a `.c` source file.
fn write_c(c_code: &str, output: Option<&str>, input_files: &[String]) -> usize {
    let c_path = match output {
        Some(path) if path.ends_with(".c") => path.to_owned(),
        Some(path) => format!("{path}.c"),
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
        1
    } else {
        0
    }
}

fn print_help() {
    println!("riddlec — the Riddle compiler (frontend)");
    println!();
    println!("usage: riddlec [flags] <file>...");
    println!();
    println!("flags:");
    println!("  --verbose, -v            print pass status for each file");
    println!("  --no-std                 compile without the bundled standard library");
    println!("  --backend, -b <name>    generate code for target: c");
    println!("  --output, -o <file>     write generated code to file");
    println!("  --version, -V            print version and git commit hash");
    println!("  --help, -h               show this help");
}

#[cfg(test)]
#[path = "../../../tests/riddlec/cli.rs"]
mod tests;
