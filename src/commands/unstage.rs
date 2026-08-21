//! `stowe unstage`: drop the staging index, leaving the working tree alone.

use anyhow::Result;

use crate::repo::Repo;

pub fn run() -> Result<()> {
    let repo = Repo::find()?;
    match repo.read_index()? {
        None => println!("nothing staged."),
        Some(staged) => {
            repo.clear_index()?;
            println!(
                "unstaged {} file(s); working tree left as-is.",
                staged.len()
            );
        }
    }
    Ok(())
}
