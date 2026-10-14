//! `stowe log`: the linear history, newest first.

use anyhow::Result;

use crate::repo::Repo;
use crate::time::format_time;

pub fn run() -> Result<()> {
    let repo = Repo::find()?;
    let history = repo.history()?;
    if history.is_empty() {
        outln!("no commits yet.");
        return Ok(());
    }
    for (hash, c) in history {
        outln!("commit {hash}");
        outln!("Date:  {}", format_time(c.time));
        outln!("Files: {}", c.files.len());
        outln!("\n    {}\n", c.message);
    }
    Ok(())
}
