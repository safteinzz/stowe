//! `stowe status`: what changed since the last commit, without hashing audio.

use anyhow::Result;

use crate::repo::Repo;
use crate::scan;

pub fn run() -> Result<()> {
    let repo = Repo::find()?;
    let head = repo.head_manifest()?;
    // `status` is a quick "what changed?" - hash only, no audio decoding.
    let working = scan::scan(&repo, &head, false)?;
    // The staging baseline is the index if anything's staged, else HEAD.
    let base = repo.read_index()?.unwrap_or_else(|| head.clone());

    let staged = scan::diff(&head, &base); // Changes to be committed
    let unstaged = scan::diff(&base, &working); // not staged + untracked (its .added)
    let summary = scan::diff(&head, &working); // net change, for the summary line
    scan::print_status(&staged, &unstaged, &summary);
    Ok(())
}
