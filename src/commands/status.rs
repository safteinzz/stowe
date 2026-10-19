//! `stowe status`: what changed since the last commit, without hashing audio.

use anyhow::Result;
use colored::Colorize;

use crate::repo::Repo;
use crate::repos::Behind;
use crate::{repos, scan};

/// `short` prints `status -s`, whose form `--help` fixes for scripts.
pub fn run(short: bool) -> Result<()> {
    let repo = Repo::find()?;
    let head = repo.head_manifest()?;
    // `status` is a quick "what changed?" - hash only, no audio decoding.
    let working = scan::scan(&repo, &head, false)?;
    let history = repos::hashes(&repo)?;
    let remotes: Vec<_> = repo
        .config()?
        .remotes
        .keys()
        .map(|name| (name.clone(), repos::behind(&repo, name, &history)))
        .collect();
    if short {
        let d = scan::diff(&head, &working);
        let counts = [
            ('+', d.added.len()),
            ('-', d.removed.len()),
            ('~', d.modified.len()),
            ('⇄', d.moved.len()),
            ('↑', repos::unpushed(&remotes, history.len())),
            ('⇡', repos::elsewhere(&remotes, history.len())),
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
    print_remotes(&remotes);
    scan::print_status(&staged, &unstaged, &summary);
    Ok(())
}

/// One line for each remote that lacks something, and what catches it up.
fn print_remotes(remotes: &[(String, Behind)]) {
    let mut any = false;
    for (name, b) in remotes {
        let fix = match b {
            Behind::By(0) => continue,
            Behind::By(_) | Behind::Never => format!("`stowe push {name}`"),
            Behind::Elsewhere => {
                format!("`stowe pull {name}`, or `stowe push {name} --force` over it")
            }
        };
        outln!("{} {}: {fix}", name.bold(), b.words().yellow());
        any = true;
    }
    if any {
        outln!();
    }
}
