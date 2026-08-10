use clap::{Args, Parser, Subcommand};
use clue::{
    ProjectKind, TargetTriple, build_for_target_with_selection, check_for_target_with_selection,
    init, init_workspace, new, new_workspace, run_for_target_with_selection,
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
        #[arg(short = 'p', long)]
        package: Option<String>,
        #[arg(long, conflicts_with = "package")]
        workspace: bool,
        #[arg(long, value_name = "TRIPLE")]
        target: Option<TargetTriple>,
    },
    Build {
        path: Option<PathBuf>,
        #[arg(short = 'p', long)]
        package: Option<String>,
        #[arg(long, conflicts_with = "package")]
        workspace: bool,
        #[arg(long, value_name = "TRIPLE")]
        target: Option<TargetTriple>,
    },
    Run {
        path: Option<PathBuf>,
        #[arg(short = 'p', long)]
        package: Option<String>,
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

    #[arg(long, conflicts_with_all = ["lib", "bin"])]
    workspace: bool,
}

impl ProjectArgs {
    const fn kind(&self) -> ProjectKind {
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
            if args.workspace {
                init_workspace(&args.path)?;
            } else {
                init(&args.path, args.kind())?;
            }
            println!("clue: initialized {}", args.path.display());
        }
        Commands::New(args) => {
            if args.workspace {
                new_workspace(&args.path)?;
            } else {
                new(&args.path, args.kind())?;
            }
            println!("clue: created {}", args.path.display());
        }
        Commands::Check {
            path,
            package,
            workspace,
            target,
        } => {
            check_for_target_with_selection(
                path.as_deref().unwrap_or_else(|| std::path::Path::new(".")),
                target,
                package.as_deref(),
                workspace,
            )?;
        }
        Commands::Build {
            path,
            package,
            workspace,
            target,
        } => {
            build_for_target_with_selection(
                path.as_deref().unwrap_or_else(|| std::path::Path::new(".")),
                target,
                package.as_deref(),
                workspace,
            )?;
        }
        Commands::Run {
            path,
            package,
            target,
            args,
        } => {
            let status = run_for_target_with_selection(
                path.as_deref().unwrap_or_else(|| std::path::Path::new(".")),
                &args,
                target,
                package.as_deref(),
                false,
            )?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }

    Ok(())
}
