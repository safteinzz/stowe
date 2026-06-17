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
        let mut cur = std::env::current_dir()?;
        loop {
            if cur.join(".stowe").is_dir() {
                return Ok(Repo {
                    dir: cur.join(".stowe"),
                    root: cur,
                });
            }
            if !cur.pop() {
                bail!("not a stowe repo (no .stowe found here or in any parent)");
            }
        }
    }

    /// Create a fresh, empty repo at `path`.
    pub fn init(path: &Path) -> Result<Repo> {
        let dir = path.join(".stowe");
        if dir.exists() {
            bail!("`.stowe` already exists at {}", dir.display());
        }
        std::fs::create_dir_all(dir.join("commits"))?;
        std::fs::write(dir.join("HEAD"), b"")?;
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
        let s = std::fs::read_to_string(self.dir.join("HEAD")).unwrap_or_default();
        let s = s.trim().to_string();
        Ok(if s.is_empty() { None } else { Some(s) })
    }

    pub fn set_head(&self, hash: &str) -> Result<()> {
        std::fs::write(self.dir.join("HEAD"), hash.as_bytes())?;
        Ok(())
    }

    // --- config -----------------------------------------------------------

    pub fn config(&self) -> Result<Config> {
        let p = self.dir.join("config");
        if !p.exists() {
            return Ok(Config::default());
        }
        Ok(serde_json::from_slice(&std::fs::read(p)?)?)
    }

    pub fn save_config(&self, cfg: &Config) -> Result<()> {
        std::fs::write(self.dir.join("config"), serde_json::to_vec_pretty(cfg)?)?;
        Ok(())
    }

    // --- commits ----------------------------------------------------------

    fn commit_path(&self, hash: &str) -> PathBuf {
        self.dir.join("commits").join(format!("{hash}.json"))
    }

    pub fn read_commit(&self, hash: &str) -> Result<Commit> {
        let p = self.commit_path(hash);
        let bytes = std::fs::read(&p).with_context(|| format!("missing commit {hash}"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Serialize a commit and store it under its own content hash. Returns the hash.
    pub fn write_commit(&self, commit: &Commit) -> Result<String> {
        let bytes = serde_json::to_vec_pretty(commit)?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        std::fs::write(self.commit_path(&hash), &bytes)?;
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
        Ok(Some(serde_json::from_slice(&std::fs::read(p)?)?))
    }

    pub fn write_index(&self, manifest: &Manifest) -> Result<()> {
        std::fs::write(self.index_path(), serde_json::to_vec_pretty(manifest)?)?;
        Ok(())
    }

    pub fn clear_index(&self) -> Result<()> {
        let p = self.index_path();
        if p.exists() {
            std::fs::remove_file(p)?;
        }
        Ok(())
    }
}
