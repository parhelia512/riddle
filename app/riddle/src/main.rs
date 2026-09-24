use clap::{Args, Parser, ValueEnum};
use riddle::repl::{self, Session};
use riddlec::fmt::{self, FormatOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "riddle",
    version = format!("{} ({})", env!("CARGO_PKG_VERSION"), riddlec::GIT_HASH),
    about = "Riddle language tools"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Format Riddle source files.
    Fmt(FmtArgs),
    /// Compile a file and interpret it (no C toolchain needed).
    Run(RunArgs),
    /// Start an interactive session backed by the MIR interpreter.
    Repl(ReplArgs),
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Source file to run.
    file: PathBuf,

    /// Program arguments passed to `std::env::args`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    program_args: Vec<String>,

    /// Seed for `std::random` (0 = seed from the clock).
    #[arg(long, default_value_t = 0)]
    seed: u64,
}

#[derive(Debug, Args)]
struct ReplArgs {}

#[derive(Debug, Args)]
struct FmtArgs {
    /// Select the output mode.
    #[arg(long, value_enum, default_value_t = Emit::Files)]
    emit: Emit,

    /// Check formatting without changing files.
    #[arg(long, conflicts_with = "emit")]
    check: bool,

    /// Number of spaces used for one indentation level.
    #[arg(long, default_value_t = 4)]
    tab_size: u32,

    /// Use tabs instead of spaces for indentation.
    #[arg(long)]
    hard_tabs: bool,

    /// Files to format. With no files, source is read from stdin.
    files: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Emit {
    Files,
    Stdout,
    Check,
}

fn main() -> ExitCode {
    // Formatting walks deeply nested syntax trees recursively; run on a
    // large stack so pathological inputs hit the parser's nesting
    // diagnostic instead of a stack overflow.
    let worker = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(run)
        .expect("spawn formatter worker thread");
    match worker.join() {
        Ok(code) => code,
        Err(_) => ExitCode::from(1),
    }
}

fn run() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Fmt(args) => run_fmt(args),
        Command::Run(args) => run_file(args),
        Command::Repl(args) => run_repl(args),
    }
}

fn run_file(args: RunArgs) -> ExitCode {
    let source = match std::fs::read_to_string(&args.file) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("riddle run: cannot read `{}`: {error}", args.file.display());
            return ExitCode::from(2);
        }
    };
    let result =
        riddlec::pipeline::compile_for_interpretation(&source, &args.file.display().to_string());
    if !result.success() {
        let errors =
            riddlec::diagnostics::report(&result, Some(&source), &args.file.display().to_string());
        if errors == 0 {
            eprintln!("riddle run: compilation failed");
        }
        return ExitCode::from(1);
    }
    let module = result.mir_module.expect("successful compile produced MIR");
    // A program without `main` would otherwise die inside the interpreter
    // with a bare internal error; report it like the C backend does.
    if !module
        .functions
        .values()
        .any(|function| function.name == "main")
    {
        eprintln!(
            "error[E0401]: no `main` function found in the entry package
  = help: define `fun main() -> i32 {{ ... }}` as the program entry"
        );
        return ExitCode::from(1);
    }
    // Panic sites map back to the source file (through standard-macro
    // expansion) and into the bundled std region.
    let source_files = result.source_files;
    let config = interpreter::Config {
        args: {
            let mut all = vec![args.file.display().to_string()];
            all.extend(args.program_args);
            all
        },
        rng_seed: args.seed,
        ..interpreter::Config::default()
    };
    let outcome = interpreter::run_with(&module, source_files, config);
    let mut stdout = io::stdout();
    let _ = stdout.write_all(&outcome.stdout);
    let _ = stdout.flush();
    let mut stderr = io::stderr();
    let _ = stderr.write_all(&outcome.stderr);
    match &outcome.result {
        Ok(code) => ExitCode::from((*code).max(0) as u8),
        Err(trap) => {
            let mut rendered = Vec::new();
            trap.render(&mut rendered);
            let _ = stderr.write_all(&rendered);
            let _ = stderr.flush();
            ExitCode::from(repl::trap_exit_code(trap))
        }
    }
}

fn run_repl(_args: ReplArgs) -> ExitCode {
    let mut editor = match rustyline::DefaultEditor::new() {
        Ok(editor) => editor,
        Err(error) => {
            eprintln!("riddle repl: cannot initialize line editor: {error}");
            return ExitCode::from(1);
        }
    };
    let mut session = Session::new();
    println!("Riddle REPL — type :help for commands, :quit to exit");
    match read_and_eval(&mut editor, &mut session) {
        Flow::Quit | Flow::Eof => ExitCode::SUCCESS,
    }
}

enum Flow {
    Quit,
    Eof,
}

/// Reads one logical input (following `|` continuation lines) and evaluates
/// it.
fn read_and_eval(editor: &mut rustyline::DefaultEditor, session: &mut Session) -> Flow {
    let mut pending: Option<String> = None;
    loop {
        let prompt = if pending.is_some() { "  | " } else { ">>> " };
        match editor.readline(prompt) {
            Ok(line) => {
                let _ = editor.add_history_entry(line.trim_end());
                let input = match pending.take() {
                    Some(text) => Session::combine(&text, &line),
                    None => line,
                };
                if Session::needs_continuation(&input) {
                    pending = Some(input);
                    continue;
                }
                if input.trim().is_empty() {
                    continue;
                }
                match session.eval(&input) {
                    Ok(repl::Outcome::Quit) => return Flow::Quit,
                    Ok(repl::Outcome::Notice(notice)) if !notice.is_empty() => {
                        println!("{notice}");
                    }
                    Ok(repl::Outcome::Evaluated(value)) if !value.is_empty() => {
                        if value.ends_with('\n') {
                            print!("{value}");
                        } else {
                            println!("{value}");
                        }
                    }
                    Ok(_) => {}
                    Err(reject) => {
                        let text = match reject {
                            repl::Reject::Diagnostics(text)
                            | repl::Reject::Runtime(text)
                            | repl::Reject::Internal(text) => text,
                        };
                        print!("{text}");
                        let _ = io::stdout().flush();
                    }
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("(use :quit to exit)");
                pending = None;
            }
            Err(_) => return Flow::Eof,
        }
    }
}

fn run_fmt(args: FmtArgs) -> ExitCode {
    let mode = if args.check { Emit::Check } else { args.emit };
    let options = FormatOptions {
        tab_size: args.tab_size,
        insert_spaces: !args.hard_tabs,
    };
    if args.files.is_empty() || args.files.iter().any(|file| file.as_os_str() == "-") {
        if args.files.len() > 1
            || args
                .files
                .first()
                .is_some_and(|file| file.as_os_str() != "-")
        {
            eprintln!("riddle fmt: stdin mode accepts only `-` or no input files");
            return ExitCode::from(2);
        }
        return format_stdin(mode, options);
    }

    let mut changed = false;
    let mut failed = false;
    for file in args.files {
        let source = match std::fs::read_to_string(&file) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("riddle fmt: cannot read `{}`: {error}", file.display());
                failed = true;
                continue;
            }
        };
        if report_parse_errors(&file.display().to_string(), &source) {
            failed = true;
            continue;
        }
        let formatted = fmt::format_source(&source, options);
        match mode {
            Emit::Files => {
                if formatted != source {
                    changed = true;
                    if let Err(error) = std::fs::write(&file, formatted) {
                        eprintln!("riddle fmt: cannot write `{}`: {error}", file.display());
                        failed = true;
                    }
                }
            }
            Emit::Stdout => {
                if let Err(error) = io::stdout().write_all(formatted.as_bytes()) {
                    eprintln!("riddle fmt: cannot write stdout: {error}");
                    return ExitCode::from(1);
                }
            }
            Emit::Check => {
                if formatted != source {
                    changed = true;
                    println!("would reformat {}", file.display());
                }
            }
        }
    }
    if failed || (matches!(mode, Emit::Check) && changed) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn format_stdin(mode: Emit, options: FormatOptions) -> ExitCode {
    let mut source = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut source) {
        eprintln!("riddle fmt: cannot read stdin: {error}");
        return ExitCode::from(1);
    }
    if report_parse_errors("<stdin>", &source) {
        return ExitCode::from(1);
    }
    let formatted = fmt::format_source(&source, options);
    match mode {
        Emit::Check => {
            if formatted == source {
                ExitCode::SUCCESS
            } else {
                eprintln!("riddle fmt: stdin is not formatted");
                ExitCode::from(1)
            }
        }
        Emit::Files | Emit::Stdout => {
            if let Err(error) = io::stdout().write_all(formatted.as_bytes()) {
                eprintln!("riddle fmt: cannot write stdout: {error}");
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}

fn report_parse_errors(source_name: &str, source: &str) -> bool {
    let errors = fmt::parse_errors(source);
    for error in &errors {
        let offset = usize::from(error.span.start()).min(source.len());
        let prefix = &source[..offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        let column = source[line_start..offset].chars().count() + 1;
        eprintln!(
            "riddle fmt: {source_name}:{line}:{column}: {}",
            error.message
        );
    }
    !errors.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fmt_modes() {
        let cli = Cli::try_parse_from(["riddle", "fmt", "--emit", "stdout", "main.rid"])
            .expect("fmt arguments should parse");
        let Command::Fmt(args) = cli.command else {
            panic!("expected the fmt subcommand");
        };
        assert!(matches!(args.emit, Emit::Stdout));
        assert_eq!(args.files, [PathBuf::from("main.rid")]);
    }

    #[test]
    fn parses_run_and_repl() {
        let cli = Cli::try_parse_from(["riddle", "run", "--seed", "7", "app.rid", "--flag"])
            .expect("run arguments should parse");
        let Command::Run(args) = cli.command else {
            panic!("expected the run subcommand");
        };
        assert_eq!(args.file, PathBuf::from("app.rid"));
        assert_eq!(args.seed, 7);
        assert_eq!(args.program_args, ["--flag"]);

        let cli = Cli::try_parse_from(["riddle", "repl"]).expect("repl parses");
        assert!(matches!(cli.command, Command::Repl(_)));
    }
}
