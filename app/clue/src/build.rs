use crate::target::{self, TargetConfig};
use crate::{ProjectKind, TargetTriple, analyze_project};
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

pub enum BuildArtifact {
    Executable { path: PathBuf, target: TargetTriple },
    Library,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Flavor {
    Unix,
    Msvc,
}

#[derive(Debug, Clone)]
struct CCompiler {
    program: OsString,
    flavor: Flavor,
    version: Vec<u8>,
    clang: bool,
    target: TargetConfig,
}

pub fn run(root: &Path, explicit_target: Option<TargetTriple>) -> anyhow::Result<BuildArtifact> {
    let root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        env::current_dir()?.join(root)
    };
    let analysis = analyze_project(&root, &HashMap::new())?;
    ensure_analysis_success(&analysis)?;
    let triple = target::resolve(explicit_target, analysis.build_target.as_deref())?;
    let target = target::load(triple, analysis.kind == ProjectKind::Binary)?;
    build_analysis(&root, &analysis, triple, &target)
}

fn ensure_analysis_success(analysis: &crate::ProjectAnalysis) -> anyhow::Result<()> {
    let errors = riddlec::diagnostics::report_mapped(
        &analysis.result,
        &analysis.source,
        &analysis.entry.display().to_string(),
    );
    if errors > 0 || !analysis.result.success() {
        bail!("build failed");
    }
    Ok(())
}

fn build_analysis(
    root: &Path,
    analysis: &crate::ProjectAnalysis,
    triple: TargetTriple,
    target: &TargetConfig,
) -> anyhow::Result<BuildArtifact> {
    let build_dir = root.join(".clue").join("build");
    fs::create_dir_all(&build_dir)?;
    let compiler = if analysis.kind == ProjectKind::Binary {
        Some(CCompiler::detect(&build_dir, target.clone())?)
    } else {
        None
    };
    let c_path = build_dir.join(format!("{}.c", analysis.package_name));
    let custom_runtime_path = analysis.runtime_source.as_ref().map(|path| root.join(path));
    let runtime_source = if compiler.is_some() {
        Some(if analysis.gc_enabled {
            match &custom_runtime_path {
                Some(path) => fs::read_to_string(path).with_context(|| {
                    format!("failed to read runtime source `{}`", path.display())
                })?,
                None => match &target.runtime_source {
                    Some(path) => fs::read_to_string(path).with_context(|| {
                        format!("failed to read target runtime `{}`", path.display())
                    })?,
                    None => gc::RUNTIME_C.to_owned(),
                },
            }
        } else {
            gc::NO_GC_RUNTIME_C.to_owned()
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
        target,
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
        let c_code = pipeline::generate_c_with_gc(module, analysis.gc_enabled)
            .map_err(anyhow::Error::msg)?;
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

    let executable = executable_path(&c_path, triple);
    if source_is_fresh && executable.is_file() {
        println!("clue: fresh {}", executable.display());
        return Ok(BuildArtifact::Executable {
            path: executable,
            target: triple,
        });
    }

    compiler.compile(
        &[
            c_path.as_path(),
            runtime_path
                .as_deref()
                .context("binary build did not select a runtime")?,
        ],
        &executable,
        &[],
        false,
        &[],
    )?;
    fs::write(&hash_path, hash)?;
    println!("clue: built {}", executable.display());
    Ok(BuildArtifact::Executable {
        path: executable,
        target: triple,
    })
}

pub struct ProcMacroArtifacts {
    pub library: PathBuf,
    pub runner: PathBuf,
}

pub fn build_proc_macro_library(
    package: &crate::project::ProcMacroPackage,
    exports: &[crate::proc_macro::HostMacroExport],
    expanded_source: &str,
) -> anyhow::Result<ProcMacroArtifacts> {
    let host = TargetTriple::host().map_err(anyhow::Error::msg)?;
    let target = target::load(host, true)?;
    let build_dir = package.root.join(".clue").join("build");
    fs::create_dir_all(&build_dir)?;
    let compiler = CCompiler::detect(&build_dir, target.clone())?;

    let source = crate::proc_macro::host_source(expanded_source, exports);
    let bridge = crate::proc_macro::host_runtime_c(exports);
    let c_path = build_dir.join(format!("{}.proc-macro.c", package.name));
    let runtime_path = build_dir.join(format!("{}.proc-macro.runtime.c", package.name));
    let bridge_path = build_dir.join(format!("{}.proc-macro.host.c", package.name));
    let runner_c_path = build_dir.join(format!("{}.proc-macro.runner.c", package.name));
    let hash_path = build_dir.join(format!("{}.proc-macro.hash", package.name));
    let library = build_dir.join(format!(
        "{}{}.proc-macro{}",
        env::consts::DLL_PREFIX,
        package.name,
        env::consts::DLL_SUFFIX
    ));
    let runner = build_dir.join(format!(
        "{}.proc-macro-runner{}",
        package.name,
        host.executable_suffix()
    ));
    let runner_source = crate::proc_macro::proc_macro_runner_c();
    let hash = fingerprint(
        &format!(
            "{}\n{bridge}\n{runner_source}",
            package.manifest_fingerprint
        ),
        &source,
        Some(gc::RUNTIME_C),
        Some(&compiler),
        &target,
    );
    if c_path.is_file()
        && runtime_path.is_file()
        && bridge_path.is_file()
        && runner_c_path.is_file()
        && library.is_file()
        && runner.is_file()
        && fs::read_to_string(&hash_path).unwrap_or_default() == hash
    {
        return Ok(ProcMacroArtifacts { library, runner });
    }

    let analysis = pipeline::compile_with_options(&source, pipeline::CompileOptions::default());
    let errors = riddlec::diagnostics::report(
        &analysis,
        Some(&source),
        &package.entry.display().to_string(),
    );
    if errors > 0 || !analysis.success() {
        bail!("failed to compile proc-macro package `{}`", package.name);
    }
    let module = analysis
        .mir_module
        .as_ref()
        .context("successful proc-macro compilation did not produce MIR")?;
    fs::write(
        &c_path,
        pipeline::generate_c(module).map_err(anyhow::Error::msg)?,
    )?;
    fs::write(&runtime_path, gc::RUNTIME_C)?;
    fs::write(&bridge_path, bridge)?;
    fs::write(&runner_c_path, runner_source)?;
    compiler.compile(
        &[
            c_path.as_path(),
            runtime_path.as_path(),
            bridge_path.as_path(),
        ],
        &library,
        &["putchar=riddle_proc_putchar"],
        true,
        // ponytail: glibc folds `putchar` into `putc(c, stdout)` at -O2 after
        // macro expansion, bypassing the -D rename; -fno-inline stops it.
        &["-fno-inline"],
    )?;
    let runner_args = if host.is_linux() { &["-ldl"][..] } else { &[] };
    compiler.compile(&[runner_c_path.as_path()], &runner, &[], false, runner_args)?;
    fs::write(hash_path, hash)?;
    Ok(ProcMacroArtifacts { library, runner })
}

fn fingerprint(
    manifest: &str,
    source: &str,
    runtime: Option<&str>,
    compiler: Option<&CCompiler>,
    target: &TargetConfig,
) -> String {
    let mut hasher = DefaultHasher::new();
    manifest.hash(&mut hasher);
    source.hash(&mut hasher);
    runtime.hash(&mut hasher);
    "c11".hash(&mut hasher);
    riddlec::GIT_HASH.hash(&mut hasher);
    target.hash(&mut hasher);
    if let Some(compiler) = compiler {
        compiler.program.hash(&mut hasher);
        compiler.flavor.hash(&mut hasher);
        compiler.version.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

fn executable_path(c_path: &Path, target: TargetTriple) -> PathBuf {
    let mut path = c_path.with_extension("");
    if !target.executable_suffix().is_empty() {
        path.set_extension(target.executable_suffix().trim_start_matches('.'));
    }
    path
}

impl CCompiler {
    fn detect(build_dir: &Path, target: TargetConfig) -> anyhow::Result<Self> {
        if let Some(program) = env::var_os("CC") {
            let compiler = Self::new(&program, target).ok_or_else(|| {
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

        if let Some(program) = &target.c_toolchain.compiler {
            let compiler = Self::new(program.as_os_str(), target.clone()).ok_or_else(|| {
                anyhow::anyhow!(
                    "configured C compiler `{}` could not report its version",
                    program.display()
                )
            })?;
            if compiler.probe(build_dir) {
                return Ok(compiler);
            }
            bail!(
                "configured C compiler `{}` cannot compile and link for `{}`",
                program.display(),
                target.triple
            );
        }

        let candidates: &[&str] = if target.triple.is_windows() && cfg!(windows) {
            &["clang-cl", "clang", "cc", "gcc", "cl"]
        } else if target.triple.is_windows() {
            &["clang", "cc", "gcc", "clang-cl"]
        } else {
            &["clang", "cc", "gcc"]
        };
        let mut programs = candidates
            .iter()
            .copied()
            .map(OsString::from)
            .collect::<Vec<_>>();
        programs.extend(versioned_compilers());
        let tried = programs
            .iter()
            .map(|program| program.to_string_lossy())
            .collect::<Vec<_>>()
            .join(", ");
        programs
            .into_iter()
            .filter_map(|program| Self::new(&program, target.clone()))
            .find(|compiler| compiler.probe(build_dir))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no usable C11 compiler and linker found; tried {tried}; set CC to a compiler executable"
                )
            })
    }

    fn new(program: &OsStr, target: TargetConfig) -> Option<Self> {
        let program = resolve_program(program);
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
        let clang = program_name(&program).starts_with("clang")
            || String::from_utf8_lossy(&version)
                .to_ascii_lowercase()
                .contains("clang");
        let host = TargetTriple::host().ok()?;
        if target.triple != host && !clang {
            return None;
        }
        if !target.triple.is_windows() && flavor == Flavor::Msvc {
            return None;
        }
        (!version.is_empty()).then_some(Self {
            program,
            flavor,
            version,
            clang,
            target,
        })
    }

    fn probe(&self, build_dir: &Path) -> bool {
        let identity = self.identity();
        let stamp = build_dir.join(format!(".cc-{identity:016x}"));
        if stamp.is_file() {
            return true;
        }

        let source = build_dir.join(format!(".cc-{identity:016x}.c"));
        let executable = executable_path(&source, self.target.triple);
        if fs::write(&source, "int main(void) { return 0; }\n").is_err() {
            return false;
        }
        let success = self
            .command(&[source.as_path()], &executable, &[], false, &[])
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
        self.clang.hash(&mut hasher);
        self.target.hash(&mut hasher);
        hasher.finish()
    }

    fn compile(
        &self,
        sources: &[&Path],
        executable: &Path,
        defines: &[&str],
        shared_library: bool,
        extra_args: &[&str],
    ) -> anyhow::Result<()> {
        let status = self
            .command(sources, executable, defines, shared_library, extra_args)
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

    fn command(
        &self,
        sources: &[&Path],
        executable: &Path,
        defines: &[&str],
        shared_library: bool,
        extra_args: &[&str],
    ) -> Command {
        let mut command = Command::new(&self.program);
        let host = TargetTriple::host().ok();
        let cross = host.is_some_and(|host| host != self.target.triple);
        match self.flavor {
            Flavor::Unix => {
                command.args(["-std=c11", "-O2"]);
                if cross && self.clang {
                    command.arg(format!("--target={}", self.target.triple));
                } else if matches!(
                    self.target.triple,
                    TargetTriple::I686UnknownLinuxGnu | TargetTriple::I686PcWindowsMsvc
                ) {
                    command.arg("-m32");
                } else if self.target.triple == TargetTriple::X86_64UnknownLinuxGnu {
                    command.arg("-m64");
                }
                if cross {
                    command.arg("-fuse-ld=lld");
                }
                if shared_library {
                    command.arg(if self.target.triple.is_macos() {
                        "-dynamiclib"
                    } else {
                        "-shared"
                    });
                    if !self.target.triple.is_windows() {
                        command.arg("-fPIC");
                    }
                }
                self.apply_unix_target_options(&mut command);
                for define in defines {
                    command.arg(format!("-D{define}"));
                }
                command
                    .args(sources)
                    .args(extra_args)
                    .arg("-o")
                    .arg(executable);
            }
            Flavor::Msvc => {
                command.args(["/nologo", "/std:c11", "/O2"]);
                if self.clang && (cross || self.target.triple == TargetTriple::I686PcWindowsMsvc) {
                    command.arg(format!("--target={}", self.target.triple));
                }
                if cross && self.clang {
                    command.arg("-fuse-ld=lld");
                }
                if shared_library {
                    command.arg("/LD");
                }
                self.apply_msvc_target_options(&mut command);
                for define in defines {
                    command.arg(format!("/D{define}"));
                }
                if self.clang {
                    command.args(extra_args);
                }
                command
                    .args(sources)
                    .arg(format!("/Fe{}", executable.display()));
            }
        }
        command.current_dir(executable.parent().unwrap_or_else(|| Path::new(".")));
        command
    }

    fn apply_unix_target_options(&self, command: &mut Command) {
        if let Some(sysroot) = &self.target.c_toolchain.sysroot {
            if self.target.triple.is_macos() {
                command.arg("-isysroot").arg(sysroot);
            } else {
                command.arg(format!("--sysroot={}", sysroot.display()));
            }
        }
        if self.target.triple.is_windows() {
            for include in self.windows_include_paths() {
                command.arg("-isystem").arg(include);
            }
            for library in self.windows_library_paths() {
                command.arg("-L").arg(library);
            }
        }
    }

    fn apply_msvc_target_options(&self, command: &mut Command) {
        let includes = self.windows_include_paths();
        if !includes.is_empty()
            && let Ok(value) = env::join_paths(includes)
        {
            command.env("INCLUDE", value);
        }
        let libraries = self.windows_library_paths();
        if !libraries.is_empty()
            && let Ok(value) = env::join_paths(libraries)
        {
            command.env("LIB", value);
        }
    }

    fn windows_include_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(msvc) = &self.target.c_toolchain.msvc {
            paths.push(msvc.join("include"));
        }
        if let Some(sdk) = &self.target.c_toolchain.windows_sdk
            && let Some(version) = latest_child(&sdk.join("Include"))
        {
            for name in ["ucrt", "shared", "um", "winrt"] {
                paths.push(version.join(name));
            }
        }
        paths
    }

    fn windows_library_paths(&self) -> Vec<PathBuf> {
        let arch = self.target.triple.msvc_architecture();
        let mut paths = Vec::new();
        if let Some(msvc) = &self.target.c_toolchain.msvc {
            paths.push(msvc.join("lib").join(arch));
        }
        if let Some(sdk) = &self.target.c_toolchain.windows_sdk
            && let Some(version) = latest_child(&sdk.join("Lib"))
        {
            paths.push(version.join("ucrt").join(arch));
            paths.push(version.join("um").join(arch));
        }
        paths
    }
}

fn latest_child(root: &Path) -> Option<PathBuf> {
    let mut directories = fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    directories.sort();
    directories.pop()
}

fn resolve_program(program: &OsStr) -> OsString {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return ordinary_absolute(path).into_os_string();
    }
    let Some(search_path) = env::var_os("PATH") else {
        return program.to_owned();
    };
    for directory in env::split_paths(&search_path) {
        let direct = directory.join(path);
        if direct.is_file() {
            return ordinary_absolute(&direct).into_os_string();
        }
        if cfg!(windows) {
            let executable = directory.join(format!("{}.exe", program.to_string_lossy()));
            if executable.is_file() {
                return ordinary_absolute(&executable).into_os_string();
            }
        }
    }
    program.to_owned()
}

fn ordinary_absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().map_or_else(|_| path.to_path_buf(), |current| current.join(path))
    }
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
    let name = program_name(program);
    if name == "cl" || name == "clang-cl" || name.starts_with("clang-cl-") {
        Flavor::Msvc
    } else {
        Flavor::Unix
    }
}

fn program_name(program: &OsStr) -> String {
    let name = program
        .to_string_lossy()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::CToolchainConfig;

    fn shared_library_args(flavor: Flavor, triple: TargetTriple, clang: bool) -> Vec<String> {
        CCompiler {
            program: "cc".into(),
            flavor,
            version: Vec::new(),
            clang,
            target: TargetConfig {
                triple,
                runtime_source: None,
                c_toolchain: CToolchainConfig::default(),
            },
        }
        .command(&[Path::new("input.c")], Path::new("output"), &[], true, &[])
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
    }

    #[test]
    fn shared_library_flags_cover_supported_platforms() {
        let linux = shared_library_args(Flavor::Unix, TargetTriple::I686UnknownLinuxGnu, false);
        assert!(linux.iter().any(|argument| argument == "-shared"));
        assert!(linux.iter().any(|argument| argument == "-fPIC"));
        assert!(linux.iter().any(|argument| argument == "-m32"));

        let macos = shared_library_args(Flavor::Unix, TargetTriple::Aarch64AppleDarwin, true);
        assert!(macos.iter().any(|argument| argument == "-dynamiclib"));

        let windows = shared_library_args(Flavor::Msvc, TargetTriple::I686PcWindowsMsvc, true);
        assert!(windows.iter().any(|argument| argument == "/LD"));
        assert!(
            windows
                .iter()
                .any(|argument| argument == "--target=i686-pc-windows-msvc")
        );
    }
}
