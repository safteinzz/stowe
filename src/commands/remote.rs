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
    /// Add or update a named remote  <NAME> <URL>
    ///   stowe remote add origin local:/path
    #[command(verbatim_doc_comment)]
    Add {
        /// What to call it here, `origin` unless you have more than one
        name: String,
        /// Where it lives: a path, `local:/path`, or `s3://bucket/prefix`
        url: String,
        /// On-disk format: `mirror` (playable, local only) or `backup` (blobs).
        /// Omit to use the scheme default (local → mirror, s3 → backup).
        #[arg(long, value_parser = ["mirror", "backup"])]
        format: Option<String>,
        /// Shell command that makes this remote available (mount the drive,
        /// bring up an sshfs). Run before every use, so it must do nothing when
        /// already mounted. May be an inline command or a path to a script.
        /// Local remotes only.
        #[arg(long)]
        mount: Option<String>,
    },
    /// List configured remotes
    List,
    /// Forget a remote: its config here, never anything on it  <NAME>
    Remove {
        /// The remote to forget
        name: String,
    },
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
            crate::repo::valid_remote_name(&name)?;
            let url = absolute_url(&url)?;
            check(&url, format.as_deref(), mount.as_deref())?;
            // If it's a local drive that isn't plugged in, confirm before saving
            // (you may be adding it ahead of connecting, so default is yes).
            // With a mount command we already know how to reach it, so no prompt.
            if !remote_reachable(&url) && mount.is_none() {
                let shown = crate::paths::short_url(&url);
                if !confirm_default_yes(&format!(
                    "remote `{name}` ({shown}) isn't reachable right now. Add it anyway?"
                )) {
                    outln!("aborted - remote not added.");
                    return Ok(());
                }
            }
            // Read before saving, so a ref that cannot be read leaves nothing saved.
            let theirs = crate::remote::read_head(&url)?;
            save(&repo, &name, &url, format.as_deref(), mount.as_deref())?;
            let cfg = repo.config()?;
            outln!(
                "remote `{name}` -> {} ({} format)",
                crate::paths::short_url(&url),
                remote_format(&cfg, &name, &url).name()
            );
            if let Some(cmd) = &mount {
                outln!("  mount: {cmd}");
            }
            if let Some(theirs) = theirs {
                repo.set_remote_head(&name, &theirs)?;
                let history = crate::repos::hashes(&repo)?;
                let stands = if history.is_empty() {
                    format!("`stowe pull {name}` brings it here")
                } else {
                    crate::repos::behind(&repo, &name, &history).words()
                };
                outln!("  holds commit {}: {stands}", crate::model::short(&theirs));
            }
        }
        Some(RemoteCmd::Remove { name }) => {
            remove(&repo, &name)?;
            outln!("forgot remote `{name}`; nothing on it was touched");
        }
        // Bare `stowe remote` and `stowe remote list` both just list.
        None | Some(RemoteCmd::List) => {
            let cfg = repo.config()?;
            if cfg.remotes.is_empty() {
                outln!("no remotes. add one, e.g.:");
                outln!("  stowe remote add origin local:/path/to/backup");
            }
            for (name, url) in &cfg.remotes {
                outln!(
                    "{name}\t{}\t[{}]",
                    crate::paths::short_url(url),
                    remote_format(&cfg, name, url).name()
                );
                if let Some(cmd) = cfg.mounts.get(name) {
                    outln!("  mount: {cmd}");
                }
            }
        }
    }
    Ok(())
}

/// Refuse a format or a mount command that `url` cannot have.
pub fn check(url: &str, format: Option<&str>, mount: Option<&str>) -> Result<()> {
    if format == Some("mirror") && mirror::local_root(url).is_none() {
        bail!("`mirror` format needs a local path, and {url} is not one");
    }
    // A mount command only means something for a path we have to make exist.
    // An s3 store is reachable on credentials, not on mounting.
    if mount.is_some() && mirror::local_root(url).is_none() {
        bail!("--mount only applies to local remotes; {url} has nothing to mount");
    }
    Ok(())
}

/// Write remote `name` to the config: its URL, its format (None for the
/// scheme's default) and its mount command. Asks nothing.
pub fn save(
    repo: &Repo,
    name: &str,
    url: &str,
    format: Option<&str>,
    mount: Option<&str>,
) -> Result<()> {
    crate::repo::valid_remote_name(name)?;
    check(url, format, mount)?;
    let mut cfg = repo.config()?;
    match format {
        Some(fmt) => cfg.formats.insert(name.to_string(), fmt.to_string()),
        None => cfg.formats.remove(name),
    };
    match mount {
        Some(cmd) => cfg.mounts.insert(name.to_string(), cmd.to_string()),
        None => cfg.mounts.remove(name),
    };
    // A record of the old location would have push refuse the new one as a
    // drive gone missing.
    let place = |u: &str| mirror::local_root(u).map_or(u.into(), std::path::PathBuf::from);
    if cfg
        .remotes
        .insert(name.to_string(), url.to_string())
        .is_some_and(|old| place(&old) != place(url))
    {
        repo.forget_remote_head(name)?;
    }
    repo.save_config(&cfg)
}

/// Forget remote `name` here: its URL, format, mount command and the record of
/// what was pushed to it. Nothing on the remote is touched.
pub fn remove(repo: &Repo, name: &str) -> Result<()> {
    let mut cfg = repo.config()?;
    if cfg.remotes.remove(name).is_none() {
        bail!("no remote named `{name}`: `stowe remote` lists them");
    }
    cfg.formats.remove(name);
    cfg.mounts.remove(name);
    repo.save_config(&cfg)?;
    repo.forget_remote_head(name)
}

/// Call remote `old` `new`, keeping the record of what was pushed to it.
pub fn rename(repo: &Repo, old: &str, new: &str) -> Result<()> {
    crate::repo::valid_remote_name(new)?;
    let mut cfg = repo.config()?;
    if cfg.remotes.contains_key(new) {
        bail!("a remote named `{new}` already exists, so pick another name");
    }
    let Some(url) = cfg.remotes.remove(old) else {
        bail!("no remote named `{old}`: `stowe remote` lists them");
    };
    cfg.remotes.insert(new.to_string(), url);
    if let Some(f) = cfg.formats.remove(old) {
        cfg.formats.insert(new.to_string(), f);
    }
    if let Some(m) = cfg.mounts.remove(old) {
        cfg.mounts.insert(new.to_string(), m);
    }
    repo.save_config(&cfg)?;
    if let Some(head) = repo.remote_head(old)? {
        repo.set_remote_head(new, &head)?;
    }
    repo.forget_remote_head(old)
}

/// A local remote typed as a relative path, made absolute against the current
/// directory, which is where the person typing it meant; stored relative, it
/// would name a different place from every other directory. `..` is resolved
/// by name, since the drive may not be mounted yet.
pub fn absolute_url(url: &str) -> Result<String> {
    let Some(root) = mirror::local_root(url) else {
        return Ok(url.to_string());
    };
    if root.is_absolute() {
        return Ok(url.to_string());
    }
    let mut abs = std::env::current_dir()?;
    for part in root.components() {
        match part {
            std::path::Component::ParentDir => {
                abs.pop();
            }
            std::path::Component::CurDir => {}
            other => abs.push(other),
        }
    }
    let prefix = if url.starts_with("local:") {
        "local:"
    } else {
        ""
    };
    Ok(format!("{prefix}{}", abs.display()))
}
