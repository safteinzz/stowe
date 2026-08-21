//! `stowe add`: stage paths (or the whole tree) into the index.

use anyhow::{Context, Result, anyhow, bail};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::model::under_prefix;
use crate::model::{Entry, Manifest};
use crate::names::warn_unportable;
use crate::repo::Repo;
use crate::{ignore, scan};

pub fn run(paths: Vec<PathBuf>, all: bool) -> Result<()> {
    let repo = Repo::find()?;
    let head = repo.head_manifest()?;

    // `-A`: stage a fresh snapshot of the whole tree (fingerprinting audio).
    if all {
        let current = scan::scan(&repo, &head, true)?;
        let d = scan::diff(&head, &current);
        if d.is_empty() {
            println!("nothing to stage; working tree matches the last commit.");
            return Ok(());
        }
        repo.write_index(&current)?;
        println!("staged snapshot of {} files.", current.len());
        scan::print_diff(&d);
        warn_unportable(&d);
        return Ok(());
    }

    if paths.is_empty() {
        bail!("specify files/directories to stage, or `-A` to stage everything");
    }

    // Per-path staging. Start from what's already staged (or HEAD if nothing is)
    // and upsert / remove just the named paths, keyed by repo-relative path.
    let mut index: BTreeMap<String, Entry> = repo
        .read_index()?
        .unwrap_or_else(|| head.clone())
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();

    let root = repo.root.canonicalize()?;
    let cwd = std::env::current_dir()?;
    let mut staged = 0usize;
    let mut removed = 0usize;

    for arg in &paths {
        let lexical = if arg.is_absolute() {
            arg.clone()
        } else {
            cwd.join(arg)
        };
        // Resolve to an absolute path inside the repo. `canonicalize` handles
        // existing paths; for a path that was deleted, resolve via its parent.
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
        if abs.strip_prefix(&root).is_err() {
            bail!("{} is outside the repo", arg.display());
        }

        if abs.is_dir() {
            // Stage every file under the directory (in parallel)...
            let entries: Vec<Entry> = scan::files_under(&root, &abs, &ignore::Ignore::load(&root))?
                .par_iter()
                .map(|f| scan::entry_for(&root, f, true))
                .collect::<Result<_>>()?;
            let present: HashSet<String> = entries.iter().map(|e| e.path.clone()).collect();
            for e in entries {
                index.insert(e.path.clone(), e);
                staged += 1;
            }
            // ...and stage the removal of files that used to be under it but are gone.
            let gone: Vec<String> = index
                .keys()
                .filter(|p| under_prefix(p, &rel) && !present.contains(*p))
                .cloned()
                .collect();
            for p in gone {
                index.remove(&p);
                removed += 1;
            }
        } else if abs.is_file() {
            let e = scan::entry_for(&root, &abs, true)?;
            index.insert(rel, e);
            staged += 1;
        } else if index.remove(&rel).is_some() {
            // Path is gone from disk → stage its deletion.
            removed += 1;
        } else {
            bail!(
                "no such path, and nothing staged to remove: {}",
                arg.display()
            );
        }
    }

    let manifest: Manifest = index.into_values().collect();
    repo.write_index(&manifest)?;

    let mut summary = format!("staged {staged} file(s)");
    if removed > 0 {
        summary += &format!(", {removed} removal(s)");
    }
    println!("{summary}");
    let d = scan::diff(&head, &manifest);
    scan::print_diff(&d);
    warn_unportable(&d);
    Ok(())
}
