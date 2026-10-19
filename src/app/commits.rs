//! The commits and files levels: the history read once per repo, a commit's
//! changes, one file's versions across every commit, and putting one back.

use super::jobs::{Confirm, Lost, Target};
use super::load;
use super::{App, Level};
use crate::history::{self, Change, Kind};
use crate::model::Entry;
use crate::repo::Repo;
use crate::tui::widgets::Modal;

/// Which commits the commits pane shows.
#[derive(Clone, Copy, PartialEq)]
pub enum Scope {
    All,
    /// On no remote yet.
    Local,
    Pushed,
}

pub const SCOPES: [Scope; 3] = [Scope::All, Scope::Local, Scope::Pushed];

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::Local => "local",
            Scope::Pushed => "pushed",
        }
    }
}

/// One version of a file: the commit, and what it did to the file there.
pub struct Version {
    pub at: usize,
    pub change: Change,
}

impl App {
    /// Read the repo's history again in the background, keeping the cursor on
    /// the commit it was on.
    pub(super) fn request_log(&mut self) {
        if let Some(c) = self.commit() {
            self.csel.restore = Some(c.hash.clone());
        }
        let seq = self.cfeed.issue();
        self.log.clear();
        self.log_failed = None;
        self.csel.show(Vec::new());
        // Versions and hits point into the log, so they go with it and are
        // worked out again once it is back.
        self.hist.clear();
        self.hsel.show(Vec::new());
        self.hist_for = (String::new(), false);
        self.hits.clear();
        self.xsel.show(Vec::new());
        load::stream_log(
            self.repo.clone(),
            seq,
            self.cfeed.latest.clone(),
            self.tx.clone(),
        );
    }

    /// Whether commit `at` (its place in the log, newest first) is on no
    /// remote: newer than the commit each of them is known to be at.
    pub(super) fn unpushed(&self, at: usize) -> bool {
        at < self.pushed_from
    }

    /// Where in the log the newest commit some remote has sits, which every
    /// commit above it is unpushed against. Past the end while no remote has
    /// one of the commits read so far.
    pub(super) fn update_pushed(&mut self) {
        self.pushed_from = self
            .remotes
            .iter()
            .filter_map(|r| r.last.as_ref())
            .filter_map(|h| self.log.iter().position(|c| &c.hash == h))
            .min()
            .unwrap_or(usize::MAX);
    }

    fn keeps(&self, at: usize) -> bool {
        let scope_ok = match SCOPES[self.csel.scope] {
            Scope::All => true,
            Scope::Local => self.unpushed(at),
            Scope::Pushed => !self.unpushed(at),
        };
        let c = &self.log[at];
        scope_ok
            && self.csel.query.keeps(&format!(
                "{} {} {}",
                c.hash,
                c.message,
                crate::time::short_time(c.time)
            ))
    }

    pub(super) fn extend_log(&mut self, from: usize) {
        let before = self.csel.idx();
        self.update_pushed();
        let more: Vec<usize> = (from..self.log.len()).filter(|&i| self.keeps(i)).collect();
        self.csel.append(more);
        let log = &self.log;
        self.csel.settle_on(|i| log[i].hash.clone());
        if matches!(self.level, Level::Commits | Level::Repos) && self.csel.idx() != before {
            self.pending = true;
        }
    }

    pub(super) fn rescope_log(&mut self) {
        let visible = (0..self.log.len()).filter(|&i| self.keeps(i)).collect();
        self.csel.show(visible);
        let log = &self.log;
        self.csel.settle_on(|i| log[i].hash.clone());
    }

    /// The whole history is in: what was waiting on it can be worked out now.
    pub(super) fn log_done(&mut self) {
        // `rescope_log` puts the cursor back at the top, so it is told the
        // commit `extend_log` already settled on while the rows streamed in.
        self.csel.restore = self.commit().map(|c| c.hash.clone());
        self.rescope_log();
        self.csel.restore = None;
        if self.hist_for.0.is_empty() || !self.hist_for.1 {
            self.pending = true;
        }
        self.scan_waiting();
    }

    pub(super) fn commit(&self) -> Option<&load::Logged> {
        self.csel.idx().map(|i| &self.log[i])
    }

    /// The pane below the commit list: the selected commit's changes.
    pub(super) fn enter_commit(&mut self) {
        match self.csel.idx() {
            Some(i) => {
                let hash = self.log[i].hash.clone();
                self.show_changes(i, &hash);
            }
            None => {
                self.files.clear();
                self.fsel.show(Vec::new());
                self.files_for.clear();
            }
        }
    }

    /// Show commit `at`'s changes in the files list, unless it already holds them.
    pub(super) fn show_changes(&mut self, at: usize, hash: &str) {
        if self.files_for == hash {
            return;
        }
        self.files = self.log[at].changes.clone();
        self.files_for = hash.to_string();
        self.fsel.restore = None;
        self.rescope_files();
    }

    pub(super) fn rescope_files(&mut self) {
        let query = &self.fsel.query;
        let visible = (0..self.files.len())
            .filter(|&i| query.keeps(&self.files[i].shown()))
            .collect();
        self.fsel.show(visible);
        let files = &self.files;
        self.fsel.settle_on(|i| files[i].path.clone());
    }

    pub(super) fn file(&self) -> Option<&Change> {
        self.fsel.idx().map(|i| &self.files[i])
    }

    /// `↵` on a commit: its changes become the top pane.
    pub(super) fn open_commit(&mut self) {
        if self.fsel.is_empty() {
            return;
        }
        self.level = Level::Files;
        self.enter_file();
    }

    /// The pane below the files list: the selected file's versions.
    pub(super) fn enter_file(&mut self) {
        let path = self.file().map(|f| f.path.clone());
        self.show_history(path);
    }

    /// Every commit that touched `path`, under any name a move gave it. Worked
    /// out from the history already read, so it costs no disk, and again once
    /// the read is whole if it was not yet.
    pub(super) fn show_history(&mut self, path: Option<String>) {
        let Some(path) = path else {
            self.hist.clear();
            self.hsel.show(Vec::new());
            self.hist_for = (String::new(), false);
            return;
        };
        let whole = !self.cfeed.loading;
        if self.hist_for == (path.clone(), whole) {
            return;
        }
        let moves: Vec<(String, String)> = self
            .log
            .iter()
            .flat_map(|c| &c.changes)
            .filter_map(|c| c.from.clone().map(|f| (f, c.path.clone())))
            .collect();
        let names = history::aliases(&moves, &path);
        let named = |p: &str| names.iter().any(|n| n == p);
        self.hist = self
            .log
            .iter()
            .enumerate()
            .flat_map(|(at, c)| {
                c.changes
                    .iter()
                    .filter(|ch| named(&ch.path) || ch.from.as_deref().is_some_and(named))
                    .map(move |ch| Version {
                        at,
                        change: ch.clone(),
                    })
            })
            .collect();
        self.hist_for = (path, whole);
        self.rescope_hist();
    }

    pub(super) fn rescope_hist(&mut self) {
        let query = &self.hsel.query;
        let log = &self.log;
        let visible = (0..self.hist.len())
            .filter(|&i| {
                let v = &self.hist[i];
                query.keeps(&format!("{} {}", log[v.at].message, v.change.shown()))
            })
            .collect();
        self.hsel.show(visible);
        // Put the cursor on the version the top pane is showing, when there is one.
        let here = self.files_for.clone();
        if let Some(at) = self
            .hsel
            .visible
            .iter()
            .position(|&i| self.log[self.hist[i].at].hash == here)
        {
            self.hsel.cur = at;
            self.hsel.state.select(Some(at));
        }
    }

    /// `↵` on a file: put it back as the open commit has it, or as it was just
    /// before that commit removed it.
    pub(super) fn ask_restore(&mut self) {
        let (Some(change), Some(commit)) = (self.file().cloned(), self.commit()) else {
            return;
        };
        let repo = Repo::at(self.repo.clone());
        let hash = commit.hash.clone();
        let found = (|| -> anyhow::Result<Option<(String, Entry)>> {
            let c = repo.read_commit(&hash)?;
            let (from, files) = match (change.kind, &c.parent) {
                (Kind::Removed, Some(p)) => (p.clone(), repo.read_commit(p)?.files),
                (Kind::Removed, None) => return Ok(None),
                _ => (hash.clone(), c.files),
            };
            Ok(files
                .into_iter()
                .find(|e| e.path == change.path)
                .map(|e| (from, e)))
        })();
        let (from, entry) = match found {
            Ok(Some(found)) => found,
            Ok(None) => return,
            Err(e) => return self.alert(format!("could not read {}", change.path), &e),
        };
        let remote = match self.restore_remote() {
            Some(r) => r,
            None => {
                self.modal = Some(Modal::new(
                    "no remote to restore from",
                    "stowe keeps no copy of old versions here; they come back from a remote. \
                     `stowe iremote` adds one.",
                ));
                return;
            }
        };
        let dest = repo.root.join(&entry.path);
        let Ok(meta) = fs_err::metadata(&dest) else {
            return self.confirmed_now(remote, from, entry);
        };
        // Size and time against the last commit, the way a scan tells an edited
        // file without reading it, since reading a video here would freeze the
        // screen.
        let committed = repo
            .head_manifest()
            .ok()
            .and_then(|m| m.into_iter().find(|e| e.path == entry.path));
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        let as_committed = committed
            .as_ref()
            .is_some_and(|e| e.size == meta.len() && e.mtime == mtime);
        if as_committed && committed.as_ref().is_some_and(|e| e.hash == entry.hash) {
            self.set_status(format!("{} is already this version", entry.path));
            return;
        }
        // stowe keeps no copy here, so the committed version is only safe to
        // replace when the commit some remote is known to be at holds it.
        let pushed = committed.as_ref().is_some_and(|e| {
            self.remotes
                .iter()
                .filter_map(|r| r.last.as_ref())
                .any(|h| {
                    repo.read_commit(h)
                        .is_ok_and(|c| c.files.iter().any(|f| f.hash == e.hash))
                })
        });
        let lost = match (as_committed, pushed) {
            (false, _) => Some(Lost::Uncommitted),
            (true, false) => Some(Lost::Unpushed),
            (true, true) => None,
        };
        self.confirm = Some(Confirm::new(Target::Restore {
            remote,
            commit: from,
            entry,
            lost,
        }));
    }

    /// A restore with nothing in the working tree to lose goes straight ahead.
    fn confirmed_now(&mut self, remote: String, commit: String, entry: Entry) {
        self.pending_job = Some(super::jobs::Job::Restore {
            remote,
            commit,
            entry,
        });
    }

    /// Where a restore reads from: `origin`, or else the first remote.
    fn restore_remote(&self) -> Option<String> {
        let names: Vec<&str> = self.remotes.iter().map(|r| r.name.as_str()).collect();
        names
            .iter()
            .find(|n| **n == "origin")
            .or(names.first())
            .map(|n| n.to_string())
    }
}
