use clap::{ArgAction, Parser};
use std::env;
use std::ffi::OsString;
use std::fs;
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

    if opts.backend.is_some() {
        // The C backend compiles every input as one program: files are
        // concatenated into a single package so they can reference each
        // other's items, with source maps preserved for panic locations.
        let errors = compile_program(&opts.files, &opts, target);
        if errors > 0 {
            process::exit(1);
        }
        return;
    }

    let mut total_errors = 0;
    for file in &opts.files {
        total_errors += compile_file(file, &opts, target);
    }

    if total_errors > 0 {
        process::exit(1);
    }
}

/// Loads and macro-expands every input file, merging them into one package.
///
/// Returns the combined source plus per-file source maps so diagnostics and
/// generated panic locations keep pointing at the original files.
fn load_program_sources(files: &[PathBuf]) -> Result<pipeline::LoadedSource, PathBuf> {
    let mut combined = String::new();
    let mut files_loaded = Vec::new();
    let mut source_map = pipeline::SourceMap::default();
    for file in files {
        let mut loaded = pipeline::load_source_file(file).map_err(|error| {
            eprintln!("riddlec: cannot read `{}`: {error}", file.display());
            file.clone()
        })?;
        let expansion = riddlec::proc_macro::expand_standard_macros(&loaded.source);
        loaded.apply_expansion(expansion.source, &expansion.mappings);
        if !combined.is_empty() {
            combined.push('\n');
        }
        let offset = combined.len();
        combined.push_str(&loaded.source);
        source_map.extend(loaded.source_map, offset);
        files_loaded.extend(loaded.files);
    }
    Ok(pipeline::LoadedSource {
        source: combined,
        files: files_loaded,
        source_map,
    })
}

fn compile_program(files: &[PathBuf], opts: &Opts, target: TargetTriple) -> usize {
    let loaded = match load_program_sources(files) {
        Ok(loaded) => loaded,
        Err(_) => return 1,
    };
    let options = pipeline::CompileOptions {
        use_std: opts.use_std,
    };
    let package_range = 0..loaded.source.len();
    let package_ranges = std::slice::from_ref(&package_range);
    let result = pipeline::compile_package_with_options(&loaded.source, package_ranges, options);

    let entry_name = files.first().map_or_else(
        || "<unknown>".to_string(),
        |file| file.display().to_string(),
    );
    if opts.verbose {
        println!("target: {target}");
        diagnostics::report_verbose(&result, Some(&loaded.source), &entry_name);
        println!();
    }

    let mut errors = diagnostics::report_mapped(&result, &loaded, &entry_name);
    if result.success()
        && let Some(ref module) = result.mir_module
        && opts.backend.is_some()
    {
        match pipeline::generate_c_for_package_with_source_map(
            module,
            0,
            true,
            &loaded.source_map,
            &entry_name,
        ) {
            Ok(code) => errors += write_c(&code, opts.output.as_deref(), files),
            Err(error) => {
                eprintln!("riddlec: code generation error: {error:?}");
                errors += 1;
            }
        }
    }
    errors
}

fn compile_file(file: &Path, opts: &Opts, target: TargetTriple) -> usize {
    let mut loaded = match pipeline::load_source_file(file) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("riddlec: cannot read `{}`: {error}", file.display());
            return 1;
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
        pipeline::check_parsed_package_with_options(&loaded.source, parse, package_ranges, options)
    } else {
        pipeline::check_package_with_options(&loaded.source, package_ranges, options)
    };
    result.macro_diagnostics = expansion.diagnostics;

    let source_name = file.display().to_string();
    if opts.verbose {
        if opts.files.len() > 1 {
            println!("== {} ==", file.display());
        }
        println!("target: {target}");
        diagnostics::report_verbose(&result, Some(&loaded.source), &source_name);
        println!();
    }

    diagnostics::report_mapped(&result, &loaded, &source_name)
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
