use crate::{ProjectKind, analyze_project};
use anyhow::{Context, bail};
use riddlec::pipeline;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) enum BuildArtifact {
    Executable(PathBuf),
    Library,
}

#[derive(Debug, Clone, Copy, Hash)]
enum Flavor {
    Unix,
    Msvc,
}

#[derive(Debug, Clone)]
struct CCompiler {
    program: OsString,
    flavor: Flavor,
}

pub(crate) fn run(root: &Path) -> anyhow::Result<BuildArtifact> {
    let root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        env::current_dir()?.join(root)
    };
    let analysis = analyze_project(&root, &HashMap::new())?;
    let errors = riddlec::diagnostics::report_mapped(
        &analysis.result,
        &analysis.source,
        &analysis.entry.display().to_string(),
    );
    if errors > 0 || !analysis.result.success() {
        bail!("build failed");
    }

    let compiler = if analysis.kind == ProjectKind::Binary {
        Some(CCompiler::detect()?)
    } else {
        None
    };
    let build_dir = root.join(".clue").join("build");
    fs::create_dir_all(&build_dir)?;
    let c_path = build_dir.join(format!("{}.c", analysis.package_name));
    let hash_path = build_dir.join(format!("{}.hash", analysis.package_name));
    let hash = fingerprint(
        &analysis.manifest_fingerprint,
        &analysis.source.source,
        compiler.as_ref(),
    );
    let source_is_fresh =
        c_path.is_file() && fs::read_to_string(&hash_path).unwrap_or_default() == hash;

    if !source_is_fresh {
        let module = analysis
            .result
            .mir_module
            .as_ref()
            .context("successful compilation did not produce MIR")?;
        let c_code = pipeline::generate_c(module).map_err(anyhow::Error::msg)?;
        fs::write(&c_path, c_code)?;
    }

    let Some(compiler) = compiler else {
        if source_is_fresh {
            println!("clue: fresh {}", c_path.display());
        } else {
            fs::write(&hash_path, hash)?;
            println!("clue: built {}", c_path.display());
        }
        return Ok(BuildArtifact::Library);
    };

    let executable = executable_path(&c_path);
    if source_is_fresh && executable.is_file() {
        println!("clue: fresh {}", executable.display());
        return Ok(BuildArtifact::Executable(executable));
    }

    compiler.compile(&c_path, &executable)?;
    fs::write(&hash_path, hash)?;
    println!("clue: built {}", executable.display());
    Ok(BuildArtifact::Executable(executable))
}

fn fingerprint(manifest: &str, source: &str, compiler: Option<&CCompiler>) -> String {
    let mut hasher = DefaultHasher::new();
    manifest.hash(&mut hasher);
    source.hash(&mut hasher);
    riddlec::GIT_HASH.hash(&mut hasher);
    env::consts::OS.hash(&mut hasher);
    env::consts::ARCH.hash(&mut hasher);
    if let Some(compiler) = compiler {
        compiler.program.hash(&mut hasher);
        compiler.flavor.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

fn executable_path(c_path: &Path) -> PathBuf {
    let mut path = c_path.with_extension("");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

impl CCompiler {
    fn detect() -> anyhow::Result<Self> {
        let mut programs = Vec::new();
        if let Some(program) = env::var_os("CC") {
            programs.push(program);
        }
        programs.extend(["cc", "gcc", "clang"].into_iter().map(OsString::from));
        if cfg!(windows) {
            programs.extend(["clang-cl", "cl"].into_iter().map(OsString::from));
        }
        programs
            .into_iter()
            .map(Self::new)
            .find(Self::probe)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no C compiler found; set CC or install cc, gcc, clang, clang-cl, or cl"
                )
            })
    }

    fn new(program: OsString) -> Self {
        let flavor = flavor_for(&program);
        Self { program, flavor }
    }

    fn probe(&self) -> bool {
        let mut command = Command::new(&self.program);
        command.arg(match self.flavor {
            Flavor::Unix => "--version",
            Flavor::Msvc => "/?",
        });
        command.output().is_ok_and(|output| output.status.success())
    }

    fn compile(&self, c_path: &Path, executable: &Path) -> anyhow::Result<()> {
        let mut command = Command::new(&self.program);
        match self.flavor {
            Flavor::Unix => {
                command
                    .args(["-std=c11", "-O2"])
                    .arg(c_path)
                    .arg("-o")
                    .arg(executable);
            }
            Flavor::Msvc => {
                command
                    .args(["/nologo", "/std:c11", "/O2"])
                    .arg(c_path)
                    .arg(format!("/Fe{}", executable.display()))
                    .arg(format!("/Fo{}", executable.with_extension("obj").display()));
            }
        }
        let status = command.status().with_context(|| {
            format!(
                "failed to run C compiler `{}`",
                self.program.to_string_lossy()
            )
        })?;
        if !status.success() {
            bail!(
                "C compiler `{}` exited with {status}",
                self.program.to_string_lossy()
            );
        }
        Ok(())
    }
}

fn flavor_for(program: &OsStr) -> Flavor {
    let name = program
        .to_string_lossy()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match name.as_str() {
        "cl" | "cl.exe" | "clang-cl" | "clang-cl.exe" => Flavor::Msvc,
        _ => Flavor::Unix,
    }
}
