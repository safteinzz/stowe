//! Playable-mirror remotes: a remote that *is* your files.
//!
//! A `local:` remote is laid out just like the working tree — real files at
//! their real paths — so any media player (or a curious human) can read it
//! directly. Stowe's bookkeeping lives in a hidden `.stowe/` at the remote
//! root, mirroring the `.stowe/` in your working copy:
//!
//! ```text
//! <remote>/
//!   Artist/Album/song.mp3     ← real, playable files (the current commit)
//!   .stowe/
//!     refs/main               ← the commit the tree currently reflects
//!     commits/<hash>.json     ← full history
//!     objects/<ab>/<rest>     ← ONLY superseded versions, for rollback
//! ```
//!
//! Pushing syncs the tree to the latest commit: new files are copied in, moved
//! files are *renamed in place* (cheap — no re-copy over USB), and files that
//! were replaced or deleted have their old bytes tucked into `.stowe/objects/`
//! so the mirror can still travel back in time on its own.

use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::{Commit, Entry, Manifest};
use crate::repo::Repo;
use crate::scan;

/// A `local:<path>` URL (or a bare path) → the mirror root. Returns `None` for
/// non-local schemes (e.g. `s3://`), which use the object-store format instead.
pub fn local_root(url: &str) -> Option<PathBuf> {
    if let Some(p) = url.strip_prefix("local:") {
        Some(PathBuf::from(p))
    } else if url.contains("://") {
        None
    } else {
        Some(PathBuf::from(url))
    }
}

fn dot(root: &Path) -> PathBuf {
    root.join(".stowe")
}

/// Where a superseded version's bytes are parked, keyed by content hash.
fn object_path(root: &Path, hash: &str) -> PathBuf {
    dot(root).join("objects").join(&hash[..2]).join(&hash[2..])
}

/// What a sync changed, for the summary line.
#[derive(Default)]
pub struct SyncReport {
    pub added: usize,
    pub moved: usize,
    pub modified: usize,
    pub removed: usize,
    pub new_commits: usize,
}

/// Changes found on the mirror that stowe didn't make (drift).
#[derive(Default)]
struct Drift {
    /// On the mirror but not in its recorded snapshot (e.g. copy-pasted in).
    foreign: Vec<String>,
    /// In the recorded snapshot but gone from the mirror (deleted by hand).
    missing: Vec<String>,
    /// Present but a different size than recorded (edited in place).
    changed: Vec<String>,
}

impl Drift {
    fn is_empty(&self) -> bool {
        self.foreign.is_empty() && self.missing.is_empty() && self.changed.is_empty()
    }
    fn report(&self) {
        use colored::Colorize;
        eprintln!("{}", "the mirror was changed outside stowe:".yellow().bold());
        for p in &self.foreign {
            eprintln!("  {} {p}", "added on mirror:".green());
        }
        for p in &self.missing {
            eprintln!("  {} {p}", "deleted on mirror:".red());
        }
        for p in &self.changed {
            eprintln!("  {} {p}", "edited on mirror:".yellow());
        }
    }
}

/// Sync the mirror at `root` to `repo`'s HEAD. `force` overwrites drift.
pub fn sync(repo: &Repo, root: &Path, force: bool) -> Result<SyncReport> {
    let head = repo
        .head()?
        .ok_or_else(|| anyhow!("nothing committed yet — `stowe commit` first"))?;
    let history = repo.history()?;
    let target: &Manifest = &history[0].1.files;

    std::fs::create_dir_all(dot(root).join("objects"))
        .with_context(|| format!("creating mirror at {}", root.display()))?;
    std::fs::create_dir_all(dot(root).join("commits"))?;

    // The snapshot the mirror currently reflects (empty on a fresh mirror).
    let remote_manifest: Manifest = match read_ref(root)? {
        Some(h) => read_commit_files(root, &h)?,
        None => Vec::new(),
    };

    // Did someone touch the mirror behind stowe's back? Cheap check: paths +
    // sizes vs the recorded snapshot (no hashing). Bail unless --force.
    let drift = detect_drift(root, &remote_manifest)?;
    if !drift.is_empty() && !force {
        drift.report();
        bail!(
            "mirror `{}` has changes made outside stowe — reconcile, or re-run with --force to \
             overwrite it to match this commit",
            root.display()
        );
    }

    // Plan = how to turn the mirror's snapshot into HEAD's.
    let d = scan::diff(&remote_manifest, target);

    // New bytes come from the local working tree, indexed by content hash (so a
    // file renamed since the commit is still found under its new name).
    let working = scan::scan(repo, &repo.head_manifest()?, false)?;
    let mut by_hash: HashMap<&str, &str> = HashMap::new();
    for e in &working {
        by_hash.entry(&e.hash).or_insert(&e.path);
    }
    let target_by_path: HashMap<&str, &Entry> =
        target.iter().map(|e| (e.path.as_str(), e)).collect();
    let remote_by_path: HashMap<&str, &Entry> =
        remote_manifest.iter().map(|e| (e.path.as_str(), e)).collect();

    // 1. Moves — rename in place (the whole point: no re-copy).
    for (from, to) in &d.moved {
        let src = root.join(from);
        let dst = root.join(to);
        ensure_parent(&dst)?;
        if src.exists() {
            std::fs::rename(&src, &dst)?;
        } else {
            copy_in(repo, &by_hash, &target_by_path, to, &dst)?;
        }
    }
    // 2. Removals — preserve the old bytes for rollback, then drop from the tree.
    for path in &d.removed {
        if let Some(e) = remote_by_path.get(path.as_str()) {
            preserve(root, &e.hash, &root.join(path))?;
        }
        remove_file_and_empty_dirs(root, &root.join(path))?;
    }
    // 3. In-place changes — preserve the old version, write the new one.
    for path in &d.modified {
        if let Some(e) = remote_by_path.get(path.as_str()) {
            preserve(root, &e.hash, &root.join(path))?;
        }
        copy_in(repo, &by_hash, &target_by_path, path, &root.join(path))?;
    }
    // 4. New files.
    for path in &d.added {
        copy_in(repo, &by_hash, &target_by_path, path, &root.join(path))?;
    }

    // History + ref, so the mirror is self-describing.
    let mut new_commits = 0;
    for (h, c) in &history {
        let dst = dot(root).join("commits").join(format!("{h}.json"));
        if !dst.exists() {
            std::fs::write(&dst, serde_json::to_vec_pretty(c)?)?;
            new_commits += 1;
        }
    }
    write_ref(root, &head)?;

    Ok(SyncReport {
        added: d.added.len(),
        moved: d.moved.len(),
        modified: d.modified.len(),
        removed: d.removed.len(),
        new_commits,
    })
}

/// Copy the content for `path` (in the target snapshot) from the local working
/// tree into `dst` on the mirror.
fn copy_in(
    repo: &Repo,
    by_hash: &HashMap<&str, &str>,
    target_by_path: &HashMap<&str, &Entry>,
    path: &str,
    dst: &Path,
) -> Result<()> {
    let entry = target_by_path
        .get(path)
        .ok_or_else(|| anyhow!("internal: {path} not in target snapshot"))?;
    let src_rel = by_hash.get(entry.hash.as_str()).ok_or_else(|| {
        anyhow!(
            "content for `{path}` is no longer in the working tree (modified or deleted \
             since the commit) — restore it or commit the change before pushing"
        )
    })?;
    ensure_parent(dst)?;
    std::fs::copy(repo.root.join(src_rel), dst)
        .with_context(|| format!("copying {path} to mirror"))?;
    Ok(())
}

/// Move the bytes currently at `current` into the mirror's object store under
/// `hash`, unless we already have that version parked.
fn preserve(root: &Path, hash: &str, current: &Path) -> Result<()> {
    if !current.exists() {
        return Ok(());
    }
    let obj = object_path(root, hash);
    if obj.exists() {
        return Ok(()); // already have this version
    }
    ensure_parent(&obj)?;
    // Rename frees the real path for the new content and is instant on-device.
    std::fs::rename(current, &obj).with_context(|| format!("preserving old {}", current.display()))?;
    Ok(())
}

fn ensure_parent(p: &Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Remove a file and any now-empty parent directories, stopping at `root`.
fn remove_file_and_empty_dirs(root: &Path, file: &Path) -> Result<()> {
    if file.exists() {
        std::fs::remove_file(file)?;
    }
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) {
            break;
        }
        // Only removes if empty; a non-empty dir errors and we stop.
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// Walk the mirror's real tree (skipping `.stowe`) and flag anything that
/// doesn't match its recorded snapshot.
fn detect_drift(root: &Path, manifest: &Manifest) -> Result<Drift> {
    let mut expected: HashMap<String, u64> =
        manifest.iter().map(|e| (e.path.clone(), e.size)).collect();
    let mut drift = Drift::default();

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for entry in rd {
            let entry = entry?;
            if entry.file_name() == std::ffi::OsStr::new(".stowe") {
                continue;
            }
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(&entry.path())
                    .to_string_lossy()
                    .replace('\\', "/");
                match expected.remove(&rel) {
                    Some(size) => {
                        if entry.metadata()?.len() != size {
                            drift.changed.push(rel);
                        }
                    }
                    None => drift.foreign.push(rel),
                }
            }
        }
    }
    // Anything left in `expected` was recorded but is missing from the tree.
    drift.missing = expected.into_keys().collect();
    drift.foreign.sort();
    drift.missing.sort();
    drift.changed.sort();
    Ok(drift)
}

/// What a pull brought down.
pub struct PullReport {
    pub head: String,
    pub new_commits: usize,
    pub written: usize,
}

/// Pull a local mirror at `root` into `repo`: copy down the history and rebuild
/// the working tree from the mirror's real files (falling back to preserved
/// versions in `.stowe/objects/` if a current file is somehow missing).
pub fn pull(repo: &Repo, root: &Path) -> Result<PullReport> {
    let remote_head =
        read_ref(root)?.ok_or_else(|| anyhow!("mirror `{}` is empty — nothing to pull", root.display()))?;

    // Copy down the commit chain metadata we don't already have.
    let mut new_commits = 0;
    let mut cur = Some(remote_head.clone());
    while let Some(h) = cur {
        let local = repo.dir.join("commits").join(format!("{h}.json"));
        let bytes = if local.exists() {
            std::fs::read(&local)?
        } else {
            let b = std::fs::read(dot(root).join("commits").join(format!("{h}.json")))
                .with_context(|| format!("reading mirror commit {h}"))?;
            std::fs::write(&local, &b)?;
            new_commits += 1;
            b
        };
        let commit: Commit = serde_json::from_slice(&bytes)?;
        cur = commit.parent;
    }
    repo.set_head(&remote_head)?;

    // Rebuild the working tree for the mirror's snapshot.
    let files = read_commit_files(root, &remote_head)?;
    let mut written = 0;
    for e in &files {
        let dest = repo.root.join(&e.path);
        if dest.exists() && scan::hash_file(&dest)? == e.hash {
            continue;
        }
        // Prefer the mirror's current real file; fall back to a preserved copy.
        let real = root.join(&e.path);
        let src = if real.exists() && scan::hash_file(&real)? == e.hash {
            real
        } else {
            object_path(root, &e.hash)
        };
        ensure_parent(&dest)?;
        std::fs::copy(&src, &dest)
            .with_context(|| format!("pulling {} from mirror", e.path))?;
        written += 1;
    }
    repo.clear_index()?;

    Ok(PullReport {
        head: remote_head,
        new_commits,
        written,
    })
}

/// Copy the bytes for content `hash` from the mirror into `dest`, for `restore`.
/// Looks in the preserved-version store first, then among the mirror's current
/// files. Returns `false` if this mirror doesn't have that content.
pub fn fetch(root: &Path, hash: &str, dest: &Path) -> Result<bool> {
    let obj = object_path(root, hash);
    let src = if obj.exists() {
        obj
    } else {
        // Maybe it's a file that's still current on the mirror.
        let Some(h) = read_ref(root)? else { return Ok(false) };
        match read_commit_files(root, &h)?.iter().find(|e| e.hash == hash) {
            Some(e) => root.join(&e.path),
            None => return Ok(false),
        }
    };
    ensure_parent(dest)?;
    std::fs::copy(&src, dest).with_context(|| format!("restoring {} from mirror", dest.display()))?;
    Ok(true)
}

// --- mirror metadata (the remote `.stowe/`) ---------------------------------

fn read_ref(root: &Path) -> Result<Option<String>> {
    let p = dot(root).join("refs").join("main");
    match std::fs::read_to_string(p) {
        Ok(s) => {
            let s = s.trim().to_string();
            Ok(if s.is_empty() { None } else { Some(s) })
        }
        Err(_) => Ok(None),
    }
}

fn write_ref(root: &Path, hash: &str) -> Result<()> {
    let refs = dot(root).join("refs");
    std::fs::create_dir_all(&refs)?;
    std::fs::write(refs.join("main"), hash.as_bytes())?;
    Ok(())
}

fn read_commit_files(root: &Path, hash: &str) -> Result<Manifest> {
    let p = dot(root).join("commits").join(format!("{hash}.json"));
    let bytes = std::fs::read(&p).with_context(|| format!("reading mirror commit {hash}"))?;
    let commit: crate::model::Commit = serde_json::from_slice(&bytes)?;
    Ok(commit.files)
}
