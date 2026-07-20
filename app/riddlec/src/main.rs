use clap::{ArgAction, Parser};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;

use riddlec::{diagnostics, pipeline};

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum BackendKind {
    C,
}

#[derive(Debug, Parser)]
#[command(
    name = "riddlec",
    about = "The Riddle compiler (frontend)",
    disable_version_flag = true
)]
struct Opts {
    /// Print pass status for each file.
    #[arg(short, long)]
    verbose: bool,

    /// Compile without the bundled standard library.
    #[arg(long = "no-std", action = ArgAction::SetFalse, default_value_t = true)]
    use_std: bool,

    /// Generate code for a target backend.
    #[arg(short, long, value_enum)]
    backend: Option<BackendKind>,

    /// Write generated code to a file.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Print the version and git commit hash.
    #[arg(short = 'V', long)]
    version: bool,

    files: Vec<PathBuf>,
}

fn main() {
    let opts = match parse_args(env::args_os()) {
        Ok(opts) => opts,
        Err(msg) => {
            let exit_code = msg.exit_code();
            let _ = msg.print();
            process::exit(exit_code);
        }
    };

    if opts.version {
        println!(
            "riddlec {} ({})",
            env!("CARGO_PKG_VERSION"),
            riddlec::GIT_HASH
        );
        return;
    }

    if opts.files.is_empty() {
        eprintln!("riddlec: no input files");
        process::exit(1);
    }

    let mut total_errors = 0;
    let mut generated_code = String::new();

    for file in &opts.files {
        let loaded = match pipeline::load_source_file(file) {
            Ok(loaded) => loaded,
            Err(e) => {
                eprintln!("riddlec: cannot read `{}`: {e}", file.display());
                total_errors += 1;
                continue;
            }
        };
        let compile = if opts.backend.is_some() {
            pipeline::compile_with_options
        } else {
            pipeline::check_with_options
        };
        let result = compile(
            &loaded.source,
            pipeline::CompileOptions {
                use_std: opts.use_std,
            },
        );

        if opts.verbose {
            if opts.files.len() > 1 {
                println!("== {} ==", file.display());
            }
            let source_name = file.display().to_string();
            diagnostics::report_verbose(&result, Some(&loaded.source), &source_name);
            println!();
        }

        let source_name = file.display().to_string();
        total_errors += diagnostics::report_mapped(&result, &loaded, &source_name);

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
                        eprintln!("riddlec: cannot write to `{}`: {e}", path.display());
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

fn parse_args<I, T>(args: I) -> Result<Opts, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    Opts::try_parse_from(args)
}

/// Write generated C code to a `.c` source file.
fn write_c(c_code: &str, output: Option<&Path>, input_files: &[PathBuf]) -> usize {
    let c_path = match output {
        Some(path) if path.extension().is_some_and(|ext| ext == "c") => path.to_path_buf(),
        Some(path) => append_c_suffix(path),
        None => {
            let base = input_files
                .first()
                .and_then(|f| f.file_stem())
                .filter(|stem| !stem.is_empty())
                .map(|stem| {
                    let mut output = stem.to_os_string();
                    output.push(".c");
                    PathBuf::from(output)
                })
                .unwrap_or_else(|| PathBuf::from("riddle_out.c"));
            base
        }
    };

    if let Err(e) = fs::write(&c_path, c_code) {
        eprintln!("riddlec: cannot write to `{}`: {e}", c_path.display());
        1
    } else {
        0
    }
}

fn append_c_suffix(path: &Path) -> PathBuf {
    let mut output = path.as_os_str().to_os_string();
    output.push(".c");
    PathBuf::from(output)
}
