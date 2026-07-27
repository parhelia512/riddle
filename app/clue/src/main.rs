use clap::{Args, Parser, Subcommand};
use clue::{
    ProjectKind, TargetTriple, build_for_target, check_for_target, init, new, run_for_target,
};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "clue", version, about = "A project builder for Riddle")]
struct Cli {
    #[command(subcommand)]
    commands: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init(ProjectArgs),
    New(ProjectArgs),
    Check {
        path: Option<PathBuf>,
        #[arg(long, value_name = "TRIPLE")]
        target: Option<TargetTriple>,
    },
    Build {
        path: Option<PathBuf>,
        #[arg(long, value_name = "TRIPLE")]
        target: Option<TargetTriple>,
    },
    Run {
        path: Option<PathBuf>,
        #[arg(long, value_name = "TRIPLE")]
        target: Option<TargetTriple>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
}

#[derive(Args)]
struct ProjectArgs {
    path: PathBuf,

    #[arg(long, conflicts_with = "bin")]
    lib: bool,

    #[arg(long, conflicts_with = "lib")]
    bin: bool,
}

impl ProjectArgs {
    fn kind(&self) -> ProjectKind {
        if self.lib {
            ProjectKind::Library
        } else {
            ProjectKind::Binary
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.commands {
        Commands::Init(args) => {
            init(&args.path, args.kind())?;
            println!("clue: initialized {}", args.path.display());
        }
        Commands::New(args) => {
            new(&args.path, args.kind())?;
            println!("clue: created {}", args.path.display());
        }
        Commands::Check { path, target } => {
            check_for_target(
                path.as_deref().unwrap_or_else(|| std::path::Path::new(".")),
                target,
            )?;
        }
        Commands::Build { path, target } => {
            build_for_target(
                path.as_deref().unwrap_or_else(|| std::path::Path::new(".")),
                target,
            )?;
        }
        Commands::Run { path, target, args } => {
            let status = run_for_target(
                path.as_deref().unwrap_or_else(|| std::path::Path::new(".")),
                &args,
                target,
            )?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }

    Ok(())
}
