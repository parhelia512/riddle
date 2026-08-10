use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use crate::{index::ProjectIndex, text::normalized_path};

const SKIPPED_DIRECTORIES: &[&str] = &[".git", ".clue", "target", "node_modules", "dist"];

#[derive(Default)]
pub struct WorkspaceState {
    inner: RwLock<WorkspaceData>,
}

#[derive(Default)]
struct WorkspaceData {
    roots: BTreeSet<PathBuf>,
    projects: BTreeSet<PathBuf>,
    snapshots: HashMap<PathBuf, Arc<ProjectIndex>>,
    file_projects: HashMap<PathBuf, BTreeSet<PathBuf>>,
    project_generations: HashMap<PathBuf, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildToken {
    project: PathBuf,
    generation: u64,
}

impl WorkspaceState {
    /// Replaces the workspace roots and rediscovers Clue projects.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a workspace directory cannot be read.
    pub fn set_roots(&self, roots: impl IntoIterator<Item = PathBuf>) -> io::Result<()> {
        let roots = normalize_roots(roots);
        self.replace_roots(roots)
    }

    /// Adds workspace roots and rediscovers Clue projects.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a workspace directory cannot be read.
    ///
    /// # Panics
    ///
    /// Panics if the workspace lock is poisoned.
    pub fn add_roots(&self, roots: impl IntoIterator<Item = PathBuf>) -> io::Result<()> {
        let added = normalize_roots(roots);
        let mut current = self.inner.read().unwrap().roots.clone();
        current.extend(added);
        self.replace_roots(current)
    }

    /// Removes workspace roots.
    ///
    /// # Panics
    ///
    /// Panics if the workspace lock is poisoned.
    pub fn remove_roots(&self, roots: impl IntoIterator<Item = PathBuf>) {
        let removed = normalize_roots(roots);
        let retained = self
            .inner
            .read()
            .unwrap()
            .roots
            .difference(&removed)
            .cloned()
            .collect();
        let _ = self.replace_roots(retained);
    }

    #[must_use]
    /// Returns configured workspace roots.
    ///
    /// # Panics
    ///
    /// Panics if the workspace lock is poisoned.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.inner.read().unwrap().roots.iter().cloned().collect()
    }

    #[must_use]
    /// Returns discovered Clue project roots.
    ///
    /// # Panics
    ///
    /// Panics if the workspace lock is poisoned.
    pub fn projects(&self) -> Vec<PathBuf> {
        self.inner
            .read()
            .unwrap()
            .projects
            .iter()
            .cloned()
            .collect()
    }

    #[must_use]
    /// Starts a new index generation for a project.
    ///
    /// # Panics
    ///
    /// Panics if the workspace lock is poisoned.
    pub fn begin_rebuild(&self, project: &Path) -> RebuildToken {
        let project = normalized_path(project.to_path_buf());
        let mut data = self.inner.write().unwrap();
        let generation = data.project_generations.entry(project.clone()).or_default();
        *generation = generation.wrapping_add(1).max(1);
        let generation = *generation;
        drop(data);
        RebuildToken {
            project,
            generation,
        }
    }

    #[must_use]
    /// Returns whether a rebuild token is still current.
    ///
    /// # Panics
    ///
    /// Panics if the workspace lock is poisoned.
    pub fn is_current(&self, token: &RebuildToken) -> bool {
        self.inner
            .read()
            .unwrap()
            .project_generations
            .get(&token.project)
            .is_some_and(|generation| *generation == token.generation)
    }

    /// Installs a complete project snapshot when its token is current.
    ///
    /// # Panics
    ///
    /// Panics if the workspace lock is poisoned.
    pub fn install(&self, token: RebuildToken, index: ProjectIndex) -> bool {
        let mut data = self.inner.write().unwrap();
        if data.project_generations.get(&token.project) != Some(&token.generation)
            || normalized_path(index.project.clone()) != token.project
        {
            return false;
        }
        remove_snapshot(&mut data, &token.project);
        let index = Arc::new(index);
        for file in &index.files {
            data.file_projects
                .entry(normalized_path(file.clone()))
                .or_default()
                .insert(token.project.clone());
        }
        data.snapshots.insert(token.project, index);
        true
    }

    #[must_use]
    /// Returns the current snapshot for a project.
    ///
    /// # Panics
    ///
    /// Panics if the workspace lock is poisoned.
    pub fn snapshot(&self, project: &Path) -> Option<Arc<ProjectIndex>> {
        self.inner
            .read()
            .unwrap()
            .snapshots
            .get(&normalized_path(project.to_path_buf()))
            .cloned()
    }

    #[cfg(feature = "test")]
    #[must_use]
    /// Returns every installed snapshot for tests.
    ///
    /// # Panics
    ///
    /// Panics if the workspace lock is poisoned.
    pub fn snapshots(&self) -> Vec<Arc<ProjectIndex>> {
        self.inner
            .read()
            .unwrap()
            .snapshots
            .values()
            .cloned()
            .collect()
    }

    /// Invalidates every snapshot that contains `path`.
    ///
    /// # Panics
    ///
    /// Panics if the workspace lock is poisoned.
    pub fn invalidate_path(&self, path: &Path) -> Vec<PathBuf> {
        let path = normalized_path(path.to_path_buf());
        let mut data = self.inner.write().unwrap();
        let projects = data.file_projects.get(&path).cloned().unwrap_or_default();
        for project in &projects {
            let generation = data.project_generations.entry(project.clone()).or_default();
            *generation = generation.wrapping_add(1).max(1);
            remove_snapshot(&mut data, project);
        }
        drop(data);
        projects.into_iter().collect()
    }

    fn replace_roots(&self, roots: BTreeSet<PathBuf>) -> io::Result<()> {
        let projects = discover_roots(&roots)?;
        let mut data = self.inner.write().unwrap();
        let removed = data
            .snapshots
            .keys()
            .filter(|project| !projects.contains(*project))
            .cloned()
            .collect::<Vec<_>>();
        for project in removed {
            remove_snapshot(&mut data, &project);
            data.project_generations.remove(&project);
        }
        data.roots = roots;
        data.projects = projects;
        drop(data);
        Ok(())
    }
}

fn remove_snapshot(data: &mut WorkspaceData, project: &Path) {
    let Some(snapshot) = data.snapshots.remove(project) else {
        return;
    };
    for file in &snapshot.files {
        let file = normalized_path(file.clone());
        let remove_file = data.file_projects.get_mut(&file).is_some_and(|projects| {
            projects.remove(project);
            projects.is_empty()
        });
        if remove_file {
            data.file_projects.remove(&file);
        }
    }
}

fn normalize_roots(roots: impl IntoIterator<Item = PathBuf>) -> BTreeSet<PathBuf> {
    roots.into_iter().map(normalized_path).collect()
}

fn discover_roots(roots: &BTreeSet<PathBuf>) -> io::Result<BTreeSet<PathBuf>> {
    let mut projects = BTreeSet::new();
    for root in roots {
        projects.extend(discover_projects(root)?);
    }
    Ok(projects)
}

/// Discovers Clue projects below a workspace root.
///
/// # Errors
///
/// Returns an I/O error when a directory entry cannot be read.
pub fn discover_projects(root: &Path) -> io::Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    if clue::is_virtual_workspace_root(root) {
        return clue::workspace_members(root)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()));
    }
    let mut pending = vec![root.to_path_buf()];
    let mut projects = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                let skipped = path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| SKIPPED_DIRECTORIES.contains(&name));
                if !skipped {
                    pending.push(path);
                }
            } else if path.file_name().and_then(OsStr::to_str) == Some(clue::CLUE_PROJECT_FILE_NAME)
                && let Some(root) = path.parent()
            {
                projects.insert(normalized_path(root.to_path_buf()));
            }
        }
    }
    Ok(projects.into_iter().collect())
}
