//! `stowe restore`: recover committed files from a remote.

use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::Entry;
use crate::model::short;
use crate::remote::ensure_reachable;
use crate::remote::remote_format;
use crate::remote::remote_url;
use crate::repo::Repo;
use crate::{mirror, remote, scan};

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
            let lexical = if arg.is_absolute() {
                arg.clone()
            } else {
                cwd.join(arg)
            };
            // The file may be gone (we're restoring a deletion), so fall back to
            // resolving its parent and re-appending the name.
            let abs = match lexical.canonicalize() {
                Ok(c) => c,
                Err(_) => {
                    let parent = lexical.parent().unwrap_or_else(|| Path::new("."));
                    let name = lexical
                        .file_name()
                        .ok_or_else(|| anyhow!("bad path: {}", arg.display()))?;
                    parent
                        .canonicalize()
                        .with_context(|| format!("no such path: {}", arg.display()))?
                        .join(name)
                }
            };
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
    let mirror_root =
        match remote_format(&repo.config()?, remote_name, &url) {
            mirror::Format::Mirror => Some(mirror::local_root(&url).ok_or_else(|| {
                anyhow!("remote `{remote_name}` is set to mirror but isn't local")
            })?),
            _ => None,
        };
    let backend = match &mirror_root {
        Some(_) => None,
        None => Some(remote::open(&url)?),
    };

    let mut restored = 0usize;
    let mut skipped = 0usize;
    for e in &targets {
        let dest = repo.root.join(&e.path);
        // Already the wanted content? Leave it (and don't re-fetch).
        if dest.exists() && scan::hash_file(&dest)? == e.hash {
            skipped += 1;
            continue;
        }
        let got = match &mirror_root {
            Some(root) => mirror::fetch(root, &e.hash, &dest)?,
            None => {
                let backend = backend.as_ref().unwrap();
                let key = remote::object_key(&e.hash);
                if backend.exists(&key)? {
                    backend.get_file(&key, &dest)?;
                    true
                } else {
                    false
                }
            }
        };
        if !got {
            bail!(
                "content for `{}` (commit {}) isn't on remote `{remote_name}` - was it pushed?",
                e.path,
                short(&chash)
            );
        }
        restored += 1;
        println!("restored {}", e.path);
    }

    println!(
        "\n{restored} file(s) restored from {}, {skipped} already current.",
        short(&chash)
    );
    Ok(())
}
