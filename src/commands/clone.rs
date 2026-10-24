//! `stowe clone`: make a new folder a repo holding what a remote holds.

use anyhow::{Result, anyhow, bail};
use std::path::PathBuf;

use crate::mirror::{self, Format};
use crate::remote::Source;
use crate::repo::Repo;

/// What the remote a clone came from is called in the new repo.
const ORIGIN: &str = "origin";

pub fn run(url: &str, path: Option<PathBuf>, mount: Option<&str>) -> Result<()> {
    let url = super::remote::absolute_url(url)?;
    super::remote::check(&url, None, mount)?;
    let dest = std::env::current_dir()?.join(match path {
        Some(p) => p,
        None => default_dir(&url)?,
    });
    let shown = crate::paths::short(&dest);
    if mirror::local_root(&url).is_some_and(|root| root == dest) {
        bail!("{shown} is the remote itself: name another folder, `stowe clone <URL> <PATH>`");
    }
    let made = !dest.exists();
    if !made && fs_err::read_dir(&dest)?.next().is_some() {
        bail!(
            "{shown} already holds files: clone into a new or empty folder, or run `stowe init` \
             there, `stowe remote add origin <URL>` and `stowe pull`"
        );
    }
    fs_err::create_dir_all(&dest)?;
    let repo = Repo::init(&dest)?;
    if let Err(e) = connect(&repo, &url, mount) {
        undo(&repo, made);
        return Err(e);
    }
    super::pull::pull(&repo, ORIGIN, &url).map_err(|e| {
        e.context(format!(
            "the clone stopped partway (`stowe pull` in {shown} carries on from here)"
        ))
    })?;
    outln!("cloned {} into {shown}", crate::paths::short_url(&url));
    Ok(())
}

/// Save `url` as `origin`, make it reachable and check it holds a commit,
/// writing nothing outside `repo`'s `.stowe/`. A local remote's format is read
/// off the drive, since a backup there is as likely as a mirror.
fn connect(repo: &Repo, url: &str, mount: Option<&str>) -> Result<()> {
    super::remote::save(repo, ORIGIN, url, None, mount)?;
    crate::remote::ensure_reachable(repo, &repo.config()?, ORIGIN, url)?;
    if let Some(root) = mirror::local_root(url) {
        match mirror::detect_format(&root) {
            Format::Mirror => {}
            Format::Backup => super::remote::save(repo, ORIGIN, url, Some("backup"), mount)?,
            Format::Empty => bail!(
                "{} holds nothing pushed by stowe: is the drive mounted, and is that the folder \
                 you pushed to?",
                crate::paths::short(&root)
            ),
        }
    }
    if Source::open(repo, ORIGIN, url)?.head()?.is_none() {
        bail!(
            "{} holds no commit yet, so there is nothing to clone",
            crate::paths::short_url(url)
        );
    }
    Ok(())
}

/// Take back what a clone made before it pulled anything: the `.stowe/`, and
/// the folder itself when the clone created it.
fn undo(repo: &Repo, made: bool) {
    let _ = fs_err::remove_dir_all(&repo.dir);
    if made {
        let _ = fs_err::remove_dir(&repo.root);
    }
}

/// The folder a clone of `url` lands in when none is named: the last part of
/// its path, so `~/drive/Music` lands in `Music`.
fn default_dir(url: &str) -> Result<PathBuf> {
    let last = match mirror::local_root(url) {
        Some(root) => root.file_name().map(PathBuf::from),
        None => url
            .split_once("://")
            .and_then(|(_, rest)| rest.trim_end_matches('/').rsplit('/').next())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
    };
    last.ok_or_else(|| anyhow!("{url} names no folder to clone into: `stowe clone {url} <PATH>`"))
}
