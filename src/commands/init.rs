//! `stowe init`: create the `.stowe/` that makes a folder a repo.

use anyhow::Result;

use crate::repo::Repo;

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;
    Repo::init(&cwd)?;
    println!(
        "initialized empty stowe repo in {}/.stowe",
        crate::paths::short(&cwd)
    );
    Ok(())
}
