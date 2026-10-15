//! The status level: the working tree against the last commit, staging and
//! unstaging a row at a time, and committing what is staged.

use super::jobs::Job;
use super::load::{self, Side, StatusRow};
use super::patch::{self, Open};
use super::{App, FormFor, Level};
use crate::commands::{commit, unstage};
use crate::model::count;
use crate::repo::Repo;
use crate::tui::difftool::{DiffTool, run_difftool, run_editor};
use crate::tui::highlight::{DiffContext, RenderedDiff};
use crate::tui::widgets::{Field, Form};
use ratatui::DefaultTerminal;

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

    /// Ask for the selected file's diff. Reading it and the syntect pass run
    /// on a worker, so moving the cursor never waits for either.
    pub(super) fn enter_status(&mut self) {
        self.prepared = RenderedDiff::default();
        self.dscroll = 0;
        self.dhscroll = 0;
        let Some(r) = self.status_row() else {
            self.dfeed.idle();
            return;
        };
        let (repo, side, change) = (Repo::at(self.repo.clone()), r.side, r.change.clone());
        self.dfeed
            .request(move || match patch::sides(&repo, side, &change) {
                Ok(sides) => patch::raw(&sides),
                Err(e) => (
                    format!("could not read it: {}", crate::errors::words(&e)),
                    DiffContext::default(),
                ),
            });
    }

    /// `↵`: the selected file in the user's `git difftool`, the comparison the
    /// pane shows, or in their editor when only one version of it is here.
    /// Read again after, since either may have edited the file.
    pub(super) fn difftool(&mut self, terminal: &mut DefaultTerminal) {
        let Some(r) = self.status_row() else {
            return;
        };
        let (side, change) = (r.side, r.change.clone());
        let repo = Repo::at(self.repo.clone());
        let scratch = std::env::temp_dir().join(format!("stowe-difftool-{}", std::process::id()));
        let picked = match patch::sides(&repo, side, &change) {
            Ok(sides) => patch::open(&repo, &sides, side, &scratch),
            Err(e) => Open::Nothing(crate::errors::words(&e)),
        };
        let outcome = match picked {
            Open::Diff(old, new) => run_difftool(terminal, self.enhanced, &repo.root, &old, &new),
            Open::Edit(path) => run_editor(terminal, self.enhanced, &path),
            Open::Nothing(why) => {
                DiffTool::Note(format!("nothing to open for {}: {why}", change.path))
            }
        };
        let _ = fs_err::remove_dir_all(&scratch);
        match outcome {
            DiffTool::Quiet => {}
            DiffTool::Note(m) => self.set_failed(m),
            DiffTool::Failed(modal) => self.modal = Some(modal),
        }
        self.request_status();
    }

    /// `space`: stage what is not staged, unstage what is. A move is both its paths.
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

    /// `S`: stage every visible row not staged.
    pub(super) fn stage_all(&mut self) {
        let paths = self.visible_paths(|side| side != Side::Staged);
        if paths.is_empty() {
            self.set_status("nothing to stage");
            return;
        }
        self.pending_job = Some(Job::Stage(paths));
    }

    /// `U`: unstage every visible staged row.
    pub(super) fn unstage_all(&mut self) {
        let paths = self.visible_paths(|side| side == Side::Staged);
        if paths.is_empty() {
            self.set_status("nothing staged");
            return;
        }
        self.unstage(&paths);
    }

    /// The paths, a move's old one included, of the rows the scope and `/`
    /// leave on screen whose side is `wanted`, so a hidden row is never touched.
    fn visible_paths(&self, wanted: fn(Side) -> bool) -> Vec<String> {
        self.ssel
            .visible
            .iter()
            .map(|&i| &self.status[i])
            .filter(|r| wanted(r.side))
            .flat_map(|r| std::iter::once(r.change.path.clone()).chain(r.change.from.clone()))
            .collect()
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
            self.set_failed("nothing staged to commit: s stages a row, S every row");
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
                true
            }
            Err(e) => {
                self.alert("could not commit", &e);
                false
            }
        }
    }
}
