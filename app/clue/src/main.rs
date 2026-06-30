use clap::{Args, Parser, Subcommand};
use riddlec::pipeline;
use std::collections::hash_map::DefaultHasher;
use std::ffi::OsStr;
use std::fs::{self, create_dir, create_dir_all, write};
use std::hash::{Hash, Hasher};
use std::io;
use std::io::ErrorKind::AlreadyExists;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::process;

const CLUE_PROJECT_FILE_NAME: &str = "Clue.toml";
const CLUE_PROJECT_FILE_TEMPLATE: &str = r#"[package]
name = "{package_name}"
version = "0.1.0"

[dependencies]
"#;
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
    path: PathBuf,
}

#[derive(Args, Debug, Clone)]
struct BuildArg {
    path: Option<PathBuf>,
}

fn try_create_package(path: impl AsRef<Path>) -> Result<(), Error> {
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
        CLUE_PROJECT_FILE_TEMPLATE.replace("{package_name}", &package_name),
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
    let manifest = root.join(CLUE_PROJECT_FILE_NAME);
    let manifest_text = fs::read_to_string(&manifest)?;
    let package_name = package_name(&manifest_text).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("missing package name in `{}`", manifest.display()),
        )
    })?;
    let entry = entry_file(&root, &package_name)?;
    let loaded = pipeline::load_source_file(&entry)?;
    let result = pipeline::compile(&loaded.source);
    let errors =
        riddlec::diagnostics::report(&result, Some(&loaded.source), &entry.display().to_string());
    if errors > 0 || !result.success() {
        return Err(Error::new(ErrorKind::InvalidData, "build failed"));
    }

    let build_dir = root.join(".clue").join("build");
    create_dir_all(&build_dir)?;
    let c_path = build_dir.join(format!("{package_name}.c"));
    let hash_path = build_dir.join(format!("{package_name}.hash"));
    let hash = fingerprint(&manifest_text, &loaded.source);
    if fs::read_to_string(&hash_path).unwrap_or_default() == hash && c_path.is_file() {
        println!("clue: fresh {}", c_path.display());
        return Ok(());
    }

    let module = result
        .mir_module
        .as_ref()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "missing MIR module"))?;
    let c_code = pipeline::generate_c(module).map_err(Error::other)?;
    fs::write(&c_path, c_code)?;
    fs::write(&hash_path, hash)?;
    println!("clue: built {}", c_path.display());
    Ok(())
}

fn package_name(manifest: &str) -> Option<String> {
    for line in manifest.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "name" {
            continue;
        }
        let value = value.trim().trim_matches('"');
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn entry_file(root: &Path, package_name: &str) -> io::Result<PathBuf> {
    for path in [
        root.join("src").join("main.rid"),
        root.join(format!("{package_name}.rid")),
        root.join("main.rid"),
    ] {
        if path.is_file() {
            return Ok(path);
        }
    }

    Err(Error::new(
        ErrorKind::NotFound,
        "missing entry file; expected src/main.rid, <package>.rid, or main.rid",
    ))
}

fn fingerprint(manifest: &str, source: &str) -> String {
    let mut hasher = DefaultHasher::new();
    manifest.hash(&mut hasher);
    source.hash(&mut hasher);
    riddlec::GIT_HASH.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn main() {
    let args = Arg::parse();
    let result = match args.command {
        Commands::Init(arg) => try_create_package(arg.path),
        Commands::Build(arg) => build(arg),
    };
    if let Err(error) = result {
        eprintln!("clue: {error}");
        process::exit(1);
    }
}
