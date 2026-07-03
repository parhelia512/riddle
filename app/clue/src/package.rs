use crate::manifest::{self, PackageKind};
use riddlec::pipeline;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

pub struct LoadedPackage {
    pub name: String,
    pub entry: PathBuf,
    pub manifest_fingerprint: String,
    pub source: pipeline::LoadedSource,
}

pub fn load(root: &Path) -> io::Result<LoadedPackage> {
    let mut stack = HashSet::new();
    load_inner(root, PackageKind::Binary, &mut stack)
}

fn load_inner(
    root: &Path,
    kind: PackageKind,
    stack: &mut HashSet<PathBuf>,
) -> io::Result<LoadedPackage> {
    let root = fs::canonicalize(root)?;
    if !stack.insert(root.clone()) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("cyclic package dependency involving `{}`", root.display()),
        ));
    }

    let manifest = manifest::read(&root, kind)?;
    let mut source = String::new();
    let mut files = Vec::new();
    let mut manifest_fingerprint = manifest.fingerprint.clone();
    for dependency in &manifest.dependencies {
        if !is_ident(&dependency.alias) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "dependency name `{}` must be a valid module name",
                    dependency.alias
                ),
            ));
        }

        let dependency_root = root.join(&dependency.path);
        let dependency_package = load_inner(&dependency_root, PackageKind::Library, stack)?;
        if dependency_package.name != dependency.package {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "dependency `{}` expected package `{}`, found `{}`",
                    dependency.alias, dependency.package, dependency_package.name
                ),
            ));
        }

        manifest_fingerprint.push_str(&dependency_package.manifest_fingerprint);
        files.extend(dependency_package.source.files);
        source.push_str(&format!(
            "mod {} {{\n{}\n}}\n\n",
            dependency.alias, dependency_package.source.source
        ));
    }

    let own_source = pipeline::load_source_file(&manifest.entry)?;
    files.extend(own_source.files);
    source.push_str(&own_source.source);
    stack.remove(&root);
    Ok(LoadedPackage {
        name: manifest.name,
        entry: manifest.entry,
        manifest_fingerprint,
        source: pipeline::LoadedSource { source, files },
    })
}

fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}
