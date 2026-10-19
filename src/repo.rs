//! The local repository: locating `.stowe/`, reading/writing HEAD, config,
//! the staging index, and commit objects.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::model::{Commit, Config, Manifest};

/// A `.stowe/` repo together with the working tree that contains it.
pub struct Repo {
    /// Working-tree root (the directory holding `.stowe/`).
    pub root: PathBuf,
    /// The `.stowe/` directory itself.
    pub dir: PathBuf,
}

impl Repo {
    /// Walk up from the current directory until we find a `.stowe/`.
    pub fn find() -> Result<Repo> {
        Repo::find_from(&std::env::current_dir()?)
    }

    /// Walk up from `start` until we find a `.stowe/`.
    pub fn find_from(start: &Path) -> Result<Repo> {
        let mut cur = start.to_path_buf();
        loop {
            if cur.join(".stowe").is_dir() {
                return Ok(Repo::at(cur));
            }
            if !cur.pop() {
                bail!("not a stowe repo (no .stowe found here or in any parent)");
            }
        }
    }

    /// The repo whose working tree is `root`, which must already hold a `.stowe/`.
    pub fn at(root: PathBuf) -> Repo {
        Repo {
            dir: root.join(".stowe"),
            root,
        }
    }

    /// Create a fresh, empty repo at `path`.
    pub fn init(path: &Path) -> Result<Repo> {
        let dir = path.join(".stowe");
        if dir.exists() {
            bail!("`.stowe` already exists at {}", dir.display());
        }
        fs_err::create_dir_all(dir.join("commits"))?;
        fs_err::write(dir.join("HEAD"), b"")?;
        let repo = Repo {
            root: path.to_path_buf(),
            dir,
        };
        repo.save_config(&Config::default())?;
        Ok(repo)
    }

    // --- HEAD -------------------------------------------------------------

    /// The commit hash HEAD points at, or `None` if there are no commits yet.
    pub fn head(&self) -> Result<Option<String>> {
        let s = fs_err::read_to_string(self.dir.join("HEAD")).unwrap_or_default();
        let s = s.trim().to_string();
        Ok(if s.is_empty() { None } else { Some(s) })
    }

    pub fn set_head(&self, hash: &str) -> Result<()> {
        fs_err::write(self.dir.join("HEAD"), hash.as_bytes())?;
        Ok(())
    }

    // --- config -----------------------------------------------------------

    pub fn config(&self) -> Result<Config> {
        let p = self.dir.join("config");
        if !p.exists() {
            return Ok(Config::default());
        }
        Ok(serde_json::from_slice(&fs_err::read(p)?)?)
    }

    pub fn save_config(&self, cfg: &Config) -> Result<()> {
        fs_err::write(self.dir.join("config"), serde_json::to_vec_pretty(cfg)?)?;
        Ok(())
    }

    // --- commits ----------------------------------------------------------

    fn commit_path(&self, hash: &str) -> PathBuf {
        self.dir.join("commits").join(format!("{hash}.json"))
    }

    pub fn read_commit(&self, hash: &str) -> Result<Commit> {
        let p = self.commit_path(hash);
        let bytes = fs_err::read(&p).with_context(|| format!("missing commit {hash}"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Serialize a commit and store it under its own content hash. Returns the hash.
    pub fn write_commit(&self, commit: &Commit) -> Result<String> {
        let bytes = serde_json::to_vec_pretty(commit)?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        fs_err::write(self.commit_path(&hash), &bytes)?;
        Ok(hash)
    }

    /// HEAD's snapshot, or an empty manifest if there are no commits yet.
    pub fn head_manifest(&self) -> Result<Manifest> {
        match self.head()? {
            Some(h) => Ok(self.read_commit(&h)?.files),
            None => Ok(Vec::new()),
        }
    }

    /// Walk parent links from HEAD back to the root commit (newest first).
    pub fn history(&self) -> Result<Vec<(String, Commit)>> {
        let mut out = Vec::new();
        let mut cur = self.head()?;
        while let Some(hash) = cur {
            let commit = self.read_commit(&hash)?;
            cur = commit.parent.clone();
            out.push((hash, commit));
        }
        Ok(out)
    }

    // --- staging index ----------------------------------------------------

    fn index_path(&self) -> PathBuf {
        self.dir.join("index")
    }

    /// The staged snapshot, if anything is staged.
    pub fn read_index(&self) -> Result<Option<Manifest>> {
        let p = self.index_path();
        if !p.exists() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&fs_err::read(p)?)?))
    }

    pub fn write_index(&self, manifest: &Manifest) -> Result<()> {
        fs_err::write(self.index_path(), serde_json::to_vec_pretty(manifest)?)?;
        Ok(())
    }

    pub fn clear_index(&self) -> Result<()> {
        let p = self.index_path();
        if p.exists() {
            fs_err::remove_file(p)?;
        }
        Ok(())
    }

    // --- remote tracking --------------------------------------------------

    /// Where we remember the commit a remote was last known to be at (git's
    /// `refs/remotes/<name>/main`, roughly). A name is one file name there, so
    /// one that could step out of `remotes/` is refused.
    fn remote_ref(&self, name: &str) -> Result<PathBuf> {
        valid_remote_name(name)?;
        Ok(self.dir.join("remotes").join(name))
    }

    /// Forget every push to `name`, for a remote that now points somewhere new.
    pub fn forget_remote_head(&self, name: &str) -> Result<()> {
        match fs_err::remove_file(self.remote_ref(name)?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// The commit `name` was last known to be at: pushed or pulled from here, or
    /// read off the remote's own `refs/main` when nothing here said.
    ///
    /// Its *presence* is load-bearing beyond bookkeeping: it's how push knows
    /// this remote holds stowe data, and so may not silently recreate the
    /// remote's folder when the drive isn't mounted.
    pub fn remote_head(&self, name: &str) -> Result<Option<String>> {
        match fs_err::read_to_string(self.remote_ref(name)?) {
            Ok(s) => {
                let s = s.trim().to_string();
                Ok(if s.is_empty() { None } else { Some(s) })
            }
            Err(_) => Ok(None),
        }
    }

    /// Record `hash` as the commit `name` is at. It is written whole beside the
    /// record and moved over it, so a failed write never leaves it empty.
    pub fn set_remote_head(&self, name: &str, hash: &str) -> Result<()> {
        let p = self.remote_ref(name)?;
        let dir = self.dir.join("remotes");
        fs_err::create_dir_all(&dir)?;
        let tmp = dir.join(format!(".{name}.tmp"));
        fs_err::write(&tmp, hash.as_bytes())?;
        fs_err::rename(&tmp, p)?;
        Ok(())
    }
}

/// A remote name is a single, ordinary file name: no separators, not `.` or `..`.
pub fn valid_remote_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        bail!("`{name}` can't be a remote name - use a plain word like `origin` or `phone`");
    }
    Ok(())
}
