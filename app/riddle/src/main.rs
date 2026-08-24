use clap::{Args, Parser, ValueEnum};
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
}

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
    let cli = Cli::parse();
    match cli.command {
        Command::Fmt(args) => run_fmt(args),
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
        let Command::Fmt(args) = cli.command;
        assert!(matches!(args.emit, Emit::Stdout));
        assert_eq!(args.files, [PathBuf::from("main.rid")]);
    }
}
