//! `stowe pull`: rebuild the working tree from a remote.

use anyhow::{Result, anyhow, bail};

use crate::model::Commit;
use crate::model::short;
use crate::remote::ensure_reachable;
use crate::remote::remote_format;
use crate::remote::remote_url;
use crate::repo::Repo;
use crate::{mirror, remote, scan};

pub fn run(name: &str) -> Result<()> {
    let repo = Repo::find()?;
    let url = remote_url(&repo, name)?;
    ensure_reachable(&repo, &repo.config()?, name, &url)?;

    // A mirror remote is pulled by rebuilding from its real files.
    if remote_format(&repo.config()?, name, &url) == mirror::Format::Mirror {
        let root = mirror::local_root(&url)
            .ok_or_else(|| anyhow!("remote `{name}` is set to mirror but {url} isn't local"))?;
        let r = mirror::pull(&repo, &root)?;
        println!(
            "pulled from `{name}`: now at {} ({} new commits, {} files written)",
            short(&r.head),
            r.new_commits,
            r.written
        );
        return Ok(());
    }

    let backend = remote::open(&url)?;
    if !backend.exists("refs/main")? {
        bail!("remote `{name}` is empty - nothing to pull");
    }
    let remote_head = String::from_utf8(backend.get_bytes("refs/main")?)?
        .trim()
        .to_string();

    // Download the commit chain (metadata) we don't already have.
    let mut new_commits = 0;
    let mut cur = Some(remote_head.clone());
    while let Some(hash) = cur {
        let local = repo.dir.join("commits").join(format!("{hash}.json"));
        let bytes = if local.exists() {
            std::fs::read(&local)?
        } else {
            let b = backend.get_bytes(&format!("commits/{hash}.json"))?;
            std::fs::write(&local, &b)?;
            new_commits += 1;
            b
        };
        let commit: Commit = serde_json::from_slice(&bytes)?;
        cur = commit.parent;
    }
    repo.set_head(&remote_head)?;

    // Reconstruct the working tree for the remote's latest snapshot.
    let commit = repo.read_commit(&remote_head)?;
    let mut written = 0;
    for e in &commit.files {
        let dest = repo.root.join(&e.path);
        let need = !dest.exists() || scan::hash_file(&dest)? != e.hash;
        if need {
            backend.get_file(&remote::object_key(&e.hash), &dest)?;
            written += 1;
        }
    }
    repo.clear_index()?;

    println!(
        "pulled from `{name}`: now at {} ({new_commits} new commits, {written} files written)",
        short(&remote_head)
    );
    Ok(())
}
