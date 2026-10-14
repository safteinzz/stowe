//! `stowe unstage`: drop what is staged, or only some paths of it, leaving the
//! working tree alone.

use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::model::{Entry, Manifest, count, under_prefix};
use crate::repo::Repo;
use crate::scan;

pub fn run(paths: Vec<PathBuf>) -> Result<()> {
    let repo = Repo::find()?;
    if paths.is_empty() {
        match repo.read_index()? {
            None => outln!("nothing staged."),
            Some(staged) => {
                repo.clear_index()?;
                outln!(
                    "unstaged {}; working tree left as-is.",
                    count(staged.len(), "file", "files")
                );
            }
        }
        return Ok(());
    }
    let root = repo.root.canonicalize()?;
    let cwd = std::env::current_dir()?;
    let rels = paths
        .iter()
        .map(|p| Ok(scan::rel_path(&root, &scan::resolve_arg(&root, &cwd, p)?)))
        .collect::<Result<Vec<String>>>()?;
    let n = unstage_paths(&repo, &rels)?;
    if n == 0 {
        bail!("nothing staged under those paths");
    }
    outln!(
        "unstaged {}; working tree left as-is.",
        count(n, "file", "files")
    );
    Ok(())
}

/// Put every staged path at or under one of `rels` back the way the last
/// commit has it, and drop the index once nothing in it differs from that
/// commit. Returns how many paths changed.
pub fn unstage_paths(repo: &Repo, rels: &[String]) -> Result<usize> {
    let Some(index) = repo.read_index()? else {
        return Ok(0);
    };
    let head = repo.head_manifest()?;
    let wanted = |p: &str| rels.iter().any(|r| under_prefix(p, r));
    let mut out: BTreeMap<String, Entry> = index
        .iter()
        .filter(|e| !wanted(&e.path))
        .map(|e| (e.path.clone(), e.clone()))
        .collect();
    for e in head.iter().filter(|e| wanted(&e.path)) {
        out.insert(e.path.clone(), e.clone());
    }
    let out: Manifest = out.into_values().collect();
    let changed = scan::diff(&index, &out);
    let n =
        changed.added.len() + changed.removed.len() + changed.modified.len() + changed.moved.len();
    if scan::diff(&head, &out).is_empty() {
        repo.clear_index()?;
    } else {
        repo.write_index(&out)?;
    }
    Ok(n)
}
