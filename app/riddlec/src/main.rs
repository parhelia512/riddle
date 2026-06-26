mod diagnostics;
mod pipeline;

use std::env;
use std::fs;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();

    let (files, verbose) = match parse_args(&args) {
        Ok(opts) => opts,
        Err(msg) => {
            eprintln!("riddlec: {msg}");
            eprintln!("usage: riddlec [--verbose] <file>...");
            process::exit(1);
        }
    };

    if files.is_empty() {
        eprintln!("riddlec: no input files");
        eprintln!("usage: riddlec [--verbose] <file>...");
        process::exit(1);
    }

    let mut total_errors = 0;

    for file in &files {
        let source = match fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("riddlec: cannot read `{file}`: {e}");
                total_errors += 1;
                continue;
            }
        };

        let result = pipeline::compile(&source);

        if verbose {
            if files.len() > 1 {
                println!("== {file} ==");
            }
            diagnostics::report_verbose(&result);
            println!();
        }

        total_errors += diagnostics::report(&result, file);
    }

    if total_errors > 0 {
        process::exit(1);
    }
}

fn parse_args(args: &[String]) -> Result<(Vec<String>, bool), &'static str> {
    let mut files = Vec::new();
    let mut verbose = false;

    // Skip argv[0] (the binary name).
    for arg in &args[1..] {
        match arg.as_str() {
            "--verbose" | "-v" => verbose = true,
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
    }

    Ok((files, verbose))
}

fn print_help() {
    println!("riddlec — the Riddle compiler (frontend)");
    println!();
    println!("usage: riddlec [flags] <file>...");
    println!();
    println!("flags:");
    println!("  --verbose, -v    print pass status for each file");
    println!("  --version, -V    print version (git commit hash)");
    println!("  --help, -h       show this help");
}
