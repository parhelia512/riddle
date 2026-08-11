use crate::ProjectKind;
use crate::manifest::{self, CLUE_PROJECT_FILE_NAME, Manifest, WorkspaceManifest};
use crate::model::{DependencyKind, DependencySource, DependencySpec};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::{Component, Path, PathBuf};

#[derive(Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub lock_path: PathBuf,
    members: Vec<PathBuf>,
    packages: BTreeMap<PathBuf, WorkspacePackage>,
    dependencies: BTreeMap<PathBuf, Vec<PathBuf>>,
}

#[derive(Clone)]
pub struct WorkspacePackage {
    pub root: PathBuf,
    pub manifest: Manifest,
}

impl Workspace {
    pub fn load(root: &Path) -> io::Result<Self> {
        let root = canonicalize(root)?;
        let workspace = manifest::read_workspace(&root)?.ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "missing `[workspace]` in `{}`",
                    root.join(CLUE_PROJECT_FILE_NAME).display()
                ),
            )
        })?;
        if !manifest::is_virtual_workspace_root(&root)? {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "workspace root `{}` must not contain [package]",
                    root.join(CLUE_PROJECT_FILE_NAME).display()
                ),
            ));
        }
        let members = resolve_members(&root, &workspace)?;
        let (packages, dependencies) = {
            let mut builder = GraphBuilder::new(&root, &members);
            for member in &members {
                builder.visit(member, ProjectKind::Binary)?;
            }
            validate_unique_names(&builder.packages)?;
            (builder.packages, builder.dependencies)
        };
        Ok(Self {
            root: root.clone(),
            lock_path: root.join("Clue.lock"),
            members,
            packages,
            dependencies,
        })
    }

    #[must_use]
    pub fn members(&self) -> &[PathBuf] {
        &self.members
    }

    #[must_use]
    pub fn package(&self, root: &Path) -> Option<&WorkspacePackage> {
        self.packages.get(root)
    }

    #[must_use]
    pub fn package_by_name(&self, name: &str) -> Option<&WorkspacePackage> {
        self.packages
            .values()
            .find(|package| package.manifest.name == name && self.members.contains(&package.root))
    }

    #[must_use]
    pub fn member_for_path(&self, path: &Path) -> Option<PathBuf> {
        let path = canonicalize(path).ok()?;
        self.members
            .iter()
            .find(|member| path == **member || path.starts_with(member))
            .cloned()
    }

    #[must_use]
    pub fn ordered_members(&self) -> Vec<PathBuf> {
        let member_set = self.members.iter().cloned().collect::<BTreeSet<_>>();
        let mut ordered = Vec::with_capacity(self.members.len());
        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        for member in &self.members {
            visit_order(
                member,
                &member_set,
                &self.dependencies,
                &mut visiting,
                &mut visited,
                &mut ordered,
            );
        }
        ordered
    }

    #[must_use]
    pub fn ordered_batches(&self, selected: &[PathBuf]) -> Vec<Vec<PathBuf>> {
        let selected = selected.iter().cloned().collect::<BTreeSet<_>>();
        let mut remaining = selected.clone();
        let mut batches = Vec::new();
        while !remaining.is_empty() {
            let ready = remaining
                .iter()
                .filter(|root| {
                    self.dependencies.get(*root).is_none_or(|dependencies| {
                        dependencies.iter().all(|dependency| {
                            !selected.contains(dependency) || !remaining.contains(dependency)
                        })
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            if ready.is_empty() {
                batches.push(remaining.into_iter().collect());
                break;
            }
            for root in &ready {
                remaining.remove(root);
            }
            batches.push(ready);
        }
        batches
    }

    pub fn ensure_lock(&self) -> io::Result<()> {
        self.ensure_lock_mode(false)
    }

    pub fn ensure_lock_mode(&self, locked: bool) -> io::Result<()> {
        crate::package::prepare_workspace(&self.root, &self.members, locked, None)
            .map(|_| ())
            .map_err(io::Error::other)
    }
}

pub fn find_workspace_root(path: &Path) -> io::Result<Option<PathBuf>> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(&path).to_path_buf()
    };
    for ancestor in start.ancestors() {
        let manifest_path = ancestor.join(CLUE_PROJECT_FILE_NAME);
        if !manifest_path.is_file() {
            continue;
        }
        if manifest::read_workspace(ancestor)?.is_some() {
            return canonicalize(ancestor).map(Some);
        }
    }
    Ok(None)
}

pub fn load_for_path(path: &Path) -> io::Result<Option<Workspace>> {
    find_workspace_root(path)?
        .map(|root| Workspace::load(&root))
        .transpose()
}

pub fn is_workspace_root(root: &Path) -> io::Result<bool> {
    Ok(manifest::read_workspace(root)?.is_some())
}

pub fn new_manifest() -> String {
    "[workspace]\ncrates = []\n".into()
}

pub fn init(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    let root = std::fs::canonicalize(path)?;
    let manifest_path = root.join(CLUE_PROJECT_FILE_NAME);
    if manifest_path.exists() {
        anyhow::bail!("refusing to overwrite `{}`", manifest_path.display());
    }
    std::fs::write(manifest_path, new_manifest())?;
    Ok(())
}

pub fn new(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        anyhow::bail!("destination `{}` already exists", path.display());
    }
    init(path)
}

fn resolve_members(root: &Path, workspace: &WorkspaceManifest) -> io::Result<Vec<PathBuf>> {
    let mut members = Vec::new();
    for spec in &workspace.crates {
        if spec.is_absolute()
            || spec
                .components()
                .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("workspace crate path `{}` must be relative", spec.display()),
            ));
        }
        let matches = expand_spec(root, spec)?;
        if matches.is_empty() {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!(
                    "workspace crate path `{}` matched no directories",
                    spec.display()
                ),
            ));
        }
        for member in matches {
            let member = canonicalize(&member)?;
            if member == root {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "workspace cannot register itself as a crate",
                ));
            }
            if !member.join(CLUE_PROJECT_FILE_NAME).is_file() {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!(
                        "workspace crate `{}` has no `{CLUE_PROJECT_FILE_NAME}`",
                        member.display()
                    ),
                ));
            }
            if members.contains(&member) {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "workspace crate `{}` is registered more than once",
                        member.display()
                    ),
                ));
            }
            members.push(member);
        }
    }
    Ok(members)
}

fn expand_spec(root: &Path, spec: &Path) -> io::Result<Vec<PathBuf>> {
    if !spec
        .components()
        .any(|component| component.as_os_str().to_string_lossy().contains('*'))
    {
        return Ok(vec![root.join(spec)]);
    }
    let mut paths = vec![root.to_path_buf()];
    for component in spec.components() {
        let Component::Normal(component) = component else {
            if matches!(component, Component::CurDir) {
                continue;
            }
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("unsupported workspace crate path `{}`", spec.display()),
            ));
        };
        let pattern = component.to_string_lossy();
        let mut next = Vec::new();
        for path in paths {
            if pattern.contains('*') {
                for entry in fs::read_dir(path)? {
                    let entry = entry?;
                    if entry.file_type()?.is_dir()
                        && wildcard_match(&pattern, &entry.file_name().to_string_lossy())
                    {
                        next.push(entry.path());
                    }
                }
            } else {
                next.push(path.join(component));
            }
        }
        paths = next;
    }
    Ok(paths)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut states = vec![false; value.len() + 1];
    states[0] = true;
    for &character in pattern {
        let mut next = vec![false; value.len() + 1];
        for (index, matched) in states.into_iter().enumerate() {
            if !matched {
                continue;
            }
            if character == b'*' {
                next[index] = true;
                for value_index in index..value.len() {
                    next[value_index + 1] = true;
                }
            } else if value.get(index) == Some(&character) {
                next[index + 1] = true;
            }
        }
        states = next;
    }
    states[value.len()]
}

struct GraphBuilder<'a> {
    workspace_root: &'a Path,
    members: &'a [PathBuf],
    packages: BTreeMap<PathBuf, WorkspacePackage>,
    dependencies: BTreeMap<PathBuf, Vec<PathBuf>>,
    stack: Vec<PathBuf>,
}

impl<'a> GraphBuilder<'a> {
    fn new(workspace_root: &'a Path, members: &'a [PathBuf]) -> Self {
        Self {
            workspace_root,
            members,
            packages: BTreeMap::new(),
            dependencies: BTreeMap::new(),
            stack: Vec::new(),
        }
    }

    fn visit(&mut self, root: &Path, kind: ProjectKind) -> io::Result<()> {
        if self.stack.contains(&root.to_path_buf()) {
            let cycle_start = self.stack.iter().position(|item| item == root).unwrap_or(0);
            let mut cycle = self.stack[cycle_start..].to_vec();
            cycle.push(root.to_path_buf());
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "cyclic package dependency: {}",
                    cycle
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" -> ")
                ),
            ));
        }
        if self.packages.contains_key(root) {
            return Ok(());
        }
        self.stack.push(root.to_path_buf());
        let manifest = manifest::read(root, kind)?;
        let mut dependency_roots = Vec::new();
        for dependency in &manifest.dependencies {
            if dependency.kind == DependencyKind::Development {
                continue;
            }
            validate_dependency_alias(&dependency.alias)?;
            let DependencySource::Path(path) = &dependency.source else {
                continue;
            };
            let dependency_root = canonicalize(&root.join(path))?;
            if dependency_root.starts_with(self.workspace_root)
                && !self.members.iter().any(|member| member == &dependency_root)
            {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "path dependency `{}` at `{}` is not registered in workspace `{}`",
                        dependency.alias,
                        dependency_root.display(),
                        self.workspace_root.display()
                    ),
                ));
            }
            self.visit(&dependency_root, ProjectKind::Library)?;
            validate_dependency_target(
                dependency,
                &self
                    .packages
                    .get(&dependency_root)
                    .expect("visited dependency has package metadata")
                    .manifest,
            )?;
            dependency_roots.push(dependency_root);
        }
        self.stack.pop();
        self.dependencies
            .insert(root.to_path_buf(), dependency_roots);
        self.packages.insert(
            root.to_path_buf(),
            WorkspacePackage {
                root: root.to_path_buf(),
                manifest,
            },
        );
        Ok(())
    }
}

fn validate_unique_names(packages: &BTreeMap<PathBuf, WorkspacePackage>) -> io::Result<()> {
    let mut names = BTreeMap::<&str, &Path>::new();
    for package in packages.values() {
        if let Some(previous) = names.insert(&package.manifest.name, &package.root) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "package name `{}` is used by both `{}` and `{}`",
                    package.manifest.name,
                    previous.display(),
                    package.root.display()
                ),
            ));
        }
    }
    Ok(())
}

fn validate_dependency_target(dependency: &DependencySpec, manifest: &Manifest) -> io::Result<()> {
    if manifest.name != dependency.package {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "dependency `{}` expected package `{}`, found `{}`",
                dependency.alias, dependency.package, manifest.name
            ),
        ));
    }
    if let Some(requirement) = &dependency.requirement
        && !requirement.matches(&manifest.version)
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "dependency `{}` requires version `{requirement}`, found `{}`",
                dependency.alias, manifest.version
            ),
        ));
    }
    Ok(())
}

fn visit_order(
    root: &Path,
    members: &BTreeSet<PathBuf>,
    dependencies: &BTreeMap<PathBuf, Vec<PathBuf>>,
    visiting: &mut HashSet<PathBuf>,
    visited: &mut HashSet<PathBuf>,
    ordered: &mut Vec<PathBuf>,
) {
    if visited.contains(root) || !visiting.insert(root.to_path_buf()) {
        return;
    }
    if let Some(package_dependencies) = dependencies.get(root) {
        for dependency in package_dependencies {
            if members.contains(dependency) {
                visit_order(
                    dependency,
                    members,
                    dependencies,
                    visiting,
                    visited,
                    ordered,
                );
            }
        }
    }
    visiting.remove(root);
    visited.insert(root.to_path_buf());
    ordered.push(root.to_path_buf());
}

fn validate_dependency_alias(alias: &str) -> io::Result<()> {
    let mut chars = alias.chars();
    let Some(first) = chars.next() else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "dependency name must not be empty",
        ));
    };
    if (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::InvalidData,
            format!("dependency name `{alias}` must be a valid module name"),
        ))
    }
}

fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path)
}

#[cfg(test)]
fn lock_path(workspace_root: &Path, package_root: &Path) -> String {
    if let Ok(path) = package_root.strip_prefix(workspace_root) {
        if path.as_os_str().is_empty() {
            return ".".into();
        }
        return path.to_string_lossy().replace('\\', "/");
    }
    let workspace = workspace_root.components().collect::<Vec<_>>();
    let package = package_root.components().collect::<Vec<_>>();
    let common = workspace
        .iter()
        .zip(&package)
        .take_while(|(left, right)| left.as_os_str() == right.as_os_str())
        .count();
    if common == 0 {
        return package_root.to_string_lossy().replace('\\', "/");
    }
    let mut parts = vec!["..".to_owned(); workspace.len() - common];
    parts.extend(
        package[common..]
            .iter()
            .map(|component| component.as_os_str().to_string_lossy().into_owned()),
    );
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::lock_path;

    #[test]
    fn lock_paths_are_relative_for_sibling_packages() {
        let parent = std::env::temp_dir().join("clue-workspace-path-test");
        assert_eq!(
            lock_path(&parent.join("root"), &parent.join("dependency")),
            "../dependency"
        );
    }
}
