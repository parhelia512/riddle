use crate::ProjectKind;
use crate::lock::{self, LockFile, LockPackage};
use crate::manifest;
use crate::model::{DependencyKind, DependencySource, DependencySpec, GitReference};
use anyhow::{Context, bail};
use flate2::read::GzDecoder;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::Command;
use toml_edit::{DocumentMut, InlineTable, Item, Value as EditValue, value};

const DEFAULT_REGISTRY: &str = "default";
const DEFAULT_INDEX: &str = "https://registry.riddle-lang.org/index";
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;

#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) home: PathBuf,
    pub(crate) offline: bool,
    pub(crate) jobs: usize,
    default_registry: String,
    registries: BTreeMap<String, RegistryConfig>,
}

#[derive(Debug, Clone)]
struct RegistryConfig {
    index: String,
    api: Option<String>,
    token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    net: Option<NetConfig>,
    build: Option<BuildConfig>,
    registry: Option<DefaultRegistryConfig>,
    registries: Option<BTreeMap<String, RegistryFileConfig>>,
}

#[derive(Debug, Deserialize)]
struct NetConfig {
    offline: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct BuildConfig {
    jobs: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct DefaultRegistryConfig {
    default: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegistryFileConfig {
    index: Option<String>,
    api: Option<String>,
    token: Option<String>,
}

impl Config {
    pub(crate) fn load(project_root: &Path) -> anyhow::Result<Self> {
        let home = clue_home()?;
        let mut config = Self {
            home: home.clone(),
            offline: false,
            jobs: std::thread::available_parallelism().map_or(1, usize::from),
            default_registry: DEFAULT_REGISTRY.into(),
            registries: BTreeMap::from([(
                DEFAULT_REGISTRY.into(),
                RegistryConfig {
                    index: DEFAULT_INDEX.into(),
                    api: None,
                    token: None,
                },
            )]),
        };
        config.merge_file(&home.join("config.toml"))?;
        config.merge_file(&project_root.join(".clue").join("config.toml"))?;
        if let Some(value) = env_bool("CLUE_OFFLINE")? {
            config.offline = value;
        }
        if let Ok(value) = std::env::var("CLUE_JOBS") {
            config.jobs = value
                .parse::<usize>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| anyhow::anyhow!("CLUE_JOBS must be a positive integer"))?;
        }
        if let Ok(index) = std::env::var("CLUE_REGISTRY_INDEX") {
            let fallback = index.clone();
            config
                .registries
                .entry(DEFAULT_REGISTRY.into())
                .or_insert_with(|| RegistryConfig {
                    index: fallback,
                    api: None,
                    token: None,
                })
                .index = index;
        }
        if let Ok(token) = std::env::var("CLUE_REGISTRY_TOKEN") {
            config
                .registries
                .entry(config.default_registry.clone())
                .or_insert_with(|| RegistryConfig {
                    index: DEFAULT_INDEX.into(),
                    api: None,
                    token: None,
                })
                .token = Some(token);
        }
        Ok(config)
    }

    fn merge_file(&mut self, path: &Path) -> anyhow::Result<()> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let value: ConfigFile = toml::from_str(&text)
            .with_context(|| format!("invalid config `{}`", path.display()))?;
        if let Some(offline) = value.net.and_then(|net| net.offline) {
            self.offline = offline;
        }
        if let Some(jobs) = value.build.and_then(|build| build.jobs) {
            if jobs == 0 {
                bail!("`build.jobs` in `{}` must be positive", path.display());
            }
            self.jobs = jobs;
        }
        if let Some(default) = value.registry.and_then(|registry| registry.default) {
            self.default_registry = default;
        }
        for (name, registry) in value.registries.unwrap_or_default() {
            let entry = self
                .registries
                .entry(name)
                .or_insert_with(|| RegistryConfig {
                    index: DEFAULT_INDEX.into(),
                    api: None,
                    token: None,
                });
            if let Some(index) = registry.index {
                entry.index = index;
            }
            if registry.api.is_some() {
                entry.api = registry.api;
            }
            if registry.token.is_some() {
                entry.token = registry.token;
            }
        }
        Ok(())
    }

    fn registry(&self, name: Option<&str>) -> anyhow::Result<(&str, &RegistryConfig)> {
        let name = name.unwrap_or(&self.default_registry);
        self.registries
            .get_key_value(name)
            .map(|(name, config)| (name.as_str(), config))
            .ok_or_else(|| anyhow::anyhow!("registry `{name}` is not configured"))
    }
}

fn clue_home() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLUE_HOME") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| anyhow::anyhow!("cannot determine Clue home; set CLUE_HOME"))?;
    Ok(PathBuf::from(home).join(".clue"))
}

pub(crate) fn configured_jobs(root: &Path) -> anyhow::Result<usize> {
    Ok(Config::load(root)?.jobs)
}

fn env_bool(name: &str) -> anyhow::Result<Option<bool>> {
    let Ok(value) = std::env::var(name) else {
        return Ok(None);
    };
    match value.as_str() {
        "1" | "true" | "yes" => Ok(Some(true)),
        "0" | "false" | "no" => Ok(Some(false)),
        _ => bail!("{name} must be true or false"),
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RegistryVersion {
    name: String,
    #[serde(rename = "vers")]
    version: Version,
    #[serde(default)]
    deps: Vec<RegistryDependency>,
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    cksum: String,
    #[serde(default)]
    yanked: bool,
    archive: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RegistryDependency {
    name: String,
    req: VersionReq,
    package: Option<String>,
    registry: Option<String>,
    #[serde(default)]
    optional: bool,
    #[serde(default = "yes")]
    default_features: bool,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    kind: RegistryDependencyKind,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum RegistryDependencyKind {
    #[default]
    Normal,
    Dev,
}

const fn yes() -> bool {
    true
}

#[derive(Clone)]
struct Request {
    alias: String,
    package: String,
    source: SourceRequest,
    requirement: VersionReq,
    features: Vec<String>,
    default_features: bool,
}

#[derive(Clone)]
enum SourceRequest {
    Path(PathBuf),
    Git {
        url: String,
        reference: GitReference,
    },
    Registry {
        name: String,
        index: String,
    },
}

#[derive(Clone)]
struct Candidate {
    key: String,
    name: String,
    version: Version,
    source: String,
    root: Option<PathBuf>,
    checksum: String,
    archive: Option<String>,
    dependencies: Vec<DependencySpec>,
    features: BTreeMap<String, Vec<String>>,
    source_hash: String,
}

#[derive(Clone, Default)]
struct SolveState {
    selected: BTreeMap<String, Candidate>,
    requirements: BTreeMap<String, Vec<VersionReq>>,
    features: BTreeMap<String, BTreeSet<String>>,
    expanded: BTreeMap<String, BTreeSet<String>>,
}

struct Resolver {
    root: PathBuf,
    config: Config,
    include_dev: bool,
    locked: Option<LockFile>,
    registry_cache: BTreeMap<(String, String), Vec<RegistryVersion>>,
}

pub(crate) fn prepare(
    root: &Path,
    locked: bool,
    offline: Option<bool>,
    include_dev: bool,
    features: &[String],
) -> anyhow::Result<LockFile> {
    prepare_mode(root, locked, offline, include_dev, features, false)
}

pub(crate) fn update(root: &Path, offline: Option<bool>) -> anyhow::Result<LockFile> {
    prepare_mode(root, false, offline, false, &[], true)
}

fn prepare_mode(
    root: &Path,
    locked: bool,
    offline: Option<bool>,
    include_dev: bool,
    features: &[String],
    update: bool,
) -> anyhow::Result<LockFile> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("failed to resolve `{}`", root.display()))?;
    let lock_path = lock_path(&root)?;
    let current = lock::read(&lock_path)?;
    let mut config = Config::load(&root)?;
    if let Some(offline) = offline {
        config.offline = offline;
    }
    let mut resolver = Resolver {
        root: root.clone(),
        config,
        include_dev,
        locked: (!update).then(|| current.clone()).flatten(),
        registry_cache: BTreeMap::new(),
    };
    let expected = resolver.solve(features)?;
    if locked {
        let current = current.ok_or_else(|| {
            anyhow::anyhow!(
                "missing `{}`; run without --locked once",
                lock_path.display()
            )
        })?;
        if current != expected {
            bail!(
                "`{}` is out of date; run without --locked to update it",
                lock_path.display()
            );
        }
    } else {
        lock::write_if_changed(&lock_path, &expected)?;
    }
    Ok(expected)
}

pub(crate) fn prepare_workspace(
    root: &Path,
    members: &[PathBuf],
    locked: bool,
    offline: Option<bool>,
) -> anyhow::Result<LockFile> {
    prepare_workspace_mode(
        root,
        members,
        locked,
        offline,
        false,
        false,
        BTreeMap::new(),
    )
}

pub(crate) fn prepare_workspace_target(
    root: &Path,
    members: &[PathBuf],
    locked: bool,
    offline: Option<bool>,
    include_dev: bool,
    feature_package: Option<&Path>,
    features: &[String],
) -> anyhow::Result<LockFile> {
    let mut selections = BTreeMap::new();
    if let Some(package) = feature_package {
        selections.insert(package.to_path_buf(), features.to_vec());
    }
    prepare_workspace_mode(
        root,
        members,
        locked,
        offline,
        false,
        include_dev,
        selections,
    )
}

pub(crate) fn prepare_workspace_targets(
    root: &Path,
    members: &[PathBuf],
    locked: bool,
    offline: Option<bool>,
    include_dev: bool,
    selections: &[(PathBuf, Vec<String>)],
) -> anyhow::Result<LockFile> {
    prepare_workspace_mode(
        root,
        members,
        locked,
        offline,
        false,
        include_dev,
        selections.iter().cloned().collect(),
    )
}

pub(crate) fn update_workspace(
    root: &Path,
    members: &[PathBuf],
    offline: Option<bool>,
) -> anyhow::Result<LockFile> {
    prepare_workspace_mode(root, members, false, offline, true, false, BTreeMap::new())
}

fn prepare_workspace_mode(
    root: &Path,
    members: &[PathBuf],
    locked: bool,
    offline: Option<bool>,
    update: bool,
    include_dev: bool,
    feature_selections: BTreeMap<PathBuf, Vec<String>>,
) -> anyhow::Result<LockFile> {
    let root = fs::canonicalize(root)?;
    let path = root.join("Clue.lock");
    let current = lock::read(&path)?;
    let feature_selections = feature_selections
        .into_iter()
        .map(|(package, features)| Ok((fs::canonicalize(package)?, features)))
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let mut packages = BTreeMap::<(String, String), LockPackage>::new();
    for member in members {
        let member = fs::canonicalize(member)?;
        let requested_features = feature_selections
            .get(&member)
            .map_or(&[] as &[String], Vec::as_slice);
        let mut config = Config::load(&root)?;
        if let Some(offline) = offline {
            config.offline = offline;
        }
        let mut resolver = Resolver {
            root: member,
            config,
            include_dev,
            locked: (!update).then(|| current.clone()).flatten(),
            registry_cache: BTreeMap::new(),
        };
        for mut package in resolver.solve(requested_features)?.package {
            if let Some(path) = package.source.strip_prefix("path+") {
                package.path = display_path(&root, Path::new(path));
            }
            let key = (package.source.clone(), package.name.clone());
            if let Some(existing) = packages.get(&key)
                && existing.version != package.version
            {
                bail!(
                    "workspace requires incompatible versions of `{}` from `{}`: {} and {}",
                    package.name,
                    package.source,
                    existing.version,
                    package.version
                );
            }
            if let Some(existing) = packages.get_mut(&key) {
                existing.dependencies.extend(package.dependencies);
                existing.dependencies.sort();
                existing.dependencies.dedup();
                existing.features.extend(package.features);
                existing.features.sort();
                existing.features.dedup();
            } else {
                packages.insert(key, package);
            }
        }
    }
    let expected = LockFile {
        version: lock::LOCK_VERSION,
        package: packages.into_values().collect(),
    };
    if locked {
        let current = current.ok_or_else(|| {
            anyhow::anyhow!("missing `{}`; run without --locked once", path.display())
        })?;
        if current != expected {
            bail!(
                "`{}` is out of date; run without --locked to update it",
                path.display()
            );
        }
    } else {
        lock::write_if_changed(&path, &expected)?;
    }
    Ok(expected)
}

fn lock_path(root: &Path) -> anyhow::Result<PathBuf> {
    if let Some(workspace) = crate::workspace::find_workspace_root(root)? {
        Ok(workspace.join("Clue.lock"))
    } else {
        Ok(root.join("Clue.lock"))
    }
}

impl Resolver {
    fn solve(&mut self, requested_features: &[String]) -> anyhow::Result<LockFile> {
        let root_manifest = manifest::read(&self.root, ProjectKind::Binary)?;
        let root_key = format!("path+{}", self.root.display());
        let root_features = enabled_features(
            &root_manifest.features,
            &root_manifest.dependencies,
            requested_features,
            true,
        )?;
        let root_candidate = Candidate {
            key: root_key.clone(),
            name: root_manifest.name.clone(),
            version: root_manifest.version.clone(),
            source: root_key.clone(),
            root: Some(self.root.clone()),
            checksum: String::new(),
            archive: None,
            dependencies: root_manifest.dependencies.clone(),
            features: root_manifest.features.clone(),
            source_hash: root_manifest.source_hash.clone(),
        };
        let mut state = SolveState::default();
        state
            .selected
            .insert(root_key.clone(), root_candidate.clone());
        state
            .features
            .insert(root_key.clone(), root_features.clone());
        state.expanded.insert(root_key, root_features.clone());
        let pending = self.requests_for(&root_candidate, &root_features)?;
        let state = self.solve_pending(pending, state)?;
        self.materialize(&state)?;
        self.lock_file(state)
    }

    fn solve_pending(
        &mut self,
        mut pending: Vec<Request>,
        mut state: SolveState,
    ) -> anyhow::Result<SolveState> {
        let Some(request) = pending.pop() else {
            return Ok(state);
        };
        let candidates = self.candidates(&request)?;
        if candidates.len() == 1 && !request.requirement.matches(&candidates[0].version) {
            bail!(
                "dependency `{}` requires version `{}`, found `{}`",
                request.alias,
                requirement_label(&request.requirement),
                candidates[0].version
            );
        }
        let key = candidates
            .first()
            .map(|candidate| candidate.key.clone())
            .ok_or_else(|| anyhow::anyhow!("no versions found for `{}`", request.package))?;
        state
            .requirements
            .entry(key.clone())
            .or_default()
            .push(request.requirement.clone());
        let requirements = state.requirements[&key].clone();

        if let Some(selected) = state.selected.get(&key).cloned() {
            if !requirements
                .iter()
                .all(|req| req.matches(&selected.version))
            {
                bail!(
                    "version conflict for `{}`: selected {}, required {}",
                    request.package,
                    selected.version,
                    requirements
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            let changed = merge_features(
                state.features.entry(key.clone()).or_default(),
                &selected.features,
                &selected.dependencies,
                &request.features,
                request.default_features,
            )?;
            if changed || !state.expanded.contains_key(&key) {
                let active = state.features[&key].clone();
                if state.expanded.get(&key) != Some(&active) {
                    state.expanded.insert(key, active.clone());
                    pending.extend(self.requests_for(&selected, &active)?);
                }
            }
            return self.solve_pending(pending, state);
        }

        let mut errors = Vec::new();
        for candidate in candidates.into_iter().filter(|candidate| {
            requirements
                .iter()
                .all(|requirement| requirement.matches(&candidate.version))
        }) {
            let mut branch = state.clone();
            let mut active = BTreeSet::new();
            merge_features(
                &mut active,
                &candidate.features,
                &candidate.dependencies,
                &request.features,
                request.default_features,
            )?;
            branch
                .features
                .insert(candidate.key.clone(), active.clone());
            branch
                .expanded
                .insert(candidate.key.clone(), active.clone());
            branch
                .selected
                .insert(candidate.key.clone(), candidate.clone());
            let mut branch_pending = pending.clone();
            branch_pending.extend(self.requests_for(&candidate, &active)?);
            match self.solve_pending(branch_pending, branch) {
                Ok(state) => return Ok(state),
                Err(error) => errors.push(error.to_string()),
            }
        }
        bail!(
            "failed to select a version for `{}` ({}){}",
            request.alias,
            request.requirement,
            errors
                .last()
                .map(|error| format!(": {error}"))
                .unwrap_or_default()
        )
    }

    fn candidates(&mut self, request: &Request) -> anyhow::Result<Vec<Candidate>> {
        match &request.source {
            SourceRequest::Path(root) => Ok(vec![self.local_candidate(root, None)?]),
            SourceRequest::Git { url, reference } => {
                let prefix = format!("git+{url}#");
                let locked_revision = self.locked.as_ref().and_then(|lock| {
                    lock.package
                        .iter()
                        .find(|package| {
                            package.name == request.package && package.source.starts_with(&prefix)
                        })
                        .and_then(|package| package.source.strip_prefix(&prefix))
                });
                let (root, revision) = self.git_checkout(url, reference, locked_revision)?;
                Ok(vec![self.local_candidate(
                    &root,
                    Some(format!("git+{url}#{revision}")),
                )?])
            }
            SourceRequest::Registry { name, index } => {
                let source = format!("registry+{index}");
                let locked_version = self.locked.as_ref().and_then(|lock| {
                    lock.package
                        .iter()
                        .find(|package| package.name == request.package && package.source == source)
                        .and_then(|package| Version::parse(&package.version).ok())
                });
                let versions = self.registry_versions(name, index, &request.package)?;
                let mut candidates = versions
                    .into_iter()
                    .filter(|version| {
                        !version.yanked || locked_version.as_ref() == Some(&version.version)
                    })
                    .map(|version| self.registry_candidate(name, index, version))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                candidates.sort_by(|left, right| {
                    let left_locked = locked_version.as_ref() == Some(&left.version);
                    let right_locked = locked_version.as_ref() == Some(&right.version);
                    right_locked
                        .cmp(&left_locked)
                        .then_with(|| right.version.cmp(&left.version))
                });
                Ok(candidates)
            }
        }
    }

    fn local_candidate(
        &self,
        root: &Path,
        source_override: Option<String>,
    ) -> anyhow::Result<Candidate> {
        let root = fs::canonicalize(root)
            .with_context(|| format!("failed to resolve dependency `{}`", root.display()))?;
        let manifest = manifest::read(&root, ProjectKind::Library)?;
        let source = source_override.unwrap_or_else(|| format!("path+{}", root.display()));
        Ok(Candidate {
            key: format!("{source}|{}", manifest.name),
            name: manifest.name,
            version: manifest.version,
            source,
            root: Some(root),
            checksum: String::new(),
            archive: None,
            dependencies: manifest.dependencies,
            features: manifest.features,
            source_hash: manifest.source_hash,
        })
    }

    fn registry_candidate(
        &self,
        registry: &str,
        index: &str,
        version: RegistryVersion,
    ) -> anyhow::Result<Candidate> {
        let dependencies = version
            .deps
            .into_iter()
            .map(|dependency| DependencySpec {
                alias: dependency.name.clone(),
                package: dependency.package.unwrap_or(dependency.name),
                source: DependencySource::Registry {
                    registry: dependency.registry,
                },
                requirement: Some(dependency.req),
                optional: dependency.optional,
                features: dependency.features,
                default_features: dependency.default_features,
                kind: match dependency.kind {
                    RegistryDependencyKind::Normal => DependencyKind::Normal,
                    RegistryDependencyKind::Dev => DependencyKind::Development,
                },
            })
            .collect::<Vec<_>>();
        let mut features = version.features;
        for dependency in dependencies.iter().filter(|dependency| dependency.optional) {
            features.entry(dependency.alias.clone()).or_default();
        }
        let source = format!("registry+{index}");
        Ok(Candidate {
            key: format!("{source}|{}", version.name),
            name: version.name,
            version: version.version,
            source,
            root: None,
            checksum: version.cksum,
            archive: version.archive.or_else(|| {
                self.config
                    .registries
                    .get(registry)
                    .and_then(|config| config.api.as_ref())
                    .map(|api| api.trim_end_matches('/').to_owned())
            }),
            dependencies,
            features,
            source_hash: String::new(),
        })
    }

    fn requests_for(
        &self,
        candidate: &Candidate,
        active_features: &BTreeSet<String>,
    ) -> anyhow::Result<Vec<Request>> {
        let base = candidate.root.as_deref().unwrap_or(&self.root);
        let requested = active_features.iter().cloned().collect::<Vec<_>>();
        let resolution = crate::model::resolve_features(
            &candidate.features,
            &candidate.dependencies,
            &requested,
            false,
        )
        .map_err(anyhow::Error::msg)?;
        candidate
            .dependencies
            .iter()
            .filter(|dependency| {
                (self.include_dev || dependency.kind == DependencyKind::Normal)
                    && (!dependency.optional || resolution.active.contains(&dependency.alias))
            })
            .map(|dependency| {
                self.request(
                    base,
                    dependency,
                    resolution.dependency_features.get(&dependency.alias),
                )
            })
            .collect()
    }

    fn request(
        &self,
        base: &Path,
        dependency: &DependencySpec,
        forwarded_features: Option<&BTreeSet<String>>,
    ) -> anyhow::Result<Request> {
        let source = match &dependency.source {
            DependencySource::Path(path) => SourceRequest::Path(base.join(path)),
            DependencySource::Git { url, reference } => SourceRequest::Git {
                url: url.clone(),
                reference: reference.clone(),
            },
            DependencySource::Registry { registry } => {
                let (name, config) = self.config.registry(registry.as_deref())?;
                SourceRequest::Registry {
                    name: name.into(),
                    index: config.index.clone(),
                }
            }
        };
        let mut features = dependency.features.clone();
        if let Some(forwarded_features) = forwarded_features {
            features.extend(forwarded_features.iter().cloned());
            features.sort();
            features.dedup();
        }
        Ok(Request {
            alias: dependency.alias.clone(),
            package: dependency.package.clone(),
            source,
            requirement: dependency.requirement.clone().unwrap_or(VersionReq::STAR),
            features,
            default_features: dependency.default_features,
        })
    }

    fn registry_versions(
        &mut self,
        registry: &str,
        index: &str,
        package: &str,
    ) -> anyhow::Result<Vec<RegistryVersion>> {
        let key = (index.to_owned(), package.to_owned());
        if let Some(versions) = self.registry_cache.get(&key) {
            return Ok(versions.clone());
        }
        let cache = self
            .config
            .home
            .join("registry/index")
            .join(short_hash(index))
            .join(sparse_path(package));
        let text = if self.config.offline {
            fs::read_to_string(&cache).with_context(|| {
                format!("registry package `{package}` is not cached for offline use")
            })?
        } else {
            #[cfg(target_arch = "wasm32")]
            {
                bail!("network package resolution is unavailable in wasm builds");
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let (_, registry_config) = self.config.registry(Some(registry))?;
                let url = format!(
                    "{}/{}",
                    index.trim_end_matches('/'),
                    sparse_path(package)
                        .display()
                        .to_string()
                        .replace('\\', "/")
                );
                let mut request = reqwest::blocking::Client::builder()
                    .user_agent(format!("clue/{}", env!("CARGO_PKG_VERSION")))
                    .build()?
                    .get(&url);
                if let Some(token) = &registry_config.token {
                    request = request.bearer_auth(token);
                }
                let response = request.send()?.error_for_status()?;
                let text = response.text()?;
                if let Some(parent) = cache.parent() {
                    fs::create_dir_all(parent)?;
                }
                atomic_write(&cache, text.as_bytes())?;
                text
            }
        };
        let versions = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).context("invalid sparse registry entry"))
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.registry_cache.insert(key, versions.clone());
        Ok(versions)
    }

    fn git_checkout(
        &self,
        url: &str,
        reference: &GitReference,
        locked_revision: Option<&str>,
    ) -> anyhow::Result<(PathBuf, String)> {
        if let Some(revision) = locked_revision {
            let checkout = self
                .config
                .home
                .join("git/checkouts")
                .join(short_hash(url))
                .join(revision);
            if checkout.join(manifest::CLUE_PROJECT_FILE_NAME).is_file() {
                return Ok((checkout, revision.to_owned()));
            }
        }
        let repo = self.config.home.join("git/db").join(short_hash(url));
        if !repo.join("HEAD").is_file() {
            if self.config.offline {
                bail!("git dependency `{url}` is not cached for offline use");
            }
            if let Some(parent) = repo.parent() {
                fs::create_dir_all(parent)?;
            }
            run_git(
                None,
                [
                    OsStr::new("clone"),
                    OsStr::new("--mirror"),
                    OsStr::new(url),
                    repo.as_os_str(),
                ],
            )?;
        } else if !self.config.offline {
            run_git(Some(&repo), [OsStr::new("fetch"), OsStr::new("--prune")])?;
        }
        let revision = if let Some(revision) = locked_revision {
            revision.to_owned()
        } else {
            let reference = match reference {
                GitReference::DefaultBranch => "HEAD".to_owned(),
                GitReference::Branch(branch) => format!("refs/heads/{branch}"),
                GitReference::Tag(tag) => format!("refs/tags/{tag}"),
                GitReference::Rev(revision) => revision.clone(),
            };
            git_output(&repo, [OsStr::new("rev-parse"), OsStr::new(&reference)])?
                .trim()
                .to_owned()
        };
        let checkout = self
            .config
            .home
            .join("git/checkouts")
            .join(short_hash(url))
            .join(&revision);
        if !checkout.join(manifest::CLUE_PROJECT_FILE_NAME).is_file() {
            if let Some(parent) = checkout.parent() {
                fs::create_dir_all(parent)?;
            }
            run_git(
                None,
                [
                    OsStr::new("clone"),
                    OsStr::new("--no-checkout"),
                    repo.as_os_str(),
                    checkout.as_os_str(),
                ],
            )?;
            run_git(
                Some(&checkout),
                [
                    OsStr::new("checkout"),
                    OsStr::new("--detach"),
                    OsStr::new(&revision),
                ],
            )?;
        }
        Ok((checkout, revision))
    }

    fn materialize(&self, state: &SolveState) -> anyhow::Result<()> {
        for candidate in state.selected.values() {
            if candidate.source.starts_with("registry+") {
                self.materialize_registry(candidate)?;
            }
        }
        Ok(())
    }

    fn materialize_registry(&self, candidate: &Candidate) -> anyhow::Result<()> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = candidate;
            bail!("network package downloads are unavailable in wasm builds");
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let index = candidate.source.trim_start_matches("registry+");
            let destination = registry_source_dir(
                &self.config.home,
                index,
                &candidate.name,
                &candidate.version,
            );
            if let Some(cached) = cached_package_root(&destination)
                && manifest::read(&cached, ProjectKind::Binary).is_ok()
            {
                return Ok(());
            }
            if destination.exists() {
                remove_cache_entry(&destination)?;
            }
            if self.config.offline {
                bail!(
                    "package `{} {}` is not cached for offline use",
                    candidate.name,
                    candidate.version
                );
            }
            let archive_url = candidate.archive.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "registry entry for `{} {}` has no archive URL and registry has no API URL",
                    candidate.name,
                    candidate.version
                )
            })?;
            let archive_url = if archive_url.ends_with("/download")
                || archive_url.ends_with(".gz")
                || archive_url.ends_with(".cluepkg")
            {
                archive_url.to_owned()
            } else {
                format!(
                    "{archive_url}/v1/crates/{}/{}/download",
                    candidate.name, candidate.version
                )
            };
            let bytes = {
                #[cfg(target_arch = "wasm32")]
                {
                    bail!("network package downloads are unavailable in wasm builds");
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let response = reqwest::blocking::Client::builder()
                        .user_agent(format!("clue/{}", env!("CARGO_PKG_VERSION")))
                        .build()?
                        .get(archive_url)
                        .send()?
                        .error_for_status()?;
                    if response
                        .content_length()
                        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
                    {
                        bail!(
                            "package archive exceeds the {} MiB limit",
                            MAX_ARCHIVE_BYTES / 1024 / 1024
                        );
                    }
                    response.bytes()?
                }
            };
            if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
                bail!(
                    "package archive exceeds the {} MiB limit",
                    MAX_ARCHIVE_BYTES / 1024 / 1024
                );
            }
            let actual = sha256(&bytes);
            if !candidate.checksum.is_empty() && actual != candidate.checksum {
                bail!(
                    "checksum mismatch for `{} {}`: expected {}, got {actual}",
                    candidate.name,
                    candidate.version,
                    candidate.checksum
                );
            }
            let temp = destination.with_extension(format!("tmp-{}", std::process::id()));
            if temp.exists() {
                remove_cache_entry(&temp)?;
            }
            fs::create_dir_all(&temp)?;
            let result = (|| {
                let mut archive = tar::Archive::new(GzDecoder::new(bytes.as_ref()));
                let mut unpacked = 0_u64;
                let mut entries = 0_usize;
                for entry in archive.entries()? {
                    let mut entry = entry?;
                    entries += 1;
                    if entries > MAX_ARCHIVE_ENTRIES {
                        bail!("package archive has too many entries");
                    }
                    let entry_type = entry.header().entry_type();
                    if entry_type.is_symlink() || entry_type.is_hard_link() {
                        bail!("package archive contains a link");
                    }
                    if !entry_type.is_file() && !entry_type.is_dir() {
                        bail!("package archive contains an unsupported entry type");
                    }
                    unpacked = unpacked.saturating_add(entry.size());
                    if unpacked > MAX_UNPACKED_BYTES {
                        bail!(
                            "package archive exceeds the {} MiB unpacked limit",
                            MAX_UNPACKED_BYTES / 1024 / 1024
                        );
                    }
                    if !entry.unpack_in(&temp)? {
                        bail!("package archive contains an unsafe path");
                    }
                }
                let cached = cached_package_root(&temp).ok_or_else(|| {
                    anyhow::anyhow!(
                        "package archive does not contain `{}`",
                        manifest::CLUE_PROJECT_FILE_NAME
                    )
                })?;
                manifest::read(&cached, ProjectKind::Binary)
                    .context("package archive has an invalid manifest")?;
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(&temp, &destination)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = remove_cache_entry(&temp);
            }
            result
        }
    }

    fn lock_file(&self, state: SolveState) -> anyhow::Result<LockFile> {
        let mut package = Vec::new();
        for (key, candidate) in &state.selected {
            let active = state.features.get(key).cloned().unwrap_or_default();
            let mut dependencies = self
                .requests_for(candidate, &active)?
                .into_iter()
                .map(|request| request.package)
                .collect::<Vec<_>>();
            dependencies.sort();
            dependencies.dedup();
            package.push(LockPackage {
                name: candidate.name.clone(),
                version: candidate.version.to_string(),
                source: candidate.source.clone(),
                path: candidate
                    .root
                    .as_deref()
                    .map(|root| display_path(&self.root, root))
                    .unwrap_or_default(),
                dependencies,
                source_hash: candidate.source_hash.clone(),
                checksum: candidate.checksum.clone(),
                features: active.into_iter().collect(),
            });
        }
        package.sort_by(|left, right| {
            (&left.name, &left.version, &left.source).cmp(&(
                &right.name,
                &right.version,
                &right.source,
            ))
        });
        Ok(LockFile {
            version: lock::LOCK_VERSION,
            package,
        })
    }
}

fn merge_features(
    active: &mut BTreeSet<String>,
    definitions: &BTreeMap<String, Vec<String>>,
    dependencies: &[DependencySpec],
    requested: &[String],
    default_features: bool,
) -> anyhow::Result<bool> {
    let before = active.len();
    let resolved =
        crate::model::resolve_features(definitions, dependencies, requested, default_features)
            .map_err(anyhow::Error::msg)?;
    active.extend(resolved.active);
    Ok(active.len() != before)
}

fn enabled_features(
    definitions: &BTreeMap<String, Vec<String>>,
    dependencies: &[DependencySpec],
    requested: &[String],
    default_features: bool,
) -> anyhow::Result<BTreeSet<String>> {
    Ok(
        crate::model::resolve_features(definitions, dependencies, requested, default_features)
            .map_err(anyhow::Error::msg)?
            .active,
    )
}

pub(crate) fn dependency_root(
    package_root: &Path,
    dependency: &DependencySpec,
) -> io::Result<PathBuf> {
    if let DependencySource::Path(path) = &dependency.source {
        return fs::canonicalize(package_root.join(path));
    }
    let project_root =
        crate::find_project_root(package_root).unwrap_or_else(|| package_root.into());
    let config = Config::load(&project_root).map_err(io::Error::other)?;
    let lock_path = lock_path(&project_root).map_err(io::Error::other)?;
    let lock = lock::read(&lock_path)?.ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            format!("missing `{}`; run `clue fetch`", lock_path.display()),
        )
    })?;
    let package = lock
        .package
        .iter()
        .find(|package| {
            package.name == dependency.package
                && dependency.requirement.as_ref().is_none_or(|requirement| {
                    Version::parse(&package.version)
                        .is_ok_and(|version| requirement.matches(&version))
                })
                && match &dependency.source {
                    DependencySource::Git { url, .. } => {
                        package.source.starts_with(&format!("git+{url}#"))
                    }
                    DependencySource::Registry { registry } => config
                        .registry(registry.as_deref())
                        .is_ok_and(|(_, registry)| {
                            package.source == format!("registry+{}", registry.index)
                        }),
                    DependencySource::Path(_) => false,
                }
        })
        .ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!(
                    "dependency `{}` is not present in `{}`",
                    dependency.alias,
                    lock_path.display()
                ),
            )
        })?;
    let version = Version::parse(&package.version).map_err(io::Error::other)?;
    let root = if let Some(source) = package.source.strip_prefix("registry+") {
        registry_source_dir(&config.home, source, &package.name, &version)
    } else if let Some(source) = package.source.strip_prefix("git+") {
        let (url, revision) = source
            .rsplit_once('#')
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "invalid git source in Clue.lock"))?;
        config
            .home
            .join("git/checkouts")
            .join(short_hash(url))
            .join(revision)
    } else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "unsupported source in Clue.lock",
        ));
    };
    cached_package_root(&root).ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            format!(
                "dependency cache `{}` is missing; run `clue fetch`",
                root.display()
            ),
        )
    })
}

fn registry_source_dir(home: &Path, index: &str, name: &str, version: &Version) -> PathBuf {
    home.join("registry/src")
        .join(short_hash(index))
        .join(format!("{name}-{version}"))
}

fn remove_cache_entry(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn cached_package_root(root: &Path) -> Option<PathBuf> {
    if root.join(manifest::CLUE_PROJECT_FILE_NAME).is_file() {
        return Some(root.to_path_buf());
    }
    let mut entries = fs::read_dir(root).ok()?.filter_map(Result::ok);
    let only = entries.next()?.path();
    if entries.next().is_none() && only.join(manifest::CLUE_PROJECT_FILE_NAME).is_file() {
        Some(only)
    } else {
        None
    }
}

fn sparse_path(name: &str) -> PathBuf {
    let name = name.to_ascii_lowercase();
    match name.len() {
        0 => PathBuf::new(),
        1 => PathBuf::from("1").join(name),
        2 => PathBuf::from("2").join(name),
        3 => PathBuf::from("3").join(&name[..1]).join(name),
        _ => PathBuf::from(&name[..2]).join(&name[2..4]).join(name),
    }
}

fn short_hash(value: &str) -> String {
    sha256(value.as_bytes())[..16].to_owned()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn display_path(base: &Path, path: &Path) -> String {
    let path = path.strip_prefix(base).unwrap_or(path);
    if path.as_os_str().is_empty() {
        ".".into()
    } else {
        path.display().to_string()
    }
}

fn requirement_label(requirement: &VersionReq) -> String {
    let text = requirement.to_string();
    text.strip_prefix('^').unwrap_or(&text).to_owned()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temp, bytes)?;
    let result = lock::replace_file(&temp, path);
    let _ = fs::remove_file(temp);
    result
}

fn run_git<I, S>(directory: Option<&Path>, arguments: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    let status = command.args(arguments).status()?;
    if !status.success() {
        bail!("git command failed with {status}");
    }
    Ok(())
}

fn git_output<I, S>(directory: &Path, arguments: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()?;
    if !output.status.success() {
        bail!("git command failed with {}", output.status);
    }
    String::from_utf8(output.stdout).context("git output was not UTF-8")
}

pub(crate) fn clean(root: &Path) -> anyhow::Result<()> {
    let path = root.join(".clue").join("build");
    if path.exists() {
        fs::remove_dir_all(&path)?;
    }
    Ok(())
}

pub(crate) fn read_lock(root: &Path) -> anyhow::Result<LockFile> {
    let path = lock_path(root)?;
    lock::read(&path)?.ok_or_else(|| anyhow::anyhow!("missing `{}`", path.display()))
}

pub(crate) struct AddOptions<'a> {
    pub(crate) name: &'a str,
    pub(crate) version: Option<&'a str>,
    pub(crate) path: Option<&'a Path>,
    pub(crate) git: Option<&'a str>,
    pub(crate) branch: Option<&'a str>,
    pub(crate) tag: Option<&'a str>,
    pub(crate) rev: Option<&'a str>,
    pub(crate) registry: Option<&'a str>,
    pub(crate) package: Option<&'a str>,
    pub(crate) features: &'a [String],
    pub(crate) default_features: bool,
    pub(crate) optional: bool,
    pub(crate) dev: bool,
}

pub(crate) fn add(root: &Path, options: &AddOptions<'_>) -> anyhow::Result<()> {
    manifest::validate_package_name(options.name)?;
    if usize::from(options.path.is_some())
        + usize::from(options.git.is_some())
        + usize::from(options.registry.is_some())
        > 1
    {
        bail!("choose only one of --path, --git, or --registry");
    }
    if options.git.is_none()
        && (options.branch.is_some() || options.tag.is_some() || options.rev.is_some())
    {
        bail!("--branch, --tag, and --rev require --git");
    }
    if usize::from(options.branch.is_some())
        + usize::from(options.tag.is_some())
        + usize::from(options.rev.is_some())
        > 1
    {
        bail!("choose only one of --branch, --tag, or --rev");
    }
    let manifest_path = root.join(manifest::CLUE_PROJECT_FILE_NAME);
    let text = fs::read_to_string(&manifest_path)?;
    let mut document = text.parse::<DocumentMut>()?;
    let section = if options.dev {
        "dev-dependencies"
    } else {
        "dependencies"
    };
    if !document.contains_key(section) {
        document[section] = Item::Table(toml_edit::Table::new());
    }
    let simple = options.path.is_none()
        && options.git.is_none()
        && options.registry.is_none()
        && options.package.is_none()
        && options.features.is_empty()
        && options.default_features
        && !options.optional;
    document[section][options.name] = if simple {
        value(options.version.unwrap_or("*"))
    } else {
        let mut dependency = InlineTable::new();
        if let Some(version) = options.version {
            dependency.insert("version", EditValue::from(version));
        }
        if let Some(path) = options.path {
            dependency.insert("path", EditValue::from(path.display().to_string()));
        }
        if let Some(git) = options.git {
            dependency.insert("git", EditValue::from(git));
        }
        for (key, selected) in [
            ("branch", options.branch),
            ("tag", options.tag),
            ("rev", options.rev),
            ("registry", options.registry),
            ("package", options.package),
        ] {
            if let Some(selected) = selected {
                dependency.insert(key, EditValue::from(selected));
            }
        }
        if !options.features.is_empty() {
            dependency.insert(
                "features",
                EditValue::Array(options.features.iter().map(String::as_str).collect()),
            );
        }
        if !options.default_features {
            dependency.insert("default-features", EditValue::from(false));
        }
        if options.optional {
            dependency.insert("optional", EditValue::from(true));
        }
        Item::Value(EditValue::InlineTable(dependency))
    };
    atomic_write(&manifest_path, document.to_string().as_bytes())?;
    Ok(())
}

pub(crate) fn remove(root: &Path, name: &str, dev: bool) -> anyhow::Result<()> {
    let manifest_path = root.join(manifest::CLUE_PROJECT_FILE_NAME);
    let text = fs::read_to_string(&manifest_path)?;
    let mut document = text.parse::<DocumentMut>()?;
    let section = if dev {
        "dev-dependencies"
    } else {
        "dependencies"
    };
    let removed = document
        .get_mut(section)
        .and_then(Item::as_table_mut)
        .and_then(|dependencies| dependencies.remove(name));
    if removed.is_none() {
        bail!("dependency `{name}` was not found in [{section}]");
    }
    atomic_write(&manifest_path, document.to_string().as_bytes())?;
    Ok(())
}

pub(crate) fn tree(root: &Path, show_features: bool) -> anyhow::Result<String> {
    let lock = read_lock(root)?;
    let root = lock
        .package
        .iter()
        .find(|package| package.path == ".")
        .or_else(|| lock.package.first())
        .ok_or_else(|| anyhow::anyhow!("Clue.lock contains no packages"))?;
    let mut output = String::new();
    write_tree(
        &lock,
        root,
        "",
        true,
        show_features,
        &mut BTreeSet::new(),
        &mut output,
    );
    Ok(output)
}

fn write_tree(
    lock: &LockFile,
    package: &LockPackage,
    prefix: &str,
    last: bool,
    show_features: bool,
    seen: &mut BTreeSet<(String, String)>,
    output: &mut String,
) {
    if prefix.is_empty() {
        output.push_str(&format!("{} {}", package.name, package.version));
    } else {
        output.push_str(prefix);
        output.push_str(if last { "`-- " } else { "|-- " });
        output.push_str(&format!("{} {}", package.name, package.version));
    }
    if show_features && !package.features.is_empty() {
        output.push_str(&format!(" [features: {}]", package.features.join(",")));
    }
    output.push('\n');
    if !seen.insert((package.name.clone(), package.source.clone())) {
        return;
    }
    for (index, dependency) in package.dependencies.iter().enumerate() {
        let Some(child) = lock.package.iter().find(|item| item.name == *dependency) else {
            continue;
        };
        let child_prefix = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}{}   ", if last { " " } else { "|" })
        };
        write_tree(
            lock,
            child,
            &child_prefix,
            index + 1 == package.dependencies.len(),
            show_features,
            seen,
            output,
        );
    }
}

pub(crate) fn metadata(root: &Path) -> anyhow::Result<String> {
    let manifest = manifest::read(root, ProjectKind::Binary)?;
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "manifest": {
            "name": manifest.name,
            "version": manifest.version,
            "license": manifest.license,
            "publish": manifest.publish,
        },
        "lock": read_lock(root)?,
    }))?)
}

pub(crate) fn archive(root: &Path) -> anyhow::Result<PathBuf> {
    let root = fs::canonicalize(root)?;
    let manifest = manifest::read(&root, ProjectKind::Binary)?;
    let output_dir = root.join(".clue/package");
    fs::create_dir_all(&output_dir)?;
    let output = output_dir.join(format!("{}-{}.cluepkg", manifest.name, manifest.version));
    let temp = output.with_extension(format!("tmp-{}", std::process::id()));
    let file = fs::File::create(&temp)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    let prefix = format!("{}-{}", manifest.name, manifest.version);
    for relative in package_files(&root)? {
        archive.append_path_with_name(root.join(&relative), Path::new(&prefix).join(relative))?;
    }
    archive.into_inner()?.finish()?;
    lock::replace_file(&temp, &output)?;
    Ok(output)
}

pub(crate) fn package_list(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    package_files(&fs::canonicalize(root)?)
}

fn package_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let walk_root = root.to_path_buf();
    let mut files = ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(move |entry| {
            entry.path() == walk_root
                || !matches!(entry.file_name().to_str(), Some(".git" | ".clue"))
        })
        .build()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_symlink()) => {
                Some(Err(anyhow::anyhow!(
                    "package contains symbolic link `{}`",
                    entry.path().display()
                )))
            }
            Ok(entry) if entry.path() != root && !entry.path().is_dir() => Some(
                entry
                    .path()
                    .strip_prefix(root)
                    .map(Path::to_path_buf)
                    .map_err(anyhow::Error::from),
            ),
            Ok(_) => None,
            Err(error) => Some(Err(anyhow::Error::from(error))),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    files.sort();
    Ok(files)
}

pub(crate) fn publish(
    root: &Path,
    registry: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<PathBuf> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (root, registry, dry_run);
        bail!("publishing packages is unavailable in wasm builds");
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let root = fs::canonicalize(root)?;
        let config = Config::load(&root)?;
        let manifest = manifest::read(&root, ProjectKind::Binary)?;
        let (registry_name, registry) = config.registry(registry)?;
        if let Some(allowed) = &manifest.publish
            && !allowed.iter().any(|allowed| allowed == registry_name)
        {
            bail!(
                "package `{}` cannot be published to registry `{registry_name}`",
                manifest.name
            );
        }
        let archive = archive(&root)?;
        if dry_run {
            return Ok(archive);
        }
        let api = registry
            .api
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("registry has no API URL configured"))?;
        let file_name = archive
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("package.cluepkg")
            .to_owned();
        let bytes = fs::read(&archive)?;
        let part = reqwest::blocking::multipart::Part::bytes(bytes).file_name(file_name);
        let form = reqwest::blocking::multipart::Form::new().part("package", part);
        let mut request = reqwest::blocking::Client::builder()
            .user_agent(format!("clue/{}", env!("CARGO_PKG_VERSION")))
            .build()?
            .post(format!("{}/v1/crates/new", api.trim_end_matches('/')))
            .multipart(form);
        if let Some(token) = &registry.token {
            request = request.bearer_auth(token);
        }
        request.send()?.error_for_status()?;
        Ok(archive)
    }
}

pub(crate) fn install_path(
    root: &Path,
    profile: crate::BuildProfile,
    offline: Option<bool>,
) -> anyhow::Result<PathBuf> {
    let root = fs::canonicalize(root)?;
    prepare(&root, false, offline, false, &[])?;
    let artifact = crate::build::run_with_options(&root, None, profile, None, &[], false)?;
    let crate::build::BuildArtifact::Executable { path, .. } = artifact else {
        bail!("cannot install a library package");
    };
    let config = Config::load(&root)?;
    let bin_dir = config.home.join("bin");
    fs::create_dir_all(&bin_dir)?;
    let destination = bin_dir.join(
        path.file_name()
            .ok_or_else(|| anyhow::anyhow!("built executable has no file name"))?,
    );
    fs::copy(path, &destination)?;
    Ok(destination)
}

pub(crate) fn install_registry(
    root: &Path,
    name: &str,
    requirement: Option<&str>,
    registry: Option<&str>,
    profile: crate::BuildProfile,
    offline: Option<bool>,
) -> anyhow::Result<PathBuf> {
    manifest::validate_package_name(name)?;
    let requirement = VersionReq::parse(requirement.unwrap_or("*"))?;
    let root = fs::canonicalize(root)?;
    let mut config = Config::load(&root)?;
    if let Some(offline) = offline {
        config.offline = offline;
    }
    let (registry_name, index) = {
        let (registry_name, registry) = config.registry(registry)?;
        (registry_name.to_owned(), registry.index.clone())
    };
    let mut resolver = Resolver {
        root,
        config,
        include_dev: false,
        locked: None,
        registry_cache: BTreeMap::new(),
    };
    let version = resolver
        .registry_versions(&registry_name, &index, name)?
        .into_iter()
        .filter(|version| !version.yanked && requirement.matches(&version.version))
        .max_by(|left, right| left.version.cmp(&right.version))
        .ok_or_else(|| anyhow::anyhow!("no version of `{name}` matches `{requirement}`"))?;
    let candidate = resolver.registry_candidate(&registry_name, &index, version)?;
    resolver.materialize_registry(&candidate)?;
    let source = registry_source_dir(
        &resolver.config.home,
        &index,
        &candidate.name,
        &candidate.version,
    );
    let source = cached_package_root(&source)
        .ok_or_else(|| anyhow::anyhow!("installed package source is missing"))?;
    install_path(&source, profile, offline)
}

pub(crate) fn install_git(
    root: &Path,
    url: &str,
    reference: GitReference,
    profile: crate::BuildProfile,
    offline: Option<bool>,
) -> anyhow::Result<PathBuf> {
    let root = fs::canonicalize(root)?;
    let mut config = Config::load(&root)?;
    if let Some(offline) = offline {
        config.offline = offline;
    }
    let resolver = Resolver {
        root,
        config,
        include_dev: false,
        locked: None,
        registry_cache: BTreeMap::new(),
    };
    let (source, _) = resolver.git_checkout(url, &reference, None)?;
    install_path(&source, profile, offline)
}

pub(crate) fn uninstall(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    manifest::validate_package_name(name)?;
    let config = Config::load(root)?;
    let path = config.home.join("bin").join(format!(
        "{name}{}",
        crate::TargetTriple::host()
            .map_err(anyhow::Error::msg)?
            .executable_suffix()
    ));
    fs::remove_file(&path).with_context(|| format!("failed to remove `{}`", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_registry_paths_match_the_crates_layout() {
        assert_eq!(sparse_path("a"), PathBuf::from("1/a"));
        assert_eq!(sparse_path("ab"), PathBuf::from("2/ab"));
        assert_eq!(sparse_path("abc"), PathBuf::from("3/a/abc"));
        assert_eq!(sparse_path("serde"), PathBuf::from("se/rd/serde"));
    }

    #[test]
    fn feature_expansion_enables_optional_dependencies() {
        let dependencies = vec![DependencySpec {
            alias: "logging".into(),
            package: "logging".into(),
            source: DependencySource::Registry { registry: None },
            requirement: None,
            optional: true,
            features: Vec::new(),
            default_features: true,
            kind: DependencyKind::Normal,
        }];
        let definitions = BTreeMap::from([
            (
                "default".into(),
                vec!["dep:logging".into(), "logging/std".into(), "extra".into()],
            ),
            ("extra".into(), Vec::new()),
        ]);
        let features = enabled_features(&definitions, &dependencies, &[], true).unwrap();
        assert!(features.contains("logging"));
        assert!(features.contains("extra"));
        let resolution =
            crate::model::resolve_features(&definitions, &dependencies, &[], true).unwrap();
        assert!(resolution.dependency_features["logging"].contains("std"));
    }
}
