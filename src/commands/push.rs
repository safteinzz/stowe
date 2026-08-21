//! `stowe push`: bring one or more remotes up to the current commit.

use anyhow::{Result, anyhow, bail};
use std::collections::{HashMap, HashSet};

use crate::model::short;
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
        match remote_format(&cfg, name, url) {
            mirror::Format::Mirror => {
                let root = mirror::local_root(url).ok_or_else(|| {
                    anyhow!("remote `{name}` is set to mirror but {url} isn't a local path")
                })?;
                // A mirror writes real file remotes; make sure the target drive
                // can store every committed name (and offer the fix if not).
                // This may create a rename commit, so HEAD is re-read after.
                preflight_names(&repo, name, &root)?;
                let r = mirror::sync(&repo, &root, force)?;
                let head = repo.head()?.unwrap_or_default();
                // Remember we've written here: a later push must then find this
                // remote's marker, or refuse rather than recreate its folder.
                repo.set_remote_head(name, &head)?;
                println!(
                    "mirrored to `{name}`: +{} new, ~{} changed, ⇄{} moved, -{} removed, \
                     {} new commits -> {}",
                    r.added,
                    r.modified,
                    r.moved,
                    r.removed,
                    r.new_commits,
                    short(&head)
                );
            }
            _ => push_objects(&repo, name, url)?,
        }
    }
    Ok(())
}

/// Push to an object-store (non-local) remote: content-addressed blobs + history.
pub(crate) fn push_objects(repo: &Repo, name: &str, url: &str) -> Result<()> {
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
    let new_objects = backend.put_files(to_upload)?;

    let mut new_commits = 0;
    for (hash, _) in &history {
        let key = format!("commits/{hash}.json");
        if !backend.exists(&key)? {
            let bytes = std::fs::read(repo.dir.join("commits").join(format!("{hash}.json")))?;
            backend.put_bytes(&key, &bytes)?;
            new_commits += 1;
        }
    }
    backend.put_bytes("refs/main", head.as_bytes())?;
    repo.set_remote_head(name, &head)?;

    println!(
        "pushed to `{name}`: {new_objects} new objects, {new_commits} new commits, refs/main -> {}",
        short(&head)
    );
    Ok(())
}
