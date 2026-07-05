use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Error};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Unix,
    Msvc,
}

#[derive(Debug, Clone)]
struct CCompiler {
    program: OsString,
    flavor: Flavor,
}

pub fn executable_path(c_path: &Path) -> PathBuf {
    let mut path = c_path.with_extension("");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

pub fn compile_file(c_path: &Path, exe_path: &Path) -> io::Result<String> {
    let mut last_error = None;
    for compiler in candidates().into_iter().filter(CCompiler::probe) {
        match compiler.command(c_path, exe_path).status() {
            Ok(status) if status.success() => {
                return Ok(compiler.program.to_string_lossy().into_owned());
            }
            Ok(status) => {
                last_error = Some(Error::other(format!(
                    "C compiler `{}` exited with {status}",
                    compiler.program.to_string_lossy()
                )));
            }
            Err(error) => {
                last_error = Some(Error::other(format!(
                    "failed to run `{}`: {error}",
                    compiler.program.to_string_lossy()
                )));
            }
        }
    }

    Err(last_error
        .unwrap_or_else(|| Error::other(format!("no C compiler found (tried {})", tried()))))
}

fn candidates() -> Vec<CCompiler> {
    let mut candidates = Vec::new();
    if let Some(program) = env::var_os("CC") {
        candidates.push(CCompiler::new(program));
    }
    for program in ["cc", "gcc", "clang"] {
        candidates.push(CCompiler::new(program));
    }
    if cfg!(windows) {
        for program in ["clang-cl", "cl"] {
            candidates.push(CCompiler::new(program));
        }
    }
    candidates
}

fn tried() -> &'static str {
    if cfg!(windows) {
        "$CC, cc, gcc, clang, clang-cl, cl"
    } else {
        "$CC, cc, gcc, clang"
    }
}

impl CCompiler {
    fn new(program: impl Into<OsString>) -> Self {
        let program = program.into();
        let flavor = flavor_for(&program);
        Self { program, flavor }
    }

    fn probe(&self) -> bool {
        let mut command = Command::new(&self.program);
        match self.flavor {
            Flavor::Unix => {
                command.arg("--version");
            }
            Flavor::Msvc => {
                command.arg("/?");
            }
        }
        command.output().is_ok()
    }

    fn command(&self, c_path: &Path, exe_path: &Path) -> Command {
        let mut command = Command::new(&self.program);
        match self.flavor {
            Flavor::Unix => {
                command.arg("-o").arg(exe_path).arg(c_path).arg("-lgc");
            }
            Flavor::Msvc => {
                command
                    .arg("/nologo")
                    .arg(c_path)
                    .arg(format!("/Fe{}", exe_path.display()))
                    .arg(format!("/Fo{}", exe_path.with_extension("obj").display()))
                    .arg("gc.lib");
            }
        }
        command
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_path_uses_platform_extension() {
        let exe = executable_path(Path::new("build/app.c"));
        if cfg!(windows) {
            assert_eq!(exe, PathBuf::from("build/app.exe"));
        } else {
            assert_eq!(exe, PathBuf::from("build/app"));
        }
    }

    #[test]
    fn detects_msvc_drivers() {
        assert_eq!(flavor_for(OsStr::new("cl.exe")), Flavor::Msvc, "cl.exe");
        assert_eq!(
            flavor_for(OsStr::new(r"c:\VS\bin\clang-cl.exe")),
            Flavor::Msvc,
            "windows path clang-cl.exe"
        );
        assert_eq!(flavor_for(OsStr::new("gcc")), Flavor::Unix, "gcc");
    }
}
