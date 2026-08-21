//! `stowe adapt`: adopt changes made on a mirror by hand.

use anyhow::{Result, anyhow, bail};

use crate::mirror;
use crate::remote::ensure_reachable;
use crate::remote::remote_url;
use crate::repo::Repo;

pub fn run(name: &str) -> Result<()> {
    let repo = Repo::find()?;
    let url = remote_url(&repo, name)?;
    ensure_reachable(&repo, &repo.config()?, name, &url)?;
    let root = mirror::local_root(&url).ok_or_else(|| {
        anyhow!("`stowe adapt` only works on mirror (local:) remotes - `{name}` is {url}")
    })?;
    if mirror::detect_format(&root) != mirror::Format::Mirror {
        bail!("remote `{name}` isn't a mirror - nothing to adapt from");
    }

    let r = mirror::adapt(&repo, &root)?;
    if r.is_empty() {
        println!("already in sync with `{name}` - nothing to adapt.");
        return Ok(());
    }
    println!(
        "adapted from `{name}`: +{} new, ~{} changed, ⇄{} moved, -{} removed in the working tree.\n\
         review with `stowe status`, then `stowe add -A && stowe commit` to record.",
        r.added, r.modified, r.moved, r.removed
    );
    Ok(())
}
