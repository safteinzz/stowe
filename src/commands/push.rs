//! `stowe push`: bring one or more remotes up to the current commit.

use anyhow::{Result, anyhow, bail};
use std::collections::{HashMap, HashSet};

use crate::model::{count, short};
use crate::names::preflight_names;
use crate::remote::ensure_reachable;
use crate::remote::remote_format;
use crate::remote::remote_url;
use crate::repo::Repo;
use crate::{mirror, remote, scan};

pub fn run(remotes: &[String], force: bool) -> Result<()> {
    let repo = Repo::find()?;
    if repo.head()?.is_none() {
        bail!("nothing committed yet - `stowe commit` first");
    }

    // Resolve all targets up front so a bad name fails before any work. Each
    // remote is dispatched by its configured format (mirror vs object store).
    let targets = if remotes.is_empty() {
        vec!["origin".to_string()]
    } else {
        remotes.to_vec()
    };
    let cfg = repo.config()?;
    let mut resolved = Vec::new();
    for name in &targets {
        resolved.push((name.clone(), remote_url(&repo, name)?));
    }
    // Bail before any upload if a target drive isn't connected (mounting it
    // first, if the remote knows how).
    for (name, url) in &resolved {
        ensure_reachable(&repo, &cfg, name, url)?;
    }

    for (name, url) in &resolved {
        // A mirror writes real files; make sure the target drive can store
        // every committed name (and offer the fix if not). This may create a
        // rename commit, which the push then carries.
        if let Some(root) = mirror_root(&cfg, name, url)? {
            preflight_names(&repo, name, &root)?;
        }
        let report = match push_one(&repo, name, url, force) {
            Ok(report) => report,
            Err(e) => {
                if let Some(d) = e.downcast_ref::<mirror::Drifted>() {
                    d.drift.report();
                }
                return Err(e);
            }
        };
        print_report(&report);
    }
    Ok(())
}

/// What one push did, for whoever shows it.
pub struct PushReport {
    pub name: String,
    pub head: String,
    pub pushed: Pushed,
}

pub enum Pushed {
    Mirror(mirror::SyncReport),
    Backup { objects: usize, commits: usize },
}

impl PushReport {
    /// The one line that sums the push up.
    pub fn summary(&self) -> String {
        let name = &self.name;
        let head = short(&self.head);
        match &self.pushed {
            Pushed::Mirror(r) => format!(
                "mirrored to `{name}`: +{} new, ~{} changed, ⇄{} moved, -{} removed, \
                 {} -> {head}",
                r.added,
                r.modified,
                r.moved,
                r.removed,
                count(r.new_commits, "new commit", "new commits")
            ),
            Pushed::Backup { objects, commits } => format!(
                "pushed to `{name}`: {}, {}, refs/main -> {head}",
                count(*objects, "new object", "new objects"),
                count(*commits, "new commit", "new commits")
            ),
        }
    }
}

fn print_report(report: &PushReport) {
    if let Pushed::Mirror(r) = &report.pushed {
        for p in &r.cleared {
            eprintln!("removed from mirror: {}", crate::names::display(p));
        }
    }
    outln!("{}", report.summary());
}

/// The folder a remote is mirrored into, or None for a backup.
pub(crate) fn mirror_root(
    cfg: &crate::model::Config,
    name: &str,
    url: &str,
) -> Result<Option<std::path::PathBuf>> {
    match remote_format(cfg, name, url) {
        mirror::Format::Mirror => mirror::local_root(url).map(Some).ok_or_else(|| {
            anyhow!("remote `{name}` is set to mirror but {url} isn't a local path")
        }),
        _ => Ok(None),
    }
}

/// Push HEAD to one remote, which the caller has already made reachable and
/// checked the names for. Prints nothing and asks nothing, so a TUI can run it
/// on a thread: a mirror with drift fails with [`mirror::Drifted`].
pub fn push_one(repo: &Repo, name: &str, url: &str, force: bool) -> Result<PushReport> {
    let cfg = repo.config()?;
    let pushed = match mirror_root(&cfg, name, url)? {
        Some(root) => Pushed::Mirror(mirror::sync(repo, &root, force)?),
        None => {
            let (objects, commits) = push_objects(repo, name, url, force)?;
            Pushed::Backup { objects, commits }
        }
    };
    let head = repo.head()?.unwrap_or_default();
    // Remember we've written here: a later push must then find this remote's
    // marker, or refuse rather than recreate its folder.
    repo.set_remote_head(name, &head)?;
    Ok(PushReport {
        name: name.to_string(),
        head,
        pushed,
    })
}

/// Push to an object-store (non-local) remote: content-addressed blobs + history.
/// `force` replaces a history the remote has that this repo doesn't. Returns how
/// many objects and commits were new there.
pub(crate) fn push_objects(
    repo: &Repo,
    name: &str,
    url: &str,
    force: bool,
) -> Result<(usize, usize)> {
    let history = repo.history()?;
    let head = history[0].0.clone();
    let head_commit = &history[0].1;

    // stowe keeps no local object store, so an object's bytes are read from the
    // working tree. Index the tree by *content hash* rather than path, so a file
    // renamed since the commit is still found (its content lives under the new
    // name). Hash-only + cached, so this scan is cheap.
    let working = scan::scan(repo, &repo.head_manifest()?, false)?;
    let mut by_hash: HashMap<&str, &str> = HashMap::new();
    for e in &working {
        by_hash.entry(&e.hash).or_insert(&e.path);
    }

    let mut seen = HashSet::new();
    let mut to_upload = Vec::new();
    for e in &head_commit.files {
        if !seen.insert(e.hash.as_str()) {
            continue;
        }
        match by_hash.get(e.hash.as_str()) {
            Some(rel) => to_upload.push((remote::object_key(&e.hash), repo.root.join(rel))),
            None => bail!(
                "content for `{}` is no longer in the working tree (modified or deleted \
                 since the commit) - restore it or commit the change before pushing",
                e.path
            ),
        }
    }

    let backend = remote::open(url)?;
    if !force && backend.exists("refs/main")? {
        let theirs = String::from_utf8(backend.get_bytes("refs/main")?)?;
        let theirs = theirs.trim();
        if !history.iter().any(|(h, _)| h == theirs) {
            let message = format!(
                "remote `{name}` is at commit {}, which this repo doesn't have: `stowe pull \
                 {name}` first, or re-run with --force to replace its history with this one",
                short(theirs)
            );
            return Err(crate::mirror::Diverged::new(theirs, message).into());
        }
    }
    let new_objects = backend.put_files(to_upload)?;

    let mut new_commits = 0;
    for (hash, _) in &history {
        let key = format!("commits/{hash}.json");
        if !backend.exists(&key)? {
            let bytes = fs_err::read(repo.dir.join("commits").join(format!("{hash}.json")))?;
            backend.put_bytes(&key, &bytes)?;
            new_commits += 1;
        }
    }
    backend.put_bytes("refs/main", head.as_bytes())?;
    Ok((new_objects, new_commits))
}
