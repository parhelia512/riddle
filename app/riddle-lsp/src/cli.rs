use std::time::Duration;

use clap::Parser;
use riddlec::pipeline::CompileOptions;

pub struct Options {
    pub compile_options: CompileOptions,
    pub completion_delay: Duration,
}

#[derive(Parser)]
#[command(
    name = "riddle-lsp",
    version = format!("{} ({})", env!("CARGO_PKG_VERSION"), riddlec::GIT_HASH)
)]
struct CliArgs {
    #[arg(long = "no-std", help = "Disable standard library loading")]
    no_std: bool,
    #[arg(
        long = "completion-delay-ms",
        value_name = "MS",
        default_value_t = 0,
        help = "Delay completion requests by the given number of milliseconds"
    )]
    completion_delay_ms: u64,
}

pub fn parse_args(args: &[String]) -> Result<Options, clap::Error> {
    let args = CliArgs::try_parse_from(args.iter().map(String::as_str))?;
    Ok(Options {
        compile_options: CompileOptions {
            use_std: !args.no_std,
        },
        completion_delay: Duration::from_millis(args.completion_delay_ms),
    })
}
