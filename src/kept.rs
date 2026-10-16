//! Copies of small text files, kept in `.stowe/objects/` as they are staged,
//! so `istatus` can show what changed inside one with no remote at hand.
//! Anything larger, or not UTF-8, is still known here only by its hash.

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::model::Entry;
use crate::repo::Repo;

/// The largest file a copy is kept of.
pub const MAX: u64 = 1_000_000;

/// Whether `bytes` read as text: UTF-8 with no NUL in it.
pub fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

fn object(repo: &Repo, hash: &str) -> PathBuf {
    repo.dir.join("objects").join(&hash[..2]).join(&hash[2..])
}

/// The kept copy of the content `hash` names, if there is one.
pub fn read(repo: &Repo, hash: &str) -> Option<String> {
    String::from_utf8(fs_err::read(object(repo, hash)).ok()?).ok()
}

/// Whether a copy of the content `hash` names is kept, without reading it.
pub fn has(repo: &Repo, hash: &str) -> bool {
    object(repo, hash).exists()
}

/// Keep a copy of every small text file in `staged` whose content `before`
/// did not already hold. A file that changed since it was hashed is skipped,
/// so a copy is always exactly the content its name says.
pub fn keep(repo: &Repo, staged: &[Entry], before: &[Entry]) -> Result<()> {
    let had: HashSet<&str> = before.iter().map(|e| e.hash.as_str()).collect();
    // One file per content, since two threads writing one object collide.
    let mut new: HashMap<&str, &Entry> = HashMap::new();
    for e in staged {
        if e.size <= MAX && !had.contains(e.hash.as_str()) {
            new.entry(&e.hash).or_insert(e);
        }
    }
    new.into_par_iter().try_for_each(|(_, e)| keep_one(repo, e))
}

fn keep_one(repo: &Repo, e: &Entry) -> Result<()> {
    let dest = object(repo, &e.hash);
    if dest.exists() {
        return Ok(());
    }
    let Ok(bytes) = fs_err::read(repo.root.join(&e.path)) else {
        return Ok(());
    };
    if !is_text(&bytes) || blake3::hash(&bytes).to_hex().as_str() != e.hash {
        return Ok(());
    }
    let dir = dest.parent().unwrap_or(&repo.dir);
    let tmp = dir.join(format!(".{}.{}.part", &e.hash[2..], std::process::id()));
    let saved = fs_err::create_dir_all(dir)
        .and_then(|()| fs_err::write(&tmp, &bytes))
        .and_then(|()| fs_err::rename(&tmp, &dest));
    saved.with_context(|| format!("keeping a copy of `{}` in `.stowe/objects/`", e.path))
}
