//! The status level: the working tree against the last commit, staging and
//! unstaging a row at a time, and committing what is staged.

use super::jobs::Job;
use super::load::{self, Side, StatusRow};
use super::{App, FormFor, Level};
use crate::commands::{commit, unstage};
use crate::model::count;
use crate::repo::Repo;
use crate::tui::widgets::{Field, Form};

#[derive(Clone, Copy, PartialEq)]
pub enum Scope {
    All,
    Staged,
    Unstaged,
}

pub const SCOPES: [Scope; 3] = [Scope::All, Scope::Staged, Scope::Unstaged];

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::Staged => "staged",
            Scope::Unstaged => "unstaged",
        }
    }

    fn keeps(self, r: &StatusRow) -> bool {
        match self {
            Scope::All => true,
            Scope::Staged => r.side == Side::Staged,
            Scope::Unstaged => r.side != Side::Staged,
        }
    }
}

impl App {
    /// Walk the working tree again in the background, the cursor kept on its row.
    pub(super) fn request_status(&mut self) {
        if let Some(r) = self.status_row() {
            self.ssel.restore = Some(r.change.path.clone());
        }
        let seq = self.sfeed.issue();
        load::stream_status(self.repo.clone(), seq, self.tx.clone());
    }

    pub(super) fn status_in(&mut self, rows: Result<Vec<StatusRow>, String>) {
        match rows {
            Ok(rows) => {
                self.status = rows;
                self.status_failed = None;
            }
            Err(e) => {
                self.status.clear();
                self.status_failed = Some(e);
            }
        }
        self.rescope_status();
        if self.level == Level::Status {
            self.pending = true;
        }
    }

    pub(super) fn rescope_status(&mut self) {
        let scope = SCOPES[self.ssel.scope];
        let query = &self.ssel.query;
        let visible = (0..self.status.len())
            .filter(|&i| {
                let r = &self.status[i];
                scope.keeps(r) && query.keeps(&r.change.shown())
            })
            .collect();
        self.ssel.show(visible);
        let rows = &self.status;
        self.ssel.settle_on(|i| rows[i].change.path.clone());
    }

    pub(super) fn status_row(&self) -> Option<&StatusRow> {
        self.ssel.idx().map(|i| &self.status[i])
    }

    /// The pane below: the selected file's versions in history.
    pub(super) fn enter_status(&mut self) {
        let path = self.status_row().map(|r| match &r.change.from {
            Some(from) => from.clone(),
            None => r.change.path.clone(),
        });
        self.show_history(path);
    }

    /// `↵`: stage what is not staged, unstage what is. A move is both its paths.
    pub(super) fn toggle(&mut self) {
        let Some(r) = self.status_row() else {
            return;
        };
        let mut paths = vec![r.change.path.clone()];
        paths.extend(r.change.from.clone());
        if r.side == Side::Staged {
            self.unstage(&paths);
        } else {
            self.pending_job = Some(Job::Stage(paths));
        }
    }

    /// `S`: stage every row not staged.
    pub(super) fn stage_all(&mut self) {
        let paths: Vec<String> = self
            .status
            .iter()
            .filter(|r| r.side != Side::Staged)
            .flat_map(|r| std::iter::once(r.change.path.clone()).chain(r.change.from.clone()))
            .collect();
        if paths.is_empty() {
            self.set_status("nothing to stage");
            return;
        }
        self.pending_job = Some(Job::Stage(paths));
    }

    /// `U`: unstage everything.
    pub(super) fn unstage_all(&mut self) {
        let paths: Vec<String> = self
            .status
            .iter()
            .filter(|r| r.side == Side::Staged)
            .flat_map(|r| std::iter::once(r.change.path.clone()).chain(r.change.from.clone()))
            .collect();
        if paths.is_empty() {
            self.set_status("nothing staged");
            return;
        }
        self.unstage(&paths);
    }

    fn unstage(&mut self, paths: &[String]) {
        match unstage::unstage_paths(&Repo::at(self.repo.clone()), paths) {
            Ok(n) => self.set_status(format!("unstaged {}", count(n, "file", "files"))),
            Err(e) => self.alert("could not unstage", &e),
        }
        self.request_status();
    }

    /// `c`: a form for the commit's message.
    pub(super) fn create_commit(&mut self) {
        if !self.status.iter().any(|r| r.side == Side::Staged) {
            self.set_failed("nothing staged to commit: ↵ stages a row, S every row");
            return;
        }
        let form = Form::new(
            "new commit",
            vec![Field::new(
                "message",
                "what this snapshot is, one line",
                true,
            )],
        );
        self.form = Some((form, FormFor::Commit));
    }

    pub(super) fn submit_commit(&mut self, form: &Form) -> bool {
        let message = form.value(0).to_string();
        match commit::commit(&Repo::at(self.repo.clone()), &message) {
            Ok((hash, _)) => {
                self.set_status(format!(
                    "committed {} \"{message}\"",
                    crate::model::short(&hash)
                ));
                self.request_status();
                self.request_log();
                true
            }
            Err(e) => {
                self.alert("could not commit", &e);
                false
            }
        }
    }
}
