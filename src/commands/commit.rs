//! `stowe commit`: record the staged snapshot as the new head.

use anyhow::{Result, anyhow, bail};

use crate::model::Commit;
use crate::model::short;
use crate::repo::Repo;
use crate::scan;
use crate::time::now;

pub fn run(message: &str) -> Result<()> {
    let repo = Repo::find()?;
    let staged = repo
        .read_index()?
        .ok_or_else(|| anyhow!("nothing staged - run `stowe add -A` first"))?;
    let head = repo.head_manifest()?;
    let d = scan::diff(&head, &staged);
    if d.is_empty() {
        bail!("staged snapshot is identical to the last commit; nothing to commit");
    }
    let commit = Commit {
        parent: repo.head()?,
        message: message.to_string(),
        time: now(),
        files: staged,
    };
    let hash = repo.write_commit(&commit)?;
    repo.set_head(&hash)?;
    repo.clear_index()?;
    println!("committed {} \"{message}\"", short(&hash));
    scan::print_diff(&d);
    println!("\n(remember: `stowe push` to back the file contents up to a remote)");
    Ok(())
}
