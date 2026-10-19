//! The work a screen starts: pushes, adapts, converts, restores and staging,
//! each on a thread with its progress in the footer, and the boxes that stand
//! in front of the ones that can lose something.
//!
//! A remote with a mount command has it run first with the terminal handed
//! over, since a mount may ask for a password, and the job only starts once
//! the screen is back.

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::style::Color;
use std::path::PathBuf;
use std::thread;

use super::App;
use crate::commands::{adapt, convert, push, restore};
use crate::mirror;
use crate::model::{Entry, count};
use crate::names;
use crate::remote::{MountFailed, Unseen, remote_url, run_mount, verify_reachable};
use crate::repo::Repo;
use crate::tui::suspended;
use crate::tui::widgets::{Modal, Query};

/// Lines of the program's own words a failure box shows.
const WORDS: usize = 5;
/// Paths a box lists before saying how many more there are.
const LISTED: usize = 8;

/// What a job is doing, which the footer says while it runs.
#[derive(Clone)]
pub(super) enum Job {
    Push {
        name: String,
        force: bool,
        rename: bool,
    },
    /// `approved` is what the gate in front of it listed as lost, which the
    /// adapt goes ahead with only while it is still exactly that.
    Adapt {
        name: String,
        approved: Vec<String>,
    },
    Convert {
        name: String,
        to: mirror::Format,
    },
    Restore {
        remote: String,
        commit: String,
        entry: Entry,
    },
    Stage(Vec<String>),
    /// Put a status row's paths back as the last commit has them, the bytes
    /// from `remote` when no copy of them is here.
    Discard {
        what: Discard,
        paths: Vec<String>,
        remote: Option<String>,
    },
}

impl Job {
    pub(super) fn doing(&self) -> String {
        match self {
            Job::Push { name, .. } => format!("pushing to {name}"),
            Job::Adapt { name, .. } => format!("adapting from {name}"),
            Job::Convert { name, to } => format!("converting {name} to a {}", to.name()),
            Job::Restore { entry, .. } => format!("restoring {}", entry.path),
            Job::Stage(_) => "staging".to_string(),
            Job::Discard { paths, .. } => format!("putting back {}", paths[0]),
        }
    }

    /// The remote it reads or writes, whose mount command runs first.
    fn remote(&self) -> Option<&str> {
        match self {
            Job::Push { name, .. } | Job::Adapt { name, .. } | Job::Convert { name, .. } => {
                Some(name)
            }
            Job::Restore { remote, .. } => Some(remote),
            Job::Discard { remote, .. } => remote.as_deref(),
            Job::Stage(_) => None,
        }
    }

    fn failed(&self) -> String {
        match self {
            Job::Push { name, .. } => format!("could not push to {name}"),
            Job::Adapt { name, .. } => format!("could not adapt from {name}"),
            Job::Convert { name, .. } => format!("could not convert {name}"),
            Job::Restore { entry, .. } => format!("could not restore {}", entry.path),
            Job::Stage(_) => "could not stage".to_string(),
            Job::Discard { what, paths, .. } => match what {
                Discard::Untracked => format!("could not delete {}", leaf(&paths[0])),
                _ => format!("could not delete the changes to {}", leaf(&paths[0])),
            },
        }
    }
}

/// What `d` on a status row takes away, which decides what its gate says.
#[derive(Clone)]
pub(super) enum Discard {
    /// The file is on no commit and not staged, so it goes from the disk.
    Untracked,
    /// The last commit has no file at that path, so the file itself goes.
    Added,
    /// It moves back to this path, every change with it.
    Moved(String),
    /// Every change since the last commit goes, staged or not.
    Changed,
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A job's end, sent back to the drawing loop.
pub(super) struct Outcome {
    job: Job,
    result: Result<String>,
}

/// The error a push stops with before writing anything when the drive cannot
/// store some committed names, which the screen offers to rename.
#[derive(Debug)]
struct Unportable(Vec<String>);

impl std::fmt::Display for Unportable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} cannot be stored there",
            count(self.0.len(), "committed name", "committed names")
        )
    }
}

impl std::error::Error for Unportable {}

/// The error an adapt stops with before touching anything when it would
/// delete or overwrite files here, which the screen asks about first.
#[derive(Debug)]
struct Loses(Vec<String>);

impl std::fmt::Display for Loses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} would go from the working tree",
            count(self.0.len(), "file", "files")
        )
    }
}

impl std::error::Error for Loses {}

/// A box waiting on an answer.
pub(super) struct Confirm {
    pub(super) target: Target,
    pub(super) yes: bool,
    pub(super) typed: Query,
}

pub(super) enum Target {
    /// Forget a remote here; nothing on it is touched.
    Remove(String),
    /// Push over what changed on a mirror by hand.
    Force { name: String, drift: Vec<String> },
    /// Adapt from a mirror that lost files by hand, taking them from here too.
    Adapt { name: String, loses: Vec<String> },
    /// Push over a remote at commit `theirs`, which this repo does not have.
    Replace { name: String, theirs: String },
    /// Rename the names a drive cannot store, then push, with `force` kept
    /// from the push that found them.
    Rename {
        name: String,
        offenders: Vec<String>,
        force: bool,
    },
    /// Push to a mirror where it deletes or replaces `loses`.
    Push { name: String, loses: Vec<String> },
    /// Push into an empty folder never pushed to, shown as `shown`.
    Unseen { name: String, shown: String },
    /// Flip a remote to the other format.
    Convert { name: String, to: mirror::Format },
    /// Put a file back as a commit had it, over the one in the working tree.
    /// `lost` when that one exists nowhere else, which takes the typed gate.
    Restore {
        remote: String,
        commit: String,
        entry: Entry,
        lost: Option<Lost>,
    },
    /// Throw away a status row's changes, the gate in front of `Job::Discard`.
    Discard {
        what: Discard,
        paths: Vec<String>,
        remote: Option<String>,
    },
}

/// Why the file a restore replaces exists nowhere else.
#[derive(Clone, Copy)]
pub(super) enum Lost {
    /// It holds changes no commit has.
    Uncommitted,
    /// It is as the last commit has it, and no remote has that version.
    Unpushed,
}

impl Confirm {
    pub(super) fn new(target: Target) -> Confirm {
        Confirm {
            yes: matches!(target, Target::Rename { .. }),
            typed: Query::default(),
            target,
        }
    }
}

impl Target {
    pub(super) fn typed(&self) -> bool {
        match self {
            Target::Force { .. } | Target::Replace { .. } | Target::Adapt { .. } => true,
            Target::Restore { lost, .. } => lost.is_some(),
            Target::Discard { .. } => true,
            _ => false,
        }
    }

    pub(super) fn title(&self) -> String {
        match self {
            Target::Remove(_) => "forget this remote?".to_string(),
            Target::Force { name, .. } => format!("overwrite what changed on {name}?"),
            Target::Replace { name, .. } => format!("replace the history on {name}?"),
            Target::Adapt { name, .. } => format!("adapt from {name}?"),
            Target::Rename { name, .. } => format!("rename what {name} cannot store?"),
            Target::Unseen { .. } => "push into an empty folder?".to_string(),
            Target::Push { name, .. } => format!("delete or replace on {name}?"),
            Target::Convert { name, to } => format!("convert {name} to a {}?", to.name()),
            Target::Restore { .. } => "put back this version?".to_string(),
            Target::Discard {
                what: Discard::Untracked,
                ..
            } => "delete this file?".to_string(),
            Target::Discard { .. } => "delete its changes?".to_string(),
        }
    }

    /// What the box names, and what a typed gate wants typed.
    pub(super) fn name(&self) -> String {
        match self {
            Target::Remove(name)
            | Target::Force { name, .. }
            | Target::Replace { name, .. }
            | Target::Adapt { name, .. }
            | Target::Rename { name, .. }
            | Target::Unseen { name, .. }
            | Target::Push { name, .. }
            | Target::Convert { name, .. } => name.clone(),
            Target::Restore { entry, lost, .. } => {
                if lost.is_some() {
                    leaf(&entry.path).to_string()
                } else {
                    entry.path.clone()
                }
            }
            Target::Discard { paths, .. } => leaf(&paths[0]).to_string(),
        }
    }

    pub(super) fn note(&self) -> String {
        match self {
            Target::Remove(_) => {
                "only this repo forgets it and what was pushed there; nothing on it is touched"
                    .to_string()
            }
            Target::Force { drift, .. } => format!(
                "these changed there outside stowe, and pushing anyway makes it match this commit:\n{}",
                listed(drift)
            ),
            Target::Replace { name, theirs } => format!(
                "{name} is at commit {}, which this repo doesn't have, and pushing anyway replaces \
                 that history with this one\n`stowe pull {name}` instead brings it here",
                crate::model::short(theirs)
            ),
            Target::Adapt { loses, .. } => format!(
                "these went from the mirror by hand, and adapting makes the working tree match, \
                 where they may be the last copy:\n{}",
                listed(loses)
            ),
            Target::Rename { offenders, .. } => format!(
                "renamed here to names it can store, and recorded as a commit:\n{}",
                listed(offenders)
            ),
            Target::Push { name, loses } => format!(
                "this push deletes or replaces these on {name}, keeping the old versions in its \
                 .stowe/objects for `stowe restore --from`:\n{}",
                listed(loses)
            ),
            Target::Unseen { shown, .. } => format!(
                "{shown} is an empty folder stowe has never pushed to\nif its drive is not \
                 mounted there, everything lands on the disk that holds the folder: `e` gives it \
                 a mount command"
            ),
            Target::Convert { to, .. } => match to {
                mirror::Format::Backup => {
                    "its files become deduped blobs in place; convert back any time".to_string()
                }
                _ => "its blobs become real files in place; convert back any time".to_string(),
            },
            Target::Restore {
                entry,
                commit,
                lost,
                ..
            } => {
                let at = crate::model::short(commit);
                let path = names::display(&entry.path);
                match lost {
                    Some(Lost::Uncommitted) => format!(
                        "{path} has changes no commit holds, and they go\nthe version from {at} replaces it"
                    ),
                    Some(Lost::Unpushed) => format!(
                        "{path} is as the last commit has it, and no remote has that version yet, so it goes for good\nthe version from {at} replaces it"
                    ),
                    None => format!("the file there now is replaced by the version from {at}"),
                }
            }
            Target::Discard { what, paths, .. } => {
                let path = names::display(&paths[0]);
                let lost = match what {
                    Discard::Untracked => {
                        "untracked, so stowe has no copy: it goes from the disk".to_string()
                    }
                    Discard::Added => {
                        "added since the last commit, so the file itself goes".to_string()
                    }
                    Discard::Moved(from) => format!(
                        "every change goes and it moves back to {}",
                        names::display(from)
                    ),
                    Discard::Changed => {
                        "every change since the last commit goes, staged or not".to_string()
                    }
                };
                format!("{path}\n{lost}")
            }
        }
    }

    pub(super) fn verb(&self) -> &'static str {
        match self {
            Target::Force { .. }
            | Target::Replace { .. }
            | Target::Unseen { .. }
            | Target::Push { .. } => "push",
            Target::Adapt { .. } => "adapt",
            Target::Discard { .. } => "del",
            _ => "restore",
        }
    }

    pub(super) fn colour(&self) -> Color {
        match self {
            Target::Rename { .. } => Color::Cyan,
            _ => Color::Red,
        }
    }
}

/// What a push to mirror `name` deletes or replaces there, from the commit it
/// is known to be at and HEAD. Nothing for a backup, which only ever adds, and
/// nothing when that commit is not here, since the push stops for that itself.
fn push_loses(repo: &Repo, name: &str) -> Result<Vec<String>> {
    let cfg = repo.config()?;
    let url = remote_url(repo, name)?;
    if push::mirror_root(&cfg, name, &url)?.is_none() {
        return Ok(Vec::new());
    }
    let Some(last) = repo
        .remote_head(name)?
        .and_then(|h| repo.read_commit(&h).ok())
    else {
        return Ok(Vec::new());
    };
    let d = crate::scan::diff(&last.files, &repo.head_manifest()?);
    Ok(d.removed
        .iter()
        .map(|p| format!("deleted: {p}"))
        .chain(d.modified.iter().map(|p| format!("replaced: {p}")))
        .collect())
}

/// At most `LISTED` of `paths`, one a line, then how many more.
fn listed(paths: &[String]) -> String {
    let mut out: Vec<String> = paths
        .iter()
        .take(LISTED)
        .map(|p| names::display(p))
        .collect();
    if paths.len() > LISTED {
        out.push(format!("and {} more", paths.len() - LISTED));
    }
    out.join("\n")
}

impl App {
    /// Start `job`, unless one is already running: a remote is written by one
    /// job at a time. A remote's mount command runs here, in the terminal.
    pub(super) fn start(&mut self, job: Job, terminal: &mut DefaultTerminal) {
        if let Some(running) = &self.job {
            self.set_failed(format!("still {}", running.doing()));
            return;
        }
        let repo = Repo::at(self.repo.clone());
        let cfg = match repo.config() {
            Ok(cfg) => cfg,
            Err(e) => return self.alert(job.failed(), &e),
        };
        if let Some(name) = job.remote()
            && let Some(cmd) = cfg.mounts.get(name)
        {
            let (name, cmd) = (name.to_string(), cmd.clone());
            let mounted = suspended(terminal, self.enhanced, || run_mount(&name, &cmd));
            if let Err(e) = mounted {
                return self.mount_failed(job.failed(), &e);
            }
        }
        self.job = Some(job.clone());
        let tx = self.tx.clone();
        let root = self.repo.clone();
        thread::spawn(move || {
            let result = run(&Repo::at(root), &job);
            let _ = tx.send(super::load::Batch::Job(Outcome { job, result }));
        });
    }

    /// `m`: run the remote's mount command with the terminal handed over.
    pub(super) fn mount(&mut self, name: &str, terminal: &mut DefaultTerminal) {
        let repo = Repo::at(self.repo.clone());
        let cmd = match repo.config() {
            Ok(cfg) => cfg.mounts.get(name).cloned(),
            Err(e) => return self.alert(format!("could not mount {name}"), &e),
        };
        let Some(cmd) = cmd else {
            self.modal = Some(Modal::new(
                format!("{name} has no mount command"),
                format!(
                    "`e` on it adds one: the command that makes {name} available, such as an sshfs line."
                ),
            ));
            return;
        };
        match suspended(terminal, self.enhanced, || run_mount(name, &cmd)) {
            Ok(()) => self.set_status(format!("mounted {name}")),
            Err(e) => self.mount_failed(format!("could not mount {name}"), &e),
        }
        self.reread_remotes();
    }

    /// A job came back: say what it did, or open the box its failure needs.
    pub(super) fn finished(&mut self, outcome: Outcome) {
        self.job = None;
        let Outcome { job, result } = outcome;
        match result {
            Ok(line) => self.set_status(line),
            Err(e) => {
                if let (Job::Push { name, .. }, Some(d)) =
                    (&job, e.downcast_ref::<mirror::Drifted>())
                {
                    let mut drift: Vec<String> = d
                        .drift
                        .foreign
                        .iter()
                        .map(|p| format!("added there: {p}"))
                        .collect();
                    drift.extend(
                        d.drift
                            .missing
                            .iter()
                            .map(|p| format!("deleted there: {p}")),
                    );
                    drift.extend(d.drift.changed.iter().map(|p| format!("edited there: {p}")));
                    self.confirm = Some(Confirm::new(Target::Force {
                        name: name.clone(),
                        drift,
                    }));
                } else if let (Job::Adapt { name, .. }, Some(l)) = (&job, e.downcast_ref::<Loses>())
                {
                    self.confirm = Some(Confirm::new(Target::Adapt {
                        name: name.clone(),
                        loses: l.0.clone(),
                    }));
                } else if let (Job::Push { name, .. }, Some(d)) =
                    (&job, e.downcast_ref::<mirror::Diverged>())
                {
                    self.confirm = Some(Confirm::new(Target::Replace {
                        name: name.clone(),
                        theirs: d.theirs.clone(),
                    }));
                } else if let (Job::Push { name, force, .. }, Some(u)) =
                    (&job, e.downcast_ref::<Unportable>())
                {
                    self.confirm = Some(Confirm::new(Target::Rename {
                        name: name.clone(),
                        offenders: u.0.clone(),
                        force: *force,
                    }));
                } else if let (Job::Push { .. }, Some(u)) = (&job, e.downcast_ref::<Unseen>()) {
                    self.confirm = Some(Confirm::new(Target::Unseen {
                        name: u.name.clone(),
                        shown: u.shown.clone(),
                    }));
                } else if let Some(c) = e.downcast_ref::<mirror::Conflicts>() {
                    self.modal = Some(Modal::new(
                        job.failed(),
                        format!(
                            "{e}\n\nchanged both here and on the mirror:\n{}",
                            listed(&c.paths)
                        ),
                    ));
                } else if let Some(d) = e.downcast_ref::<mirror::Drifted>() {
                    let mut all = d.drift.missing.clone();
                    all.extend(d.drift.changed.iter().cloned());
                    self.modal = Some(Modal::new(
                        job.failed(),
                        format!("{e}\n\nchanged there outside stowe:\n{}", listed(&all)),
                    ));
                } else {
                    self.alert(job.failed(), &e);
                }
            }
        }
        self.after(&job);
    }

    /// Read again what a job may have changed.
    fn after(&mut self, job: &Job) {
        match job {
            Job::Push { .. } | Job::Convert { .. } => {
                self.reread_remotes();
                self.request_log();
            }
            Job::Adapt { .. } => self.reread_remotes(),
            Job::Restore { .. } | Job::Stage(_) | Job::Discard { .. } => {}
        }
        if self.level == super::Level::Status || matches!(job, Job::Adapt { .. }) {
            self.request_status();
        }
    }

    /// A failure that has to be read: our words as the title, then the
    /// error's own, cut to the lines that say what failed.
    /// A mount that did not succeed: a status line when it was cancelled, else
    /// the alert with the command's own words under ours, since the terminal
    /// it wrote them to is already covered by the screen again.
    fn mount_failed(&mut self, title: String, e: &anyhow::Error) {
        let Some(m) = e.downcast_ref::<MountFailed>() else {
            return self.alert(title, e);
        };
        if m.cancelled {
            return self.set_failed(m.to_string());
        }
        let mut body = m.to_string();
        if !m.said.is_empty() {
            body = format!("{body}\n\n{}", m.said.join("\n"));
        }
        self.note = None;
        self.modal = Some(Modal::new(title, body));
    }

    pub(super) fn alert(&mut self, title: impl Into<String>, e: &anyhow::Error) {
        let words: Vec<String> = crate::errors::words(e)
            .lines()
            .take(WORDS)
            .map(str::to_string)
            .collect();
        self.note = None;
        self.modal = Some(Modal::new(title, words.join("\n")));
    }

    /// `↵` on a remote.
    /// `↵`: push, behind a gate when it deletes or replaces files on a mirror.
    pub(super) fn ask_push(&mut self, name: &str, terminal: &mut DefaultTerminal) {
        let loses = push_loses(&Repo::at(self.repo.clone()), name).unwrap_or_default();
        if loses.is_empty() {
            self.start(
                Job::Push {
                    name: name.to_string(),
                    force: false,
                    rename: false,
                },
                terminal,
            );
        } else {
            self.confirm = Some(Confirm::new(Target::Push {
                name: name.to_string(),
                loses,
            }));
        }
    }

    /// A box's yes.
    pub(super) fn confirmed(&mut self, target: Target, terminal: &mut DefaultTerminal) {
        match target {
            Target::Remove(name) => self.forget_remote(&name),
            Target::Force { name, .. }
            | Target::Replace { name, .. }
            | Target::Unseen { name, .. } => self.start(
                Job::Push {
                    name,
                    force: true,
                    rename: false,
                },
                terminal,
            ),
            Target::Push { name, .. } => self.start(
                Job::Push {
                    name,
                    force: false,
                    rename: false,
                },
                terminal,
            ),
            Target::Rename { name, force, .. } => self.start(
                Job::Push {
                    name,
                    force,
                    rename: true,
                },
                terminal,
            ),
            Target::Convert { name, to } => self.start(Job::Convert { name, to }, terminal),
            Target::Adapt { name, loses } => self.start(
                Job::Adapt {
                    name,
                    approved: loses,
                },
                terminal,
            ),
            Target::Restore {
                remote,
                commit,
                entry,
                ..
            } => self.start(
                Job::Restore {
                    remote,
                    commit,
                    entry,
                },
                terminal,
            ),
            Target::Discard {
                what,
                paths,
                remote,
            } => self.start(
                Job::Discard {
                    what,
                    paths,
                    remote,
                },
                terminal,
            ),
        }
    }
}

/// The job itself, off the drawing loop. Prints nothing: its line comes back.
fn run(repo: &Repo, job: &Job) -> Result<String> {
    let cfg = repo.config()?;
    let reach = |name: &str| -> Result<String> {
        let url = remote_url(repo, name)?;
        verify_reachable(repo, &cfg, name, &url)?;
        Ok(url)
    };
    match job {
        Job::Push {
            name,
            force,
            rename,
        } => {
            if repo.head()?.is_none() {
                anyhow::bail!(
                    "nothing committed yet: `c` in `stowe istatus` makes the first commit"
                );
            }
            let url = reach(name)?;
            if !*force && crate::remote::unseen(repo, name, &url, cfg.mounts.contains_key(name)) {
                return Err(Unseen::new(name, &url).into());
            }
            let mut renamed = 0;
            if let Some(root) = push::mirror_root(&cfg, name, &url)? {
                let found = names::unstorable(repo, &root)?;
                if !found.clashes.is_empty() {
                    let pairs: Vec<String> =
                        found.clashes.iter().map(|g| g.join("  /  ")).collect();
                    anyhow::bail!(
                        "`{name}` ignores case, so it cannot hold both names of each pair; rename \
                         one, commit, and push again:\n{}",
                        listed(&pairs)
                    );
                }
                if !found.offenders.is_empty() {
                    if !*rename {
                        return Err(Unportable(found.offenders).into());
                    }
                    renamed = names::rename_unportable(repo, &found)?.0.len();
                }
            }
            let report = push::push_one(repo, name, &url, *force)?;
            let mut line = report.summary();
            if let push::Pushed::Mirror(r) = &report.pushed
                && !r.cleared.is_empty()
            {
                line = format!("{line}; dropped {} put there by hand", r.cleared.len());
            }
            if renamed > 0 {
                line = format!("renamed {renamed}, {line}");
            }
            Ok(line)
        }
        Job::Adapt { name, approved } => {
            let url = reach(name)?;
            let (root, plan) = adapt::plan(repo, name, &url)?;
            let loses = plan.loses();
            if !loses.is_empty() && loses != *approved {
                return Err(Loses(loses).into());
            }
            let r = mirror::apply_adapt(repo, &root, plan)?;
            Ok(if r.is_empty() {
                format!("already in sync with `{name}`: nothing to adapt")
            } else {
                adapt::summary(name, &r)
            })
        }
        Job::Convert { name, to } => {
            let url = reach(name)?;
            Ok(convert::convert(repo, name, &url, Some(to.name()))?.summary(name))
        }
        Job::Restore {
            remote,
            commit,
            entry,
        } => {
            let url = reach(remote)?;
            Ok(restore::restore(repo, remote, &url, commit, &[entry], |_| {})?.summary())
        }
        Job::Stage(rels) => {
            let root = repo.root.canonicalize()?;
            let named: Vec<(PathBuf, String, String)> = rels
                .iter()
                .map(|r| (root.join(r), r.clone(), r.clone()))
                .collect();
            let (staged, removed, _) = crate::commands::add::stage_paths(repo, &root, &named)?;
            Ok(match removed {
                0 => format!("staged {}", count(staged, "file", "files")),
                n => format!(
                    "staged {}, {}",
                    count(staged, "file", "files"),
                    count(n, "removal", "removals")
                ),
            })
        }
        Job::Discard {
            what,
            paths,
            remote,
        } => {
            let from = match remote {
                Some(name) => Some((name.as_str(), reach(name)?)),
                None => None,
            };
            super::status::discard(repo, paths, from)?;
            Ok(match what {
                Discard::Untracked => format!("deleted {}", leaf(&paths[0])),
                _ => format!("deleted the changes to {}", leaf(&paths[0])),
            })
        }
    }
}
