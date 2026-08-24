//! `stowe remote`: list, add or update the named remotes.

use anyhow::{Result, bail};
use clap::Subcommand;

use crate::mirror;
use crate::prompt::confirm_default_yes;
use crate::remote::remote_format;
use crate::remote::remote_reachable;
use crate::repo::Repo;

#[derive(Subcommand)]
pub enum RemoteCmd {
    /// Add or update a named remote: `stowe remote add origin local:/path`.
    Add {
        name: String,
        url: String,
        /// On-disk format: `mirror` (playable, local only) or `backup` (blobs).
        /// Omit to use the scheme default (local → mirror, s3 → backup).
        #[arg(long, value_parser = ["mirror", "backup"])]
        format: Option<String>,
        /// Shell command that makes this remote available (mount the drive,
        /// bring up an sshfs). Run automatically when it isn't reachable. May be
        /// an inline command or a path to a script. Local remotes only.
        #[arg(long)]
        mount: Option<String>,
    },
    /// List configured remotes.
    List,
}

pub fn run(cmd: Option<RemoteCmd>) -> Result<()> {
    let repo = Repo::find()?;
    match cmd {
        Some(RemoteCmd::Add {
            name,
            url,
            format,
            mount,
        }) => {
            let mut cfg = repo.config()?;
            match &format {
                Some(fmt) => {
                    if fmt == "mirror" && mirror::local_root(&url).is_none() {
                        bail!("`mirror` format needs a local path - {url} can't be a mirror");
                    }
                    cfg.formats.insert(name.clone(), fmt.clone());
                }
                // No override → fall back to the scheme default.
                None => {
                    cfg.formats.remove(&name);
                }
            }
            // A mount command only means something for a path we have to make
            // exist. An s3 store is reachable on credentials, not on mounting.
            match &mount {
                Some(cmd) => {
                    if mirror::local_root(&url).is_none() {
                        bail!("--mount only applies to local remotes; {url} has nothing to mount");
                    }
                    cfg.mounts.insert(name.clone(), cmd.clone());
                }
                None => {
                    cfg.mounts.remove(&name);
                }
            }
            // If it's a local drive that isn't plugged in, confirm before saving
            // (you may be adding it ahead of connecting, so default is yes).
            // With a mount command we already know how to reach it, so no prompt.
            if !remote_reachable(&url) && mount.is_none() {
                let shown = crate::paths::short_url(&url);
                if !confirm_default_yes(&format!(
                    "remote `{name}` ({shown}) isn't reachable right now. Add it anyway?"
                )) {
                    println!("aborted - remote not added.");
                    return Ok(());
                }
            }
            cfg.remotes.insert(name.clone(), url.clone());
            repo.save_config(&cfg)?;
            println!(
                "remote `{name}` -> {} ({} format)",
                crate::paths::short_url(&url),
                remote_format(&cfg, &name, &url).name()
            );
            if let Some(cmd) = &mount {
                println!("  mount: {cmd}");
            }
        }
        // Bare `stowe remote` and `stowe remote list` both just list.
        None | Some(RemoteCmd::List) => {
            let cfg = repo.config()?;
            if cfg.remotes.is_empty() {
                println!("no remotes. add one, e.g.:");
                println!("  stowe remote add origin local:/path/to/backup");
            }
            for (name, url) in &cfg.remotes {
                println!(
                    "{name}\t{}\t[{}]",
                    crate::paths::short_url(url),
                    remote_format(&cfg, name, url).name()
                );
                if let Some(cmd) = cfg.mounts.get(name) {
                    println!("  mount: {cmd}");
                }
            }
        }
    }
    Ok(())
}
