use clap::{Args, Parser, Subcommand};
use clue::{ProjectKind, build, check, init, new};
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
    Check { path: Option<PathBuf> },
    Build { path: Option<PathBuf> },
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
        Commands::Check { path } => {
            check(path.as_deref().unwrap_or_else(|| std::path::Path::new(".")))?;
        }
        Commands::Build { path } => {
            build(path.as_deref().unwrap_or_else(|| std::path::Path::new(".")))?;
        }
    }

    Ok(())
}
