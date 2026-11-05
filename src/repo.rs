//! The local repository: locating `.stowe/`, reading/writing HEAD, config,
//! the staging index, and commit objects.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::model::{Commit, Config, Entry, Manifest};

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
        self.snapshot(self.head()?.as_deref())
    }

    /// The snapshot of commit `hash`, as [`Repo::head`] names one: empty for
    /// none. Read from `.stowe/cache/head` while that holds the same commit,
    /// since the JSON of a big tree's commit takes several times as long to
    /// parse, and kept there for the next read.
    pub fn snapshot(&self, hash: Option<&str>) -> Result<Manifest> {
        let Some(hash) = hash else {
            return Ok(Vec::new());
        };
        if let Some(files) = self.read_head_cache(hash) {
            return Ok(files);
        }
        let files = self.read_commit(hash)?.files;
        let _ = self.write_cache("head", &encode_head(hash, &files));
        Ok(files)
    }

    /// The parent of commit `hash`, read off the start of its file when it is
    /// written there first, as stowe writes it, so a walk up the history never
    /// reads a whole snapshot to learn one hash.
    pub fn read_parent(&self, hash: &str) -> Result<Option<String>> {
        let mut start = [0u8; 256];
        let mut file = fs_err::File::open(self.commit_path(hash))
            .with_context(|| format!("missing commit {hash}"))?;
        let mut n = 0;
        while n < start.len() {
            match file.read(&mut start[n..])? {
                0 => break,
                read => n += read,
            }
        }
        match parent_at_start(&start[..n]) {
            Some(parent) => Ok(parent),
            None => Ok(self.read_commit(hash)?.parent),
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

    // --- cache ------------------------------------------------------------

    /// `.stowe/cache/`: what scans worked out and can work out again, so
    /// anything in it may be deleted at any time.
    pub fn cache_dir(&self) -> PathBuf {
        self.dir.join("cache")
    }

    /// What `.stowe/cache/hashes` holds, or nothing when there is no readable
    /// cache, since it only ever saves a read.
    pub fn read_hash_cache(&self) -> HashCache {
        self.read_cache("hashes")
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// `.stowe/cache/<name>`, or nothing when it can't be read.
    pub fn read_cache(&self, name: &str) -> Option<Vec<u8>> {
        fs_err::read(self.cache_dir().join(name)).ok()
    }

    pub fn write_hash_cache(&self, cache: &HashCache) -> Result<()> {
        self.write_cache("hashes", &serde_json::to_vec(cache)?)
    }

    /// The snapshot `.stowe/cache/head` holds, when it holds commit `hash`'s
    /// whole and nothing else, as [`encode_head`] wrote it.
    fn read_head_cache(&self, hash: &str) -> Option<Manifest> {
        let file = fs_err::File::open(self.cache_dir().join("head")).ok()?;
        let mut r = std::io::BufReader::with_capacity(1 << 16, file);
        let mut magic = [0u8; HEAD_CACHE.len()];
        r.read_exact(&mut magic).ok()?;
        if magic != *HEAD_CACHE || read_text(&mut r, MAX_FIELD)? != hash {
            return None;
        }
        let count = u64::from_le_bytes(read_8(&mut r)?);
        let mut files = Manifest::with_capacity(usize::try_from(count).ok()?.min(1 << 24));
        for _ in 0..count {
            let path = read_text(&mut r, MAX_PATH)?;
            let size = u64::from_le_bytes(read_8(&mut r)?);
            let mtime = i64::from_le_bytes(read_8(&mut r)?);
            let hash = read_text(&mut r, MAX_FIELD)?;
            let fp = match read_u8(&mut r)? {
                0 => None,
                1 => Some(read_text(&mut r, MAX_FIELD)?),
                _ => return None,
            };
            files.push(Entry {
                path,
                size,
                mtime,
                hash,
                fp,
            });
        }
        // Anything after the last entry means this is not what was written.
        read_u8(&mut r).is_none().then_some(files)
    }

    /// Write `.stowe/cache/<name>` whole: into a file of this write's own
    /// beside it, then moved over it, so a reader never sees half of one and
    /// two writers never share a file.
    pub fn write_cache(&self, name: &str, bytes: &[u8]) -> Result<()> {
        static WRITES: AtomicUsize = AtomicUsize::new(0);
        let dir = self.cache_dir();
        fs_err::create_dir_all(&dir)?;
        let n = WRITES.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{name}.{}.{n}", std::process::id()));
        let written =
            fs_err::write(&tmp, bytes).and_then(|()| fs_err::rename(&tmp, dir.join(name)));
        if written.is_err() {
            let _ = fs_err::remove_file(&tmp);
        }
        Ok(written?)
    }

    /// `.stowe/cache/<name>`, made if missing and opened to be locked.
    pub fn cache_lock(&self, name: &str) -> Result<fs_err::File> {
        let dir = self.cache_dir();
        fs_err::create_dir_all(&dir)?;
        Ok(fs_err::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join(name))?)
    }

    // --- staging index ----------------------------------------------------

    fn index_path(&self) -> PathBuf {
        self.dir.join("index")
    }

    /// The staged snapshot, if anything is staged.
    pub fn read_index(&self) -> Result<Option<Manifest>> {
        Ok(match self.index_bytes()? {
            Some(bytes) => Some(serde_json::from_slice(&bytes)?),
            None => None,
        })
    }

    /// The index file as it is on disk, if anything is staged.
    pub fn index_bytes(&self) -> Result<Option<Vec<u8>>> {
        let p = self.index_path();
        if !p.exists() {
            return Ok(None);
        }
        Ok(Some(fs_err::read(p)?))
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

/// The hashes scans took of files HEAD and the index hold no hash for at their
/// size and mtime, which `.stowe/cache/hashes` keeps.
#[derive(Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct HashCache {
    /// The second the scan that took them began in.
    pub since: i64,
    /// Path-sorted, each with its file's real mtime.
    pub files: Manifest,
}

impl HashCache {
    /// Whether `e`'s hash still holds for a file found with `size` and `mtime`
    /// by a scan that began in second `now`. A write sets a file's mtime to the
    /// second it lands in, so bytes hashed with an mtime older than the scan
    /// that hashed them, or one still ahead of `now`, can't have changed since
    /// without the mtime changing too.
    pub fn holds(&self, e: &Entry, size: u64, mtime: i64, now: i64) -> bool {
        e.size == size && e.mtime == mtime && (mtime < self.since || mtime > now)
    }
}

/// What `.stowe/cache/head` opens with, so a cache written another way is
/// never read as this one.
const HEAD_CACHE: &[u8; 13] = b"stowe-head 1\n";
/// The longest hash or fingerprint a cached entry may claim, past which the
/// file is not one this wrote.
const MAX_FIELD: u32 = 1 << 10;
/// The same for a path.
const MAX_PATH: u32 = 1 << 16;

/// Commit `hash`'s `files` as `.stowe/cache/head` holds them: [`HEAD_CACHE`],
/// the hash, how many entries, then each entry's fields in a row, numbers
/// little-endian and text after its length.
fn encode_head(hash: &str, files: &Manifest) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + files.len() * 160);
    let text = |out: &mut Vec<u8>, s: &str| {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    };
    out.extend_from_slice(HEAD_CACHE);
    text(&mut out, hash);
    out.extend_from_slice(&(files.len() as u64).to_le_bytes());
    for e in files {
        text(&mut out, &e.path);
        out.extend_from_slice(&e.size.to_le_bytes());
        out.extend_from_slice(&e.mtime.to_le_bytes());
        text(&mut out, &e.hash);
        match &e.fp {
            None => out.push(0),
            Some(fp) => {
                out.push(1);
                text(&mut out, fp);
            }
        }
    }
    out
}

fn read_u8(r: &mut impl Read) -> Option<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b).ok()?;
    Some(b[0])
}

fn read_8(r: &mut impl Read) -> Option<[u8; 8]> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).ok()?;
    Some(b)
}

/// A length then that many bytes of UTF-8, refused past `max` bytes.
fn read_text(r: &mut impl Read, max: u32) -> Option<String> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    let len = u32::from_le_bytes(b);
    if len > max {
        return None;
    }
    let mut bytes = vec![0u8; len as usize];
    r.read_exact(&mut bytes).ok()?;
    String::from_utf8(bytes).ok()
}

/// The `parent` a commit file opens with, `{"parent": "<hash>"` or
/// `{"parent": null`, or `None` when it opens any other way.
fn parent_at_start(start: &[u8]) -> Option<Option<String>> {
    let text = match std::str::from_utf8(start) {
        Ok(text) => text,
        Err(e) => std::str::from_utf8(&start[..e.valid_up_to()]).ok()?,
    };
    let value = text
        .trim_start()
        .strip_prefix('{')?
        .trim_start()
        .strip_prefix("\"parent\"")?
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    if value.starts_with("null") {
        return Some(None);
    }
    let (hash, _) = value.strip_prefix('"')?.split_once('"')?;
    let plain = !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit());
    plain.then(|| Some(hash.to_string()))
}

/// A remote name is a single, ordinary file name: no separators, not `.` or `..`.
pub fn valid_remote_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        bail!("`{name}` can't be a remote name - use a plain word like `origin` or `phone`");
    }
    Ok(())
}
