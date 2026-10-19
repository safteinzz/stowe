//! The repos level: every stowe repo under a folder, and drilling into one.

use super::App;
use super::load;
use crate::repos::State;
use std::path::PathBuf;

/// A repo found, and its state once its thread has read it.
pub(super) struct RepoRow {
    pub path: PathBuf,
    pub name: String,
    pub state: Option<Result<State, String>>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Scope {
    All,
    Changed,
    Unpushed,
}

/// Tab order: the default first, then local towards remote.
pub const SCOPES: [Scope; 3] = [Scope::All, Scope::Changed, Scope::Unpushed];

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::Changed => "changed",
            Scope::Unpushed => "unpushed",
        }
    }

    /// A repo still being read is kept by every tab, so it does not vanish
    /// and come back as its answer lands.
    fn keeps(self, r: &RepoRow) -> bool {
        match (self, &r.state) {
            (Scope::All, _) | (_, None) | (_, Some(Err(_))) => true,
            (Scope::Changed, Some(Ok(s))) => s.changed > 0,
            (Scope::Unpushed, Some(Ok(s))) => s.unpushed() + s.elsewhere() > 0,
        }
    }
}

impl App {
    /// List `found` at once and read each one's state on its own thread.
    pub(super) fn load_repos(&mut self, found: Vec<PathBuf>) {
        let seq = self.rfeed.issue();
        self.repos = found
            .iter()
            .map(|p| RepoRow {
                name: crate::repos::relative_name(p, &self.base),
                path: p.clone(),
                state: None,
            })
            .collect();
        load::stream_repos(found, self.base.clone(), seq, self.tx.clone());
        self.rescope_repos();
    }

    pub(super) fn repo_in(&mut self, path: PathBuf, state: Result<State, String>) {
        if let Some(r) = self.repos.iter_mut().find(|r| r.path == path) {
            r.state = Some(state);
        }
        self.rfeed.loading = self.repos.iter().any(|r| r.state.is_none());
        if SCOPES[self.rsel.scope] != Scope::All && !self.rfeed.loading {
            self.rsel.restore = self.rsel.idx().map(|i| self.repos[i].name.clone());
            self.rescope_repos();
            self.pending = true;
        }
    }

    pub(super) fn rescope_repos(&mut self) {
        let scope = SCOPES[self.rsel.scope];
        let query = &self.rsel.query;
        let visible = (0..self.repos.len())
            .filter(|&i| scope.keeps(&self.repos[i]) && query.keeps(&self.repos[i].name))
            .collect();
        self.rsel.show(visible);
        let repos = &self.repos;
        self.rsel.settle_on(|i| repos[i].name.clone());
    }

    /// Point the app at the repo under the cursor and read its history, which
    /// the pane below previews.
    pub(super) fn enter_repo(&mut self) {
        let Some(i) = self.rsel.idx() else {
            self.log.clear();
            self.csel.show(Vec::new());
            return;
        };
        if self.repos[i].path != self.repo || self.log.is_empty() {
            self.repo = self.repos[i].path.clone();
            self.load_remotes();
            self.request_log();
        }
    }
}
