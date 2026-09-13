//! `stowe pull`: rebuild the working tree from a remote.

use anyhow::{Result, bail};
use std::collections::HashMap;

use crate::model::Commit;
use crate::model::short;
use crate::remote::Source;
use crate::remote::ensure_reachable;
use crate::remote::remote_url;
use crate::repo::Repo;
use crate::scan;

pub fn run(name: &str) -> Result<()> {
    let repo = Repo::find()?;
    let url = remote_url(&repo, name)?;
    ensure_reachable(&repo, &repo.config()?, name, &url)?;
    let source = Source::open(&repo, name, &url)?;
    let Some(remote_head) = source.head()? else {
        bail!("remote `{name}` is empty - nothing to pull");
    };

    // Copy down the commit chain metadata we don't already have.
    let mut chain = Vec::new();
    let mut new_commits = 0;
    let mut cur = Some(remote_head.clone());
    while let Some(hash) = cur {
        let local = repo.dir.join("commits").join(format!("{hash}.json"));
        let bytes = if local.exists() {
            std::fs::read(&local)?
        } else {
            let b = source.commit_bytes(&hash)?;
            std::fs::write(&local, &b)?;
            new_commits += 1;
            b
        };
        let commit: Commit = serde_json::from_slice(&bytes)?;
        cur = commit.parent;
        chain.push(hash);
    }
    if let Some(local) = repo.head()?
        && !chain.contains(&local)
    {
        bail!(
            "this repo is at commit {}, which `{name}` doesn't have - `stowe push {name}` it \
             first, or pulling would drop it",
            short(&local)
        );
    }

    // A file pulled over must hold committed content, or the pull loses it.
    let files = repo.read_commit(&remote_head)?.files;
    let committed: HashMap<String, String> = repo
        .head_manifest()?
        .into_iter()
        .map(|e| (e.path, e.hash))
        .collect();
    let working: HashMap<String, String> = scan::scan(&repo, &repo.head_manifest()?, false)?
        .into_iter()
        .map(|e| (e.path, e.hash))
        .collect();
    let mut wanted = Vec::new();
    let mut conflicts = Vec::new();
    for e in &files {
        let dest = repo.root.join(&e.path);
        let current = match working.get(&e.path) {
            Some(h) => Some(h.clone()),
            None if dest.is_file() => Some(scan::hash_file(&dest)?),
            None if dest.exists() => Some(String::new()),
            None => None,
        };
        match current {
            Some(h) if h == e.hash => {}
            Some(h) if committed.get(&e.path) != Some(&h) => conflicts.push(&e.path),
            _ => wanted.push(e),
        }
    }
    if !conflicts.is_empty() {
        eprintln!("changed here and not committed:");
        for p in &conflicts {
            eprintln!("  {}", crate::names::display(p));
        }
        bail!("nothing pulled - commit those files, or move them aside, then pull again");
    }

    for e in &wanted {
        if !source.fetch(&e.path, &e.hash, &repo.root.join(&e.path))? {
            bail!(
                "content for `{}` (commit {}) isn't on remote `{name}` - it was deleted or \
                 changed there outside stowe",
                crate::names::display(&e.path),
                short(&remote_head)
            );
        }
    }
    repo.set_head(&remote_head)?;
    repo.clear_index()?;

    println!(
        "pulled from `{name}`: now at {} ({new_commits} new commits, {} files written)",
        short(&remote_head),
        wanted.len()
    );
    Ok(())
}
