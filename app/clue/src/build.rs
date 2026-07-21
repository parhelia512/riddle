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
    version: Vec<u8>,
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

    let build_dir = root.join(".clue").join("build");
    fs::create_dir_all(&build_dir)?;
    let compiler = if analysis.kind == ProjectKind::Binary {
        Some(CCompiler::detect(&build_dir)?)
    } else {
        None
    };
    let c_path = build_dir.join(format!("{}.c", analysis.package_name));
    let custom_runtime_path = analysis.runtime_source.as_ref().map(|path| root.join(path));
    let runtime_source = if compiler.is_some() {
        Some(match &custom_runtime_path {
            Some(path) => fs::read_to_string(path)
                .with_context(|| format!("failed to read runtime source `{}`", path.display()))?,
            None => gc::RUNTIME_C.to_owned(),
        })
    } else {
        None
    };
    let runtime_path = compiler.as_ref().map(|_| {
        custom_runtime_path
            .clone()
            .unwrap_or_else(|| build_dir.join(format!("{}.runtime.c", analysis.package_name)))
    });
    let hash_path = build_dir.join(format!("{}.hash", analysis.package_name));
    let hash = fingerprint(
        &analysis.manifest_fingerprint,
        &analysis.source.source,
        runtime_source.as_deref(),
        compiler.as_ref(),
    );
    let source_is_fresh = c_path.is_file()
        && runtime_path.as_ref().is_none_or(|path| path.is_file())
        && fs::read_to_string(&hash_path).unwrap_or_default() == hash;

    if !source_is_fresh {
        let module = analysis
            .result
            .mir_module
            .as_ref()
            .context("successful compilation did not produce MIR")?;
        let c_code = pipeline::generate_c(module).map_err(anyhow::Error::msg)?;
        fs::write(&c_path, c_code)?;
        if analysis.runtime_source.is_none()
            && let (Some(path), Some(source)) = (&runtime_path, &runtime_source)
        {
            fs::write(path, source)?;
        }
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

    compiler.compile(
        &[
            c_path.as_path(),
            runtime_path
                .as_deref()
                .context("binary build did not select a runtime")?,
        ],
        &executable,
    )?;
    fs::write(&hash_path, hash)?;
    println!("clue: built {}", executable.display());
    Ok(BuildArtifact::Executable(executable))
}

fn fingerprint(
    manifest: &str,
    source: &str,
    runtime: Option<&str>,
    compiler: Option<&CCompiler>,
) -> String {
    let mut hasher = DefaultHasher::new();
    manifest.hash(&mut hasher);
    source.hash(&mut hasher);
    runtime.hash(&mut hasher);
    riddlec::GIT_HASH.hash(&mut hasher);
    env::consts::OS.hash(&mut hasher);
    env::consts::ARCH.hash(&mut hasher);
    if let Some(compiler) = compiler {
        compiler.program.hash(&mut hasher);
        compiler.flavor.hash(&mut hasher);
        compiler.version.hash(&mut hasher);
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
    fn detect(build_dir: &Path) -> anyhow::Result<Self> {
        if let Some(program) = env::var_os("CC") {
            let compiler = Self::new(program.clone()).ok_or_else(|| {
                anyhow::anyhow!(
                    "C compiler from CC `{}` could not report its version",
                    program.to_string_lossy()
                )
            })?;
            if compiler.probe(build_dir) {
                return Ok(compiler);
            }
            bail!(
                "C compiler from CC `{}` cannot compile and link C11",
                program.to_string_lossy()
            );
        }

        let mut programs = ["cc", "gcc", "clang"]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        if cfg!(windows) {
            programs.extend(["clang-cl", "cl"].into_iter().map(OsString::from));
        }
        programs.extend(versioned_compilers());
        let tried = programs
            .iter()
            .map(|program| program.to_string_lossy())
            .collect::<Vec<_>>()
            .join(", ");
        programs
            .into_iter()
            .filter_map(Self::new)
            .find(|compiler| compiler.probe(build_dir))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no usable C11 compiler and linker found; tried {tried}; set CC to a compiler executable"
                )
            })
    }

    fn new(program: OsString) -> Option<Self> {
        let program = resolve_program(&program);
        let flavor = flavor_for(&program);
        let output = Command::new(&program)
            .arg(match flavor {
                Flavor::Unix => "--version",
                Flavor::Msvc => "/?",
            })
            .output()
            .ok()?;
        let mut version = output.stdout;
        version.extend(output.stderr);
        (!version.is_empty()).then_some(Self {
            program,
            flavor,
            version,
        })
    }

    fn probe(&self, build_dir: &Path) -> bool {
        let identity = self.identity();
        let stamp = build_dir.join(format!(".cc-{identity:016x}"));
        if stamp.is_file() {
            return true;
        }

        let source = build_dir.join(format!(".cc-{identity:016x}.c"));
        let executable = executable_path(&source);
        if fs::write(&source, "int main(void) { return 0; }\n").is_err() {
            return false;
        }
        let success = self
            .command(&[source.as_path()], &executable)
            .output()
            .is_ok_and(|output| output.status.success() && executable.is_file());
        let _ = fs::remove_file(&source);
        let _ = fs::remove_file(&executable);
        let _ = fs::remove_file(source.with_extension("obj"));
        if success {
            let _ = fs::write(stamp, b"c11\n");
        }
        success
    }

    fn identity(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.program.hash(&mut hasher);
        self.flavor.hash(&mut hasher);
        self.version.hash(&mut hasher);
        hasher.finish()
    }

    fn compile(&self, sources: &[&Path], executable: &Path) -> anyhow::Result<()> {
        let status = self
            .command(sources, executable)
            .status()
            .with_context(|| {
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

    fn command(&self, sources: &[&Path], executable: &Path) -> Command {
        let mut command = Command::new(&self.program);
        match self.flavor {
            Flavor::Unix => {
                command.args(["-std=c11", "-O2"]);
                command.args(sources).arg("-o").arg(executable);
            }
            Flavor::Msvc => {
                command.args(["/nologo", "/std:c11", "/O2"]);
                command
                    .args(sources)
                    .arg(format!("/Fe{}", executable.display()));
            }
        }
        command.current_dir(executable.parent().unwrap_or_else(|| Path::new(".")));
        command
    }
}

fn resolve_program(program: &OsStr) -> OsString {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .into_os_string();
    }
    let Some(search_path) = env::var_os("PATH") else {
        return program.to_owned();
    };
    for directory in env::split_paths(&search_path) {
        let direct = directory.join(path);
        if direct.is_file() {
            return fs::canonicalize(&direct).unwrap_or(direct).into_os_string();
        }
        if cfg!(windows) {
            let executable = directory.join(format!("{}.exe", program.to_string_lossy()));
            if executable.is_file() {
                return fs::canonicalize(&executable)
                    .unwrap_or(executable)
                    .into_os_string();
            }
        }
    }
    program.to_owned()
}

fn versioned_compilers() -> Vec<OsString> {
    let Some(path) = env::var_os("PATH") else {
        return Vec::new();
    };
    let mut programs = env::split_paths(&path)
        .enumerate()
        .flat_map(|(path_index, directory)| {
            fs::read_dir(directory)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(move |entry| {
                    let version = versioned_compiler_name(&entry.file_name())?;
                    entry
                        .path()
                        .is_file()
                        .then_some((version, path_index, entry.path()))
                })
        })
        .collect::<Vec<_>>();
    programs.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
    });
    programs
        .into_iter()
        .map(|(_, _, path)| path.into_os_string())
        .collect()
}

fn versioned_compiler_name(name: &OsStr) -> Option<Vec<u32>> {
    let name = name.to_string_lossy().to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    let version = ["clang-cl-", "clang-", "gcc-"]
        .into_iter()
        .find_map(|prefix| name.strip_prefix(prefix))?;
    (!version.is_empty() && version.chars().all(|ch| ch.is_ascii_digit() || ch == '.')).then(|| {
        version
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    })
}

fn flavor_for(program: &OsStr) -> Flavor {
    let name = program
        .to_string_lossy()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    if name == "cl" || name == "clang-cl" || name.starts_with("clang-cl-") {
        Flavor::Msvc
    } else {
        Flavor::Unix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_versioned_compiler_names_and_flavors() {
        assert_eq!(
            versioned_compiler_name(OsStr::new("clang-18.1.exe")),
            Some(vec![18, 1])
        );
        assert_eq!(
            versioned_compiler_name(OsStr::new("gcc-13")),
            Some(vec![13])
        );
        assert_eq!(
            versioned_compiler_name(OsStr::new("clang-cl-19")),
            Some(vec![19])
        );
        assert_eq!(versioned_compiler_name(OsStr::new("gcc-helper")), None);
        assert!(matches!(
            flavor_for(OsStr::new("C:\\LLVM\\clang-cl-18.exe")),
            Flavor::Msvc
        ));
    }
}
