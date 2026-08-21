//! `stowe convert`: flip a remote between mirror and backup in place.

use anyhow::{Result, anyhow, bail};

use crate::mirror;
use crate::remote::ensure_reachable;
use crate::remote::remote_url;
use crate::repo::Repo;

pub fn run(name: &str, to: Option<&str>) -> Result<()> {
    let repo = Repo::find()?;
    let url = remote_url(&repo, name)?;
    ensure_reachable(&repo, &repo.config()?, name, &url)?;
    let root = mirror::local_root(&url).ok_or_else(|| {
        anyhow!("only local remotes can be a playable mirror - `{name}` is {url}")
    })?;

    let current = mirror::detect_format(&root);
    if current == mirror::Format::Empty {
        bail!("remote `{name}` is empty - push to it first, then convert");
    }

    // Default target = flip to the other format.
    let target = match to {
        Some("mirror") => mirror::Format::Mirror,
        Some("backup") => mirror::Format::Backup,
        _ => match current {
            mirror::Format::Mirror => mirror::Format::Backup,
            _ => mirror::Format::Mirror,
        },
    };
    if current == target {
        println!("remote `{name}` is already a {}.", target.name());
        return Ok(());
    }

    let r = match target {
        mirror::Format::Mirror => mirror::backup_to_mirror(&root)?,
        mirror::Format::Backup => mirror::mirror_to_backup(&root)?,
        mirror::Format::Empty => unreachable!(),
    };

    // Persist the new format so the next `push` keeps it (otherwise the
    // scheme default - mirror for local - would flip it back).
    let mut cfg = repo.config()?;
    cfg.formats
        .insert(name.to_string(), target.name().to_string());
    repo.save_config(&cfg)?;

    println!(
        "converted `{name}` to {}: {} files, {} preserved version(s).",
        target.name(),
        r.files,
        r.preserved
    );
    Ok(())
}

// --- helpers ----------------------------------------------------------------
