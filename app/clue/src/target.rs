use anyhow::{Context, bail};
use riddlec::target::TargetTriple;
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Default, Hash)]
pub(crate) struct CToolchainConfig {
    pub compiler: Option<PathBuf>,
    pub sysroot: Option<PathBuf>,
    pub windows_sdk: Option<PathBuf>,
    pub msvc: Option<PathBuf>,
}

#[derive(Debug, Clone, Hash)]
pub(crate) struct TargetConfig {
    pub triple: TargetTriple,
    pub runtime_source: Option<PathBuf>,
    pub c_toolchain: CToolchainConfig,
}

#[derive(Deserialize)]
struct ComponentManifest {
    schema: u32,
    triple: String,
    runtime: PathBuf,
}

#[derive(Default, Deserialize)]
struct CToolchainFile {
    compiler: Option<PathBuf>,
    sysroot: Option<PathBuf>,
    windows_sdk: Option<PathBuf>,
    msvc: Option<PathBuf>,
}

pub(crate) fn resolve(
    explicit: Option<TargetTriple>,
    manifest: Option<&str>,
) -> anyhow::Result<TargetTriple> {
    if let Some(target) = explicit {
        return Ok(target);
    }
    if let Some(target) = env::var_os("RIDDLE_TARGET") {
        return target
            .to_string_lossy()
            .parse()
            .map_err(anyhow::Error::msg)
            .context("invalid RIDDLE_TARGET");
    }
    if let Some(target) = manifest {
        return target
            .parse()
            .map_err(anyhow::Error::msg)
            .context("invalid build.target in Clue.toml");
    }
    TargetTriple::host().map_err(anyhow::Error::msg)
}

pub(crate) fn load(triple: TargetTriple, require_component: bool) -> anyhow::Result<TargetConfig> {
    let host = TargetTriple::host().map_err(anyhow::Error::msg)?;
    if triple == host {
        let c_toolchain = component_root(triple)
            .map(|root| load_c_toolchain(&root))
            .transpose()?
            .unwrap_or_default();
        return Ok(TargetConfig {
            triple,
            runtime_source: None,
            c_toolchain,
        });
    }
    if !require_component {
        return Ok(TargetConfig {
            triple,
            runtime_source: None,
            c_toolchain: CToolchainConfig::default(),
        });
    }

    let root = component_root(triple).ok_or_else(|| {
        anyhow::anyhow!(
            "target component `{triple}` is not installed; run `ridup target add {triple}`"
        )
    })?;
    let manifest_path = root.join("target.toml");
    let manifest_text = fs::read_to_string(&manifest_path).with_context(|| {
        format!(
            "target component `{triple}` is incomplete; missing `{}`",
            manifest_path.display()
        )
    })?;
    let manifest: ComponentManifest = toml::from_str(&manifest_text)
        .with_context(|| format!("invalid target component `{}`", manifest_path.display()))?;
    if manifest.schema != 1 {
        bail!(
            "unsupported target component schema {} in `{}`",
            manifest.schema,
            manifest_path.display()
        );
    }
    if manifest.triple != triple.as_str() {
        bail!(
            "target component `{}` describes `{}` instead of `{triple}`",
            manifest_path.display(),
            manifest.triple
        );
    }
    if !safe_relative(&manifest.runtime) {
        bail!(
            "target component runtime path `{}` is not a safe relative path",
            manifest.runtime.display()
        );
    }
    let runtime_source = root.join(&manifest.runtime);
    if !runtime_source.is_file() {
        bail!(
            "target component `{triple}` is incomplete; missing runtime `{}`",
            runtime_source.display()
        );
    }

    let c_toolchain = load_c_toolchain(&root)?;
    if triple.is_linux() && c_toolchain.sysroot.is_none() {
        bail!(
            "C toolchain for `{triple}` is missing a Linux sysroot; run `ridup target configure {triple} --sysroot <path>`"
        );
    }
    if triple.is_macos() && host.operating_system() != "macos" && c_toolchain.sysroot.is_none() {
        bail!(
            "C toolchain for `{triple}` is missing an Apple SDK; run `ridup target configure {triple} --sysroot <sdk-path>`"
        );
    }
    if triple.is_windows()
        && triple != host
        && (c_toolchain.windows_sdk.is_none() || c_toolchain.msvc.is_none())
    {
        bail!(
            "C toolchain for `{triple}` requires Windows SDK and MSVC paths; run `ridup target configure {triple} --windows-sdk <path> --msvc <path>`"
        );
    }
    Ok(TargetConfig {
        triple,
        runtime_source: Some(runtime_source),
        c_toolchain,
    })
}

fn component_root(triple: TargetTriple) -> Option<PathBuf> {
    if let Some(path) = env::var_os("RIDDLE_TARGET_ROOT") {
        return Some(PathBuf::from(path));
    }
    env::var_os("RIDUP_TOOLCHAIN_ROOT")
        .map(PathBuf::from)
        .map(|root| root.join("targets").join(triple.as_str()))
}

fn load_c_toolchain(root: &Path) -> anyhow::Result<CToolchainConfig> {
    let path = root.join("c-toolchain.toml");
    let file = match fs::read_to_string(&path) {
        Ok(text) => toml::from_str::<CToolchainFile>(&text)
            .with_context(|| format!("invalid C toolchain config `{}`", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => CToolchainFile::default(),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to read C toolchain config `{}`", path.display())
            });
        }
    };
    Ok(CToolchainConfig {
        compiler: resolve_path(root, file.compiler),
        sysroot: resolve_path(root, file.sysroot),
        windows_sdk: resolve_path(root, file.windows_sdk),
        msvc: resolve_path(root, file.msvc),
    })
}

fn resolve_path(root: &Path, path: Option<PathBuf>) -> Option<PathBuf> {
    path.map(|path| {
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    })
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_component_runtime_paths() {
        assert!(safe_relative(Path::new("runtime.c")));
        assert!(!safe_relative(Path::new("../runtime.c")));
        assert!(!safe_relative(Path::new("/runtime.c")));
    }
}
