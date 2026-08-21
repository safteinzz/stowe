//! `stowe log`: the linear history, newest first.

use anyhow::Result;

use crate::repo::Repo;
use crate::time::format_time;

pub fn run() -> Result<()> {
    let repo = Repo::find()?;
    let history = repo.history()?;
    if history.is_empty() {
        println!("no commits yet.");
        return Ok(());
    }
    for (hash, c) in history {
        println!("commit {hash}");
        println!("Date:  {}", format_time(c.time));
        println!("Files: {}", c.files.len());
        println!("\n    {}\n", c.message);
    }
    Ok(())
}
