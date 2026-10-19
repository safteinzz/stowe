//! The status level: the working tree against the last commit, staging and
//! unstaging a row at a time, and committing what is staged.

use super::jobs::{Confirm, Discard, Job, Target};
use super::load::{self, Side, StatusRow};
use super::patch::{self, Open};
use super::{App, FormFor, Level};
use crate::commands::{commit, restore, unstage};
use crate::model::{Entry, count};
use crate::repo::Repo;
use crate::scan;
use crate::tui::difftool::{DiffTool, run_difftool, run_editor};
use crate::tui::highlight::{DiffContext, RenderedDiff};
use crate::tui::widgets::{Field, Form, Modal};
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

    /// `d`: the typed gate in front of throwing a row's changes away. The
    /// remote is picked here, so its mount runs before the job starts; a row
    /// whose committed bytes are neither here nor on a remote is refused.
    pub(super) fn ask_discard(&mut self) {
        let Some(r) = self.status_row() else {
            return;
        };
        let (side, change) = (r.side, r.change.clone());
        let repo = Repo::at(self.repo.clone());
        let head = match repo.head_manifest() {
            Ok(head) => head,
            Err(e) => return self.alert(format!("could not read {}", change.path), &e),
        };
        let mut paths = vec![change.path.clone()];
        paths.extend(change.from.clone());
        let committed = |p: &str| head.iter().find(|e| e.path == p);
        let what = match (&change.from, side) {
            (Some(from), _) if committed(from).is_some() => Discard::Moved(from.clone()),
            (_, Side::Untracked) => Discard::Untracked,
            _ if committed(&change.path).is_none() => Discard::Added,
            _ => Discard::Changed,
        };
        // A move puts back the file it moved, so only an edit or a removal
        // needs the committed bytes from somewhere.
        let wanted = match what {
            Discard::Changed => committed(&change.path)
                .filter(|e| !untouched(&repo, e) && !crate::kept::has(&repo, &e.hash)),
            _ => None,
        };
        let wanted = wanted.cloned();
        let remote = match wanted {
            None => None,
            Some(e) => match self.holding(&repo, &e.hash) {
                Some(name) => Some(name),
                None => {
                    self.modal = Some(Modal::new(
                        format!("cannot put back {}", change.path),
                        "stowe keeps no copy of it here and no remote has the last commit's \
                         version yet, so it cannot come back: push, then d again",
                    ));
                    return;
                }
            },
        };
        self.confirm = Some(Confirm::new(Target::Discard {
            what,
            paths,
            remote,
        }));
    }

    /// The remote to bring back the content `hash` names: `origin` when the
    /// commit it is known to be at holds it, else the first that does.
    fn holding(&mut self, repo: &Repo, hash: &str) -> Option<String> {
        // istatus reads no remotes until one is needed
        self.load_remotes();
        let mut has = self.remotes.iter().filter(|r| {
            r.last.as_ref().is_some_and(|h| {
                repo.read_commit(h)
                    .is_ok_and(|c| c.files.iter().any(|f| f.hash == hash))
            })
        });
        let first = has.next()?;
        let origin = std::iter::once(first)
            .chain(has)
            .find(|r| r.name == "origin");
        Some(origin.unwrap_or(first).name.clone())
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

/// Whether the working file is as `e` records it, by size and time as a scan
/// tells, since hashing it here would freeze the screen on a video.
pub(super) fn untouched(repo: &Repo, e: &Entry) -> bool {
    let Ok(meta) = fs_err::metadata(repo.root.join(&e.path)) else {
        return false;
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    e.size == meta.len() && e.mtime == mtime
}

/// Put `paths` back as the last commit has them, in the working tree and the
/// index: a path it has gets its bytes back, from a file of the same row that
/// holds them (a move), the kept copy or `remote` (its name and url, already
/// reachable), and a path it lacks is deleted. Nothing is unstaged or deleted
/// until every byte is back.
pub(super) fn discard(
    repo: &Repo,
    paths: &[String],
    remote: Option<(&str, String)>,
) -> anyhow::Result<()> {
    let head = repo.head_manifest()?;
    let (kept, gone): (Vec<&String>, Vec<&String>) = paths
        .iter()
        .partition(|p| head.iter().any(|e| &e.path == *p));
    let mut moved = Vec::new();
    let mut fetch = Vec::new();
    for p in kept {
        let Some(e) = head.iter().find(|e| &e.path == p) else {
            continue;
        };
        let dest = repo.root.join(&e.path);
        if dest.exists() && scan::hash_file(&dest)? == e.hash {
            continue;
        }
        let here: Vec<&&String> = gone
            .iter()
            .filter(|g| repo.root.join(g).is_file())
            .collect();
        let same = here
            .iter()
            .find(|g| scan::hash_file(&repo.root.join(g)).is_ok_and(|h| h == e.hash));
        if let Some(g) = same {
            put(&dest, |tmp| fs_err::rename(repo.root.join(g), tmp))?;
            moved.push(g.as_str());
        } else if let Some(text) = crate::kept::read(repo, &e.hash) {
            put(&dest, |tmp| fs_err::write(tmp, text.as_bytes()))?;
        } else if remote.is_some() {
            fetch.push(e);
        } else if let Some(g) = here.first() {
            // a move whose bytes changed on the way, a song re-tagged say: it
            // goes back, and then shows as an edit of its own
            put(&dest, |tmp| fs_err::rename(repo.root.join(g), tmp))?;
            moved.push(g.as_str());
        } else {
            anyhow::bail!(
                "`{}` is not as the last commit has it, and no copy of that is here: d on it \
                 again brings it back from a remote",
                e.path
            );
        }
    }
    if let (Some((name, url)), false) = (&remote, fetch.is_empty()) {
        let head_hash = repo.head()?.unwrap_or_default();
        restore::restore(repo, name, url, &head_hash, &fetch, |_| {})?;
    }
    for g in &gone {
        let path = repo.root.join(g);
        if !moved.contains(&g.as_str()) && fs_err::symlink_metadata(&path).is_ok() {
            fs_err::remove_file(&path)?;
        }
        // the folders it leaves empty go too, as a move back leaves its new one
        for dir in path.ancestors().skip(1) {
            if dir == repo.root || fs_err::remove_dir(dir).is_err() {
                break;
            }
        }
    }
    unstage::unstage_paths(repo, paths)?;
    Ok(())
}

/// Write `dest` through a file beside it, so a failure halfway never leaves
/// it cut short.
fn put(
    dest: &std::path::Path,
    write: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    let dir = dest.parent().unwrap_or(std::path::Path::new("."));
    fs_err::create_dir_all(dir)?;
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.part", std::process::id()));
    write(&tmp)?;
    fs_err::rename(&tmp, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::add;

    #[test]
    fn d_with_no_copy_of_the_last_commit_leaves_the_file_and_the_index_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repo::init(tmp.path()).unwrap();
        let song = tmp.path().join("song.flac");
        // a NUL keeps it binary, so no copy of it is kept to put it back from
        fs_err::write(&song, b"fLaC\0take one").unwrap();
        add::stage_all(&repo).unwrap();
        commit::commit(&repo, "song").unwrap();
        fs_err::write(&song, b"fLaC\0take two, longer").unwrap();
        add::stage_all(&repo).unwrap();
        let staged = repo.read_index().unwrap();

        assert!(
            discard(&repo, &["song.flac".to_string()], None).is_err(),
            "with no copy of the committed song and no remote, d must refuse"
        );
        assert_eq!(
            fs_err::read(&song).unwrap(),
            b"fLaC\0take two, longer",
            "the edited song was overwritten although its committed bytes were nowhere"
        );
        assert!(
            repo.read_index().unwrap() == staged,
            "the edit was unstaged although nothing could be put back"
        );
    }
}
