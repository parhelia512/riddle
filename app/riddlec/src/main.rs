use clap::{ArgAction, Parser};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;

use riddlec::{diagnostics, pipeline, target::TargetTriple};

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

    /// Select the target platform triple.
    #[arg(long, value_name = "TRIPLE")]
    target: Option<TargetTriple>,

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
    let target = match selected_target(opts.target) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("riddlec: {error}");
            process::exit(1);
        }
    };
    if matches!(opts.backend, Some(BackendKind::C)) && opts.files.len() != 1 {
        eprintln!("riddlec: C backend accepts exactly one input file");
        process::exit(1);
    }

    let mut total_errors = 0;
    let mut generated_code = String::new();

    for file in &opts.files {
        let (errors, code) = compile_file(file, &opts, target);
        total_errors += errors;
        generated_code.push_str(&code);
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

fn compile_file(file: &Path, opts: &Opts, target: TargetTriple) -> (usize, String) {
    let mut loaded = match pipeline::load_source_file(file) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("riddlec: cannot read `{}`: {error}", file.display());
            return (1, String::new());
        }
    };
    let expansion = riddlec::proc_macro::expand_standard_macros(&loaded.source);
    loaded.apply_expansion(expansion.source, &expansion.mappings);
    let options = pipeline::CompileOptions {
        use_std: opts.use_std,
    };
    let package_range = 0..loaded.source.len();
    let package_ranges = std::slice::from_ref(&package_range);
    let mut result = if let Some(parse) = expansion.parse.as_ref() {
        if opts.backend.is_some() {
            pipeline::compile_parsed_package_with_options(
                &loaded.source,
                parse,
                package_ranges,
                options,
            )
        } else {
            pipeline::check_parsed_package_with_options(
                &loaded.source,
                parse,
                package_ranges,
                options,
            )
        }
    } else if opts.backend.is_some() {
        pipeline::compile_package_with_options(&loaded.source, package_ranges, options)
    } else {
        pipeline::check_package_with_options(&loaded.source, package_ranges, options)
    };
    result.macro_diagnostics = expansion.diagnostics;

    if opts.verbose {
        if opts.files.len() > 1 {
            println!("== {} ==", file.display());
        }
        println!("target: {target}");
        let source_name = file.display().to_string();
        diagnostics::report_verbose(&result, Some(&loaded.source), &source_name);
        println!();
    }

    let source_name = file.display().to_string();
    let mut errors = diagnostics::report_mapped(&result, &loaded, &source_name);
    let mut generated = String::new();
    if result.success()
        && let Some(ref module) = result.mir_module
        && let Some(backend) = opts.backend
    {
        match generate(module, backend, &source_name) {
            Ok(code) => generated = code,
            Err(error) => {
                eprintln!("riddlec: code generation error: {error:?}");
                errors += 1;
            }
        }
    }
    (errors, generated)
}

fn generate(
    module: &mir::Module,
    backend: BackendKind,
    source_name: &str,
) -> Result<String, Box<dyn std::fmt::Debug>> {
    match backend {
        BackendKind::C => pipeline::generate_c_with_gc_and_source(module, true, source_name)
            .map_err(|e| Box::new(e) as Box<dyn std::fmt::Debug>),
    }
}

fn parse_args<I, T>(args: I) -> Result<Opts, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    Opts::try_parse_from(args)
}

fn selected_target(explicit: Option<TargetTriple>) -> Result<TargetTriple, String> {
    if let Some(target) = explicit {
        return Ok(target);
    }
    if let Some(target) = env::var_os("RIDDLE_TARGET") {
        return target
            .to_string_lossy()
            .parse()
            .map_err(|error| format!("invalid RIDDLE_TARGET: {error}"));
    }
    TargetTriple::host().map_err(|error| error.to_string())
}

/// Write generated C code to a `.c` source file.
fn write_c(c_code: &str, output: Option<&Path>, input_files: &[PathBuf]) -> usize {
    let c_path = match output {
        Some(path) if path.extension().is_some_and(|ext| ext == "c") => path.to_path_buf(),
        Some(path) => append_c_suffix(path),
        None => input_files
            .first()
            .and_then(|f| f.file_stem())
            .filter(|stem| !stem.is_empty())
            .map_or_else(
                || PathBuf::from("riddle_out.c"),
                |stem| {
                    let mut output = stem.to_os_string();
                    output.push(".c");
                    PathBuf::from(output)
                },
            ),
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
