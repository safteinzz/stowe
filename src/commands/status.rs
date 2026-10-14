//! `stowe status`: what changed since the last commit, without hashing audio.

use anyhow::Result;

use crate::repo::Repo;
use crate::{repos, scan};

/// `short` prints `status -s`, whose form `--help` fixes for scripts.
pub fn run(short: bool) -> Result<()> {
    let repo = Repo::find()?;
    let head = repo.head_manifest()?;
    // `status` is a quick "what changed?" - hash only, no audio decoding.
    let working = scan::scan(&repo, &head, false)?;
    if short {
        let d = scan::diff(&head, &working);
        let history = repos::hashes(&repo)?;
        let remotes: Vec<_> = repo
            .config()?
            .remotes
            .keys()
            .map(|name| (name.clone(), repos::behind(&repo, name, &history)))
            .collect();
        let unpushed = repos::unpushed(&remotes, history.len());
        let counts = [
            ('+', d.added.len()),
            ('-', d.removed.len()),
            ('~', d.modified.len()),
            ('⇄', d.moved.len()),
            ('↑', unpushed),
        ];
        let line: Vec<String> = counts
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(mark, n)| format!("{mark}{n}"))
            .collect();
        outln!("{}", line.join(" "));
        return Ok(());
    }
    // The staging baseline is the index if anything's staged, else HEAD.
    let base = repo.read_index()?.unwrap_or_else(|| head.clone());

    let staged = scan::diff(&head, &base); // Changes to be committed
    let unstaged = scan::diff(&base, &working); // not staged + untracked (its .added)
    let summary = scan::diff(&head, &working); // net change, for the summary line
    scan::print_status(&staged, &unstaged, &summary);
    Ok(())
}
