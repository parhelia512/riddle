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
}
