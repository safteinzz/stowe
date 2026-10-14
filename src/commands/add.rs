//! `stowe add`: stage paths (or the whole tree) into the index.

use anyhow::{Result, bail};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::model::under_prefix;
use crate::model::{Entry, Manifest, count};
use crate::names::warn_unportable;
use crate::repo::Repo;
use crate::{ignore, scan};

pub fn run(paths: Vec<PathBuf>, all: bool) -> Result<()> {
    let repo = Repo::find()?;
    let head = repo.head_manifest()?;

    // `-A`: stage a fresh snapshot of the whole tree (fingerprinting audio).
    if all {
        let Some((current, d)) = stage_all(&repo)? else {
            outln!("nothing to stage; working tree matches the last commit.");
            return Ok(());
        };
        outln!("staged snapshot of {} files.", current.len());
        scan::print_diff(&d);
        warn_unportable(&d);
        return Ok(());
    }

    if paths.is_empty() {
        bail!("specify files/directories to stage, or `-A` to stage everything");
    }

    let root = repo.root.canonicalize()?;
    let cwd = std::env::current_dir()?;
    let mut named = Vec::new();
    for arg in &paths {
        let abs = scan::resolve_arg(&root, &cwd, arg)?;
        let rel = scan::rel_path(&root, &abs);
        named.push((abs, rel, arg.display().to_string()));
    }

    let (staged, removed, manifest) = stage_paths(&repo, &root, &named)?;
    let mut summary = format!("staged {}", count(staged, "file", "files"));
    if removed > 0 {
        summary += &format!(", {}", count(removed, "removal", "removals"));
    }
    outln!("{summary}");
    let d = scan::diff(&head, &manifest);
    scan::print_diff(&d);
    warn_unportable(&d);
    Ok(())
}

/// `-A`: stage a fresh snapshot of the whole tree, fingerprinting audio. None
/// when the tree matches the last commit, so nothing was staged.
pub fn stage_all(repo: &Repo) -> Result<Option<(Manifest, scan::Diff)>> {
    let head = repo.head_manifest()?;
    let current = scan::scan(repo, &head, true)?;
    let d = scan::diff(&head, &current);
    if d.is_empty() {
        return Ok(None);
    }
    repo.write_index(&current)?;
    Ok(Some((current, d)))
}

/// Stage each `(abs, rel, as typed)` path: a file, a folder, or a path gone from
/// disk, which stages its removal. `root` is the repo root, canonical. Starts
/// from what is already staged (or HEAD), and returns how many files were
/// staged, how many removals, and the index as written.
pub fn stage_paths(
    repo: &Repo,
    root: &Path,
    named: &[(PathBuf, String, String)],
) -> Result<(usize, usize, Manifest)> {
    let head = repo.head_manifest()?;
    let mut index: BTreeMap<String, Entry> = repo
        .read_index()?
        .unwrap_or_else(|| head.clone())
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();

    let ignore = ignore::Ignore::load(root).keeping(index.keys().map(String::as_str));
    let mut staged = 0usize;
    let mut removed = 0usize;

    for (abs, rel, typed) in named {
        if abs.is_dir() {
            // Stage every file under the directory (in parallel)...
            let entries: Vec<Entry> = scan::files_under(root, abs, &ignore)?
                .par_iter()
                .map(|f| scan::entry_for(root, f, true))
                .collect::<Result<_>>()?;
            let present: HashSet<String> = entries.iter().map(|e| e.path.clone()).collect();
            for e in entries {
                index.insert(e.path.clone(), e);
                staged += 1;
            }
            // ...and stage the removal of files that used to be under it but are gone.
            let gone: Vec<String> = index
                .keys()
                .filter(|p| under_prefix(p, rel) && !present.contains(*p))
                .cloned()
                .collect();
            for p in gone {
                index.remove(&p);
                removed += 1;
            }
        } else if abs.is_file() {
            let e = scan::entry_for(root, abs, true)?;
            index.insert(rel.clone(), e);
            staged += 1;
        } else if index.remove(rel).is_some() {
            // Path is gone from disk → stage its deletion.
            removed += 1;
        } else {
            bail!("no such path, and nothing staged to remove: {typed}");
        }
    }

    let manifest: Manifest = index.into_values().collect();
    repo.write_index(&manifest)?;
    Ok((staged, removed, manifest))
}
