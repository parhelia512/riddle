use clap::{Args, Parser, Subcommand};
use clue::{manifest, package};
use manifest::{CLUE_PROJECT_FILE_NAME, PackageKind};
use riddlec::{c_compiler, pipeline};
use std::collections::hash_map::DefaultHasher;
use std::ffi::OsStr;
use std::fs::{self, create_dir, create_dir_all, write};
use std::hash::{Hash, Hasher};
use std::io;
use std::io::ErrorKind::AlreadyExists;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::process;

const GITIGNORE_FILE_NAME: &str = ".gitignore";

#[derive(Parser, Debug)]
#[command(name = "clue")]
#[command(version, about = "A Riddle builder")]
struct Arg {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug, Clone)]
enum Commands {
    Init(InitArg),
    Build(BuildArg),
}

#[derive(Args, Debug, Clone)]
struct InitArg {
    #[arg(long, conflicts_with = "lib")]
    bin: bool,
    #[arg(long, conflicts_with = "bin")]
    lib: bool,
    path: PathBuf,
}

impl InitArg {
    fn package_kind(&self) -> PackageKind {
        if self.lib {
            PackageKind::Library
        } else {
            PackageKind::Binary
        }
    }
}

#[derive(Args, Debug, Clone)]
struct BuildArg {
    path: Option<PathBuf>,
}

fn try_create_package(path: impl AsRef<Path>, kind: PackageKind) -> Result<(), Error> {
    let path = path.as_ref();
    match create_dir(path) {
        Err(error) => {
            if error.kind() != AlreadyExists {
                return Err(error);
            }
        }
        _ => {}
    }
    let package_name = package_name_from_path(path)?;
    write(
        path.join(CLUE_PROJECT_FILE_NAME),
        manifest::new_manifest(&package_name, kind),
    )?;
    write(path.join(GITIGNORE_FILE_NAME), "/.clue")?;
    Ok(())
}

fn package_name_from_path(path: &Path) -> io::Result<String> {
    let name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "project path must end with a valid UTF-8 directory name",
        )
    })?;

    Ok(name.to_string())
}

fn build(arg: BuildArg) -> io::Result<()> {
    let root = arg.path.unwrap_or_else(|| PathBuf::from("."));
    let package = package::load(&root)?;
    let result = pipeline::compile(&package.source.source);
    let errors = riddlec::diagnostics::report(
        &result,
        Some(&package.source.source),
        &package.entry.display().to_string(),
    );
    if errors > 0 || !result.success() {
        return Err(Error::new(ErrorKind::InvalidData, "build failed"));
    }

    let build_dir = root.join(".clue").join("build");
    create_dir_all(&build_dir)?;
    let c_path = build_dir.join(format!("{}.c", package.name));
    let exe_path = c_compiler::executable_path(&c_path);
    let hash_path = build_dir.join(format!("{}.hash", package.name));
    let hash = fingerprint(&package.manifest_fingerprint, &package.source.source);
    if fs::read_to_string(&hash_path).unwrap_or_default() == hash
        && c_path.is_file()
        && output_fresh(&c_path, &exe_path)
    {
        println!("clue: fresh {}", exe_path.display());
        return Ok(());
    }

    if fs::read_to_string(&hash_path).unwrap_or_default() != hash || !c_path.is_file() {
        let module = result
            .mir_module
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "missing MIR module"))?;
        let c_code = pipeline::generate_c(module).map_err(Error::other)?;
        fs::write(&c_path, c_code)?;
        fs::write(&hash_path, hash)?;
        println!("clue: built {}", c_path.display());
    }

    let compiler = c_compiler::compile_file(&c_path, &exe_path)?;
    println!("clue: compiled {} with `{compiler}`", exe_path.display());
    Ok(())
}

fn fingerprint(manifest: &str, source: &str) -> String {
    let mut hasher = DefaultHasher::new();
    manifest.hash(&mut hasher);
    source.hash(&mut hasher);
    riddlec::GIT_HASH.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn output_fresh(input: &Path, output: &Path) -> bool {
    let Ok(input_modified) = input.metadata().and_then(|metadata| metadata.modified()) else {
        return false;
    };
    let Ok(output_modified) = output.metadata().and_then(|metadata| metadata.modified()) else {
        return false;
    };
    output_modified >= input_modified
}

fn main() {
    let args = Arg::parse();
    let result = match args.command {
        Commands::Init(arg) => {
            let kind = arg.package_kind();
            try_create_package(arg.path, kind)
        }
        Commands::Build(arg) => build(arg),
    };
    if let Err(error) = result {
        eprintln!("clue: {error}");
        process::exit(1);
    }
}
