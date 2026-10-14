//! `stowe adapt`: adopt changes made on a mirror by hand.

use anyhow::{Result, anyhow, bail};
use std::path::PathBuf;

use crate::mirror;
use crate::remote::ensure_reachable;
use crate::remote::remote_url;
use crate::repo::Repo;

pub fn run(name: &str) -> Result<()> {
    let repo = Repo::find()?;
    let url = remote_url(&repo, name)?;
    ensure_reachable(&repo, &repo.config()?, name, &url)?;
    let r = match adapt(&repo, name, &url) {
        Ok(r) => r,
        Err(e) => {
            if let Some(c) = e.downcast_ref::<mirror::Conflicts>() {
                eprintln!("changed both here and on the mirror:");
                for p in &c.paths {
                    eprintln!("  {}", crate::names::display(p));
                }
            }
            return Err(e);
        }
    };
    if r.is_empty() {
        outln!("already in sync with `{name}` - nothing to adapt.");
        return Ok(());
    }
    outln!(
        "{}\nreview with `stowe status`, then `stowe add -A && stowe commit` to record.",
        summary(name, &r)
    );
    Ok(())
}

/// Adapt from a remote the caller has already made reachable. Prints nothing:
/// files changed on both sides fail with [`mirror::Conflicts`].
pub fn adapt(repo: &Repo, name: &str, url: &str) -> Result<mirror::AdaptReport> {
    mirror::adapt(repo, &mirror_root(name, url)?)
}

/// [`adapt`] in two halves: what it would do, then
/// [`mirror::apply_adapt`] against the root this returns.
pub fn plan(repo: &Repo, name: &str, url: &str) -> Result<(PathBuf, mirror::AdaptPlan)> {
    let root = mirror_root(name, url)?;
    let plan = mirror::plan_adapt(repo, &root)?;
    Ok((root, plan))
}

fn mirror_root(name: &str, url: &str) -> Result<PathBuf> {
    let root = mirror::local_root(url).ok_or_else(|| {
        anyhow!("`stowe adapt` only works on mirror (local:) remotes - `{name}` is {url}")
    })?;
    if mirror::detect_format(&root) != mirror::Format::Mirror {
        bail!("remote `{name}` isn't a mirror - nothing to adapt from");
    }
    Ok(root)
}

/// The line that sums an adapt up.
pub fn summary(name: &str, r: &mirror::AdaptReport) -> String {
    format!(
        "adapted from `{name}`: +{} new, ~{} changed, ⇄{} moved, -{} removed in the working tree.",
        r.added, r.modified, r.moved, r.removed
    )
}
