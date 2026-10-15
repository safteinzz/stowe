//! What the drill reads in the background: a repo's whole history as changes,
//! its working tree against the last commit, a push's plan, whether a remote is
//! there, and each repo's state under `irepos`. Every reader is a thread that
//! sends its rows down one channel, tagged with the request they answer, so a
//! request the cursor has moved past is dropped rather than shown.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::thread;

use crate::history::{self, Change, Kind};
use crate::mirror;
use crate::model::Config;
use crate::repo::Repo;
use crate::repos;
use crate::scan;

/// How many commits a history read hands over at a time.
const BATCH: usize = 32;

/// One commit as the drill shows it, with everything it changed.
pub struct Logged {
    pub hash: String,
    pub time: i64,
    pub message: String,
    /// Files in its snapshot.
    pub files: usize,
    pub changes: Vec<Change>,
}

impl Logged {
    /// How many of its changes are of `kind`.
    pub fn count(&self, kind: Kind) -> usize {
        self.changes.iter().filter(|c| c.kind == kind).count()
    }
}

/// Where a file stands in the working tree against the last commit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Staged,
    Unstaged,
    Untracked,
}

pub struct StatusRow {
    pub side: Side,
    pub change: Change,
}

/// Whether a remote can be written to right now, as far as looking can tell
/// without writing to it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Checking,
    /// Its marker is there, or a folder a first push can create.
    Here,
    /// Not there, and nothing here knows how to make it so.
    Unplugged,
    /// Not there, but its mount command can bring it.
    Mountable,
    /// An object store, reached on credentials when it is used.
    Cloud,
}

impl Reach {
    pub fn word(self) -> &'static str {
        match self {
            Reach::Checking => "checking…",
            Reach::Here => "here",
            Reach::Unplugged => "unplugged",
            Reach::Mountable => "mountable",
            Reach::Cloud => "cloud",
        }
    }
}

/// A chunk of what a background read produced, `seq` naming the request.
pub enum Batch {
    Log {
        seq: u64,
        rows: Vec<Logged>,
        done: bool,
        /// Why the read stopped short, on the last batch of one that did.
        failed: Option<String>,
    },
    Status {
        seq: u64,
        rows: Result<Vec<StatusRow>, String>,
    },
    /// What pushing to a remote would do, or why that cannot be worked out.
    Plan {
        seq: u64,
        rows: Result<Vec<Change>, String>,
    },
    Reach {
        name: String,
        reach: Reach,
    },
    Repo {
        seq: u64,
        path: PathBuf,
        state: Result<repos::State, String>,
    },
    /// A job the screen started has finished.
    Job(super::jobs::Outcome),
}

/// Read `repo`'s history newest first, a batch at a time, stopping when
/// `latest` names a newer request.
pub fn stream_log(repo: PathBuf, seq: u64, latest: Arc<AtomicU64>, tx: Sender<Batch>) {
    thread::spawn(move || {
        let repo = Repo::at(repo);
        let mut rows = Vec::with_capacity(BATCH);
        let mut alive = true;
        let walked = history::walk(&repo, |hash, commit, changes| {
            if latest.load(Ordering::Relaxed) != seq {
                alive = false;
                return false;
            }
            rows.push(Logged {
                hash: hash.to_string(),
                time: commit.time,
                message: commit.message.clone(),
                files: commit.files.len(),
                changes,
            });
            if rows.len() >= BATCH {
                let batch = std::mem::take(&mut rows);
                alive = tx
                    .send(Batch::Log {
                        seq,
                        rows: batch,
                        done: false,
                        failed: None,
                    })
                    .is_ok();
            }
            alive
        });
        // A history that cannot be read ends where it broke, saying why,
        // rather than leaving the pane loading for ever.
        if alive {
            let _ = tx.send(Batch::Log {
                seq,
                rows,
                done: true,
                failed: walked.err().map(|e| crate::errors::words(&e)),
            });
        }
    });
}

/// `stowe status`, as rows: staged, then not staged, then untracked.
pub fn stream_status(repo: PathBuf, seq: u64, tx: Sender<Batch>) {
    thread::spawn(move || {
        let rows = status_rows(&Repo::at(repo)).map_err(|e| crate::errors::words(&e));
        let _ = tx.send(Batch::Status { seq, rows });
    });
}

fn status_rows(repo: &Repo) -> anyhow::Result<Vec<StatusRow>> {
    let head = repo.head_manifest()?;
    let working = scan::scan(repo, &head, false)?;
    let base = repo.read_index()?.unwrap_or_else(|| head.clone());
    let staged = scan::diff(&head, &base);
    let unstaged = scan::diff(&base, &working);
    let mut rows: Vec<StatusRow> = history::changes(&staged)
        .into_iter()
        .map(|change| StatusRow {
            side: Side::Staged,
            change,
        })
        .collect();
    rows.extend(history::changes(&unstaged).into_iter().map(|change| {
        let side = if change.kind == Kind::Added {
            Side::Untracked
        } else {
            Side::Unstaged
        };
        StatusRow { side, change }
    }));
    Ok(rows)
}

/// What pushing HEAD to remote `name` would do, worked out from the snapshot
/// last pushed there and HEAD's, never from the remote itself.
pub fn stream_plan(repo: PathBuf, name: String, seq: u64, tx: Sender<Batch>) {
    thread::spawn(move || {
        let rows = plan(&Repo::at(repo), &name).map_err(|e| crate::errors::words(&e));
        let _ = tx.send(Batch::Plan { seq, rows });
    });
}

fn plan(repo: &Repo, name: &str) -> anyhow::Result<Vec<Change>> {
    let head = repo.head_manifest()?;
    let last = match repo.remote_head(name)? {
        None => Vec::new(),
        Some(h) => match repo.read_commit(&h) {
            Ok(c) => c.files,
            Err(_) => anyhow::bail!(
                "`{name}` was last pushed a commit this repo does not have, so what a push \
                 would change cannot be worked out here"
            ),
        },
    };
    Ok(history::changes(&scan::diff(&last, &head)))
}

/// Look at every remote in `cfg`, one thread each, since a dead mount can make
/// one look take seconds.
pub fn probe_remotes(repo: PathBuf, cfg: &Config, tx: Sender<Batch>) {
    for (name, url) in &cfg.remotes {
        let (name, url) = (name.clone(), url.clone());
        let mountable = cfg.mounts.contains_key(&name);
        let (repo, tx) = (repo.clone(), tx.clone());
        thread::spawn(move || {
            let reach = reach(&Repo::at(repo), &name, &url, mountable);
            let _ = tx.send(Batch::Reach { name, reach });
        });
    }
}

/// A folder being there proves nothing for a remote pushed to before, since an
/// unmounted drive leaves its mountpoint behind: only its `.stowe` marker does.
fn reach(repo: &Repo, name: &str, url: &str, mountable: bool) -> Reach {
    let Some(root) = mirror::local_root(url) else {
        return Reach::Cloud;
    };
    let known = repo.remote_head(name).ok().flatten().is_some();
    let there = if known {
        mirror::detect_format(&root) != mirror::Format::Empty
    } else if mountable {
        // An unmounted drive leaves its mountpoint behind, empty, and an empty
        // drive just mounted reads the same, so only something in it says here.
        fs_err::read_dir(&root).is_ok_and(|mut d| d.next().is_some())
    } else {
        crate::remote::remote_reachable(url)
    };
    match (there, mountable) {
        (true, _) => Reach::Here,
        (false, true) => Reach::Mountable,
        (false, false) => Reach::Unplugged,
    }
}

/// Every repo's state under `irepos`, all at once, each arriving on its own.
pub fn stream_repos(paths: Vec<PathBuf>, base: PathBuf, seq: u64, tx: Sender<Batch>) {
    for path in paths {
        let (base, tx) = (base.clone(), tx.clone());
        thread::spawn(move || {
            let state = repos::state(&path, &base).map_err(|e| crate::errors::words(&e));
            let _ = tx.send(Batch::Repo { seq, path, state });
        });
    }
}
