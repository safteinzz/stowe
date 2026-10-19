//! `stowe restore`: recover committed files from a remote.

use anyhow::{Result, anyhow, bail};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::model::Entry;
use crate::model::{count, short};
use crate::remote::Source;
use crate::remote::ensure_reachable;
use crate::remote::remote_url;
use crate::repo::Repo;
use crate::scan;

pub fn run(paths: Vec<PathBuf>, all: bool, from: Option<&str>, remote_name: &str) -> Result<()> {
    let repo = Repo::find()?;
    let history = repo.history()?;
    if history.is_empty() {
        bail!("nothing committed yet - nothing to restore");
    }

    // Resolve the target commit: HEAD by default, else the one commit whose
    // hash starts with `--from` (a unique prefix is enough).
    let (chash, commit) = match from {
        None => history[0].clone(),
        Some(prefix) => {
            let mut it = history.iter().filter(|(h, _)| h.starts_with(prefix));
            match (it.next(), it.next()) {
                (None, _) => bail!("no commit matches `{prefix}` (see `stowe log`)"),
                (Some(one), None) => one.clone(),
                (Some(_), Some(_)) => bail!("`{prefix}` is ambiguous - give more characters"),
            }
        }
    };
    let manifest = commit.files;
    let by_path: BTreeMap<&str, &Entry> = manifest.iter().map(|e| (e.path.as_str(), e)).collect();

    // Which entries to restore: everything in the commit, or the named paths.
    let targets: Vec<&Entry> = if all {
        manifest.iter().collect()
    } else {
        if paths.is_empty() {
            bail!("specify files to restore, or `-A` for the whole snapshot");
        }
        let root = repo.root.canonicalize()?;
        let cwd = std::env::current_dir()?;
        let mut out = Vec::new();
        for arg in &paths {
            let abs = scan::resolve_arg(&root, &cwd, arg)?;
            let rel = scan::rel_path(&root, &abs);
            let e = by_path.get(rel.as_str()).ok_or_else(|| {
                anyhow!(
                    "`{rel}` isn't in commit {} - nothing to restore",
                    short(&chash)
                )
            })?;
            out.push(*e);
        }
        out
    };

    // Bytes come from the remote - a playable mirror (real files + preserved
    // versions) or an object store. stowe keeps no local copies, so restoring
    // never doubles your disk.
    let url = remote_url(&repo, remote_name)?;
    ensure_reachable(&repo, &repo.config()?, remote_name, &url)?;
    let done = restore(&repo, remote_name, &url, &chash, &targets, |p| {
        outln!("restored {p}")
    })?;
    outln!("\n{}", done.summary());
    Ok(())
}

/// What a restore wrote, and how many files already held the wanted bytes.
pub struct Restored {
    pub restored: Vec<String>,
    pub skipped: usize,
    pub from: String,
}

impl Restored {
    pub fn summary(&self) -> String {
        format!(
            "{} restored from {}, {} already current.",
            count(self.restored.len(), "file", "files"),
            short(&self.from),
            self.skipped
        )
    }
}

/// Write `targets`, as commit `chash` recorded them, from a remote the caller
/// has already made reachable, handing `wrote` each path once it is on disk,
/// so a failure partway still says which files it left behind.
pub fn restore(
    repo: &Repo,
    remote_name: &str,
    url: &str,
    chash: &str,
    targets: &[&Entry],
    mut wrote: impl FnMut(&str),
) -> Result<Restored> {
    let source = Source::open(repo, remote_name, url)?;
    let mut restored = Vec::new();
    let mut skipped = 0usize;
    for e in targets {
        let dest = repo.root.join(&e.path);
        // Already the wanted content? Leave it (and don't re-fetch).
        if dest.exists() && scan::hash_file(&dest)? == e.hash {
            skipped += 1;
            continue;
        }
        let got = source.fetch(&e.path, &e.hash, &dest)?;
        if !got {
            bail!(
                "content for `{}` (commit {}) isn't on remote `{remote_name}` - it was never \
                 pushed there, or was changed there outside stowe",
                e.path,
                short(chash)
            );
        }
        wrote(&e.path);
        restored.push(e.path.clone());
    }
    Ok(Restored {
        restored,
        skipped,
        from: chash.to_string(),
    })
}
