use semver::VersionReq;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySource {
    Path(PathBuf),
    Git {
        url: String,
        reference: GitReference,
    },
    Registry {
        registry: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitReference {
    DefaultBranch,
    Branch(String),
    Tag(String),
    Rev(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    Normal,
    Development,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetKind {
    Library,
    ProcMacro,
    Binary,
    Test,
    Example,
    Bench,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LibraryType {
    RiddleLib,
    StaticLib,
    Cdylib,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub path: PathBuf,
    pub kind: TargetKind,
    pub required_features: Vec<String>,
    pub library_types: Vec<LibraryType>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencySpec {
    pub alias: String,
    pub package: String,
    pub source: DependencySource,
    pub requirement: Option<VersionReq>,
    pub optional: bool,
    pub features: Vec<String>,
    pub default_features: bool,
    pub kind: DependencyKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureResolution {
    pub active: BTreeSet<String>,
    pub dependency_features: BTreeMap<String, BTreeSet<String>>,
}

pub fn resolve_features(
    definitions: &BTreeMap<String, Vec<String>>,
    dependencies: &[DependencySpec],
    requested: &[String],
    default_features: bool,
) -> Result<FeatureResolution, String> {
    let dependency_names = dependencies
        .iter()
        .map(|dependency| dependency.alias.as_str())
        .collect::<BTreeSet<_>>();
    let mut queue = requested.to_vec();
    if default_features && definitions.contains_key("default") {
        queue.push("default".into());
    }
    let mut resolution = FeatureResolution::default();
    let mut visited = BTreeSet::new();
    let mut conditional = Vec::new();
    while let Some(feature) = queue.pop() {
        if feature.is_empty() {
            return Err("feature name cannot be empty".into());
        }
        if let Some((dependency, forwarded)) = feature.split_once('/') {
            let conditional_dependency = dependency.strip_suffix('?');
            let dependency = conditional_dependency.unwrap_or(dependency);
            if !dependency_names.contains(dependency) || forwarded.is_empty() {
                return Err(format!("unknown dependency feature `{feature}`"));
            }
            if conditional_dependency.is_some() {
                conditional.push((dependency.to_owned(), forwarded.to_owned()));
            } else {
                resolution.active.insert(dependency.to_owned());
                resolution
                    .dependency_features
                    .entry(dependency.to_owned())
                    .or_default()
                    .insert(forwarded.to_owned());
            }
            continue;
        }
        let dependency = feature.strip_prefix("dep:").unwrap_or(&feature);
        if dependency_names.contains(dependency) {
            resolution.active.insert(dependency.to_owned());
            continue;
        }
        if !visited.insert(feature.clone()) {
            continue;
        }
        let values = definitions
            .get(&feature)
            .ok_or_else(|| format!("unknown feature `{feature}`"))?;
        resolution.active.insert(feature);
        queue.extend(values.iter().cloned());
    }
    for (dependency, forwarded) in conditional {
        if resolution.active.contains(&dependency) {
            resolution
                .dependency_features
                .entry(dependency)
                .or_default()
                .insert(forwarded);
        }
    }
    Ok(resolution)
}
