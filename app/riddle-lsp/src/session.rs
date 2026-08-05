use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use riddlec::pipeline::CheckSession;

use crate::server::Document;

#[derive(Default)]
pub struct AnalysisSessions {
    standalone: Mutex<HashMap<lsp_types::Url, Arc<Mutex<CheckSession>>>>,
    projects: Mutex<HashMap<PathBuf, Arc<Mutex<clue::ProjectSession>>>>,
}

impl AnalysisSessions {
    pub(crate) fn standalone(&self, uri: &lsp_types::Url) -> Arc<Mutex<CheckSession>> {
        Arc::clone(
            self.standalone
                .lock()
                .unwrap()
                .entry(uri.clone())
                .or_default(),
        )
    }

    pub(crate) fn project(&self, root: &std::path::Path) -> Arc<Mutex<clue::ProjectSession>> {
        Arc::clone(
            self.projects
                .lock()
                .unwrap()
                .entry(root.to_path_buf())
                .or_default(),
        )
    }

    pub(crate) fn retain_open(&self, docs: &HashMap<lsp_types::Url, Document>) {
        self.standalone
            .lock()
            .unwrap()
            .retain(|uri, _| docs.contains_key(uri));
        let roots: HashSet<_> = docs
            .keys()
            .filter_map(|uri| uri.to_file_path().ok())
            .filter_map(|path| clue::find_project_root(&path))
            .collect();
        self.projects
            .lock()
            .unwrap()
            .retain(|root, _| roots.contains(root));
    }

    pub(crate) fn clear_projects(&self) {
        self.projects.lock().unwrap().clear();
    }

    pub(crate) fn invalidate_project(&self, uri: &lsp_types::Url) {
        let Some(root) = uri
            .to_file_path()
            .ok()
            .and_then(|path| clue::find_project_root(&path))
        else {
            return;
        };
        self.projects.lock().unwrap().remove(&root);
    }

    pub(crate) fn current_revision(
        &self,
        uri: &lsp_types::Url,
        docs: &HashMap<lsp_types::Url, Document>,
    ) -> Option<u64> {
        let Ok(path) = uri.to_file_path() else {
            return Some(0);
        };
        let Some(root) = clue::find_project_root(&path) else {
            return Some(0);
        };
        let overlays = docs
            .iter()
            .filter_map(|(uri, document)| {
                uri.to_file_path()
                    .ok()
                    .map(|path| (path, document.text.clone()))
            })
            .collect::<HashMap<_, _>>();
        let session = self.projects.lock().unwrap().get(&root).cloned()?;
        let session = session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        session
            .inputs_are_current(&overlays)
            .then(|| session.revision())
    }

    pub(crate) fn revision(&self, uri: &lsp_types::Url) -> u64 {
        let Some(root) = uri
            .to_file_path()
            .ok()
            .and_then(|path| clue::find_project_root(&path))
        else {
            return 0;
        };
        self.projects
            .lock()
            .unwrap()
            .get(&root)
            .map_or(0, |session| {
                session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .revision()
            })
    }
}
