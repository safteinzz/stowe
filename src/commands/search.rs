//! `stowe search`: every commit that added, changed, moved or removed a file
//! whose name matches, newest first.

use anyhow::{Result, bail};
use colored::Colorize;

use crate::history;
use crate::model::short;
use crate::repo::Repo;
use crate::time::short_time;

pub fn run(pattern: &str) -> Result<()> {
    let repo = Repo::find()?;
    if pattern.trim().is_empty() {
        bail!("give part of a file name to look for, such as `stowe search beach`");
    }
    let mut found = 0usize;
    history::walk(&repo, |hash, commit, changes| {
        for c in changes.iter().filter(|c| c.matches(pattern)) {
            found += 1;
            let kind = format!("{:<8}", c.kind.word());
            let kind = match c.kind {
                history::Kind::Added => kind.green(),
                history::Kind::Modified => kind.yellow(),
                history::Kind::Moved => kind.blue(),
                history::Kind::Removed => kind.red(),
            };
            outln!(
                "{}  {}  {kind} {}  {}",
                short(hash).yellow(),
                short_time(commit.time).green(),
                c.shown(),
                commit.message.dimmed()
            );
        }
        true
    })?;
    if found == 0 {
        bail!("no commit touched a file whose name matches `{pattern}`");
    }
    Ok(())
}
