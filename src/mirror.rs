//! Playable-mirror remotes: a remote that *is* your files.
//!
//! A `local:` remote is laid out just like the working tree - real files at
//! their real paths - so any media player (or a curious human) can read it
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
//! files are *renamed in place* (cheap - no re-copy over USB), and files that
//! were replaced or deleted have their old bytes tucked into `.stowe/objects/`
//! so the mirror can still travel back in time on its own.

use anyhow::{Context, Result, anyhow, bail};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::ignore::Ignore;
use crate::model::{Commit, Entry, Manifest, short};
use crate::repo::Repo;
use crate::scan;

/// How many file copies to keep in flight when syncing a mirror. Enough to hide
/// per-file latency on a network mirror, few enough not to thrash a USB drive.
const COPY_CONCURRENCY: usize = 8;

/// A `local:<path>` URL (or a bare path) → the mirror root. A leading `~` is
/// expanded, so a remote can be typed (and stored) the way it is printed back.
/// Returns `None` for non-local schemes (e.g. `s3://`), which use the
/// object-store format instead.
pub fn local_root(url: &str) -> Option<PathBuf> {
    if let Some(p) = url.strip_prefix("local:") {
        Some(crate::paths::expand(p))
    } else if url.contains("://") {
        None
    } else {
        Some(crate::paths::expand(url))
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
        eprintln!(
            "{}",
            "the mirror was changed outside stowe:".yellow().bold()
        );
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

/// Sync the mirror at `root` to `repo`'s HEAD. `force` overwrites drift, and a
/// mirror whose history this repo doesn't have.
pub fn sync(repo: &Repo, root: &Path, force: bool) -> Result<SyncReport> {
    let head = repo
        .head()?
        .ok_or_else(|| anyhow!("nothing committed yet - `stowe commit` first"))?;
    let history = repo.history()?;
    let target: &Manifest = &history[0].1.files;

    std::fs::create_dir_all(dot(root).join("objects"))
        .with_context(|| format!("creating mirror at {}", crate::paths::short(root)))?;
    std::fs::create_dir_all(dot(root).join("commits"))?;
    recover_tmp(root)?;

    // The snapshot the mirror currently reflects (empty on a fresh mirror).
    let recorded_head = read_ref(root)?;
    if let Some(h) = &recorded_head
        && !force
        && !history.iter().any(|(c, _)| c == h)
    {
        bail!(
            "mirror `{}` is at commit {}, which this repo doesn't have - `stowe pull` it first, \
             or re-run with --force to replace its history with this one",
            crate::paths::short(root),
            short(h)
        );
    }
    let remote_manifest: Manifest = match &recorded_head {
        Some(h) => read_commit_files(root, h)?,
        None => Vec::new(),
    };

    // What's really on the mirror (one walk, reused below for repair).
    let ignore = Ignore::load(&repo.root).keeping(
        remote_manifest
            .iter()
            .chain(target)
            .map(|e| e.path.as_str()),
    );
    let mut actual = mirror_sizes(root, &ignore)?;
    let folds = crate::names::probe_case_insensitive(root);
    let dirs_on_disk = dir_spellings(&actual);
    let spelled = if folds {
        adopt_spellings(&mut actual, &remote_manifest)
    } else {
        Vec::new()
    };

    // Did someone touch the mirror behind stowe's back, in a way this push would
    // clobber? Cheap check: paths + sizes, no hashing. Bail unless --force.
    let drift = detect_drift(&actual, &remote_manifest, target);
    if !drift.is_empty() {
        if !force {
            drift.report();
            bail!(
                "mirror `{}` has changes made outside stowe - reconcile, or re-run with --force \
                 to overwrite it to match this commit",
                crate::paths::short(root)
            );
        }
        for p in &drift.foreign {
            remove_file_and_empty_dirs(root, &root.join(p))?;
            actual.remove(p);
            eprintln!("removed from mirror: {}", crate::names::display(p));
        }
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
    let remote_by_path: HashMap<&str, &Entry> = remote_manifest
        .iter()
        .map(|e| (e.path.as_str(), e))
        .collect();
    let hash_of = |path: &str| target_by_path.get(path).map_or("", |e| e.hash.as_str());

    let prog = scan::Progress::new();
    let tmp = dot(root).join("tmp");
    std::fs::create_dir_all(&tmp)?;

    // A name the mirror already holds (in any case, where its filesystem ignores
    // case) can't be renamed onto until whatever holds it has moved or gone, so
    // such a move parks its file under `.stowe/tmp/` and lands it last.
    let fold = |p: &str| {
        if folds {
            p.to_lowercase()
        } else {
            p.to_string()
        }
    };
    let taken: HashSet<String> = actual.keys().map(|p| fold(p)).collect();
    let mut parked: Vec<(PathBuf, &String)> = Vec::new();

    // 1. Moves - rename in place (the whole point: no re-copy). Cheap metadata
    //    ops, but each is a network round-trip on an sshfs mirror, so report.
    let mut copies: Vec<(&String, PathBuf)> = Vec::new();
    for (i, (from, to)) in d.moved.iter().enumerate() {
        let src = root.join(from);
        if !src.exists() {
            copies.push((to, root.join(to))); // content isn't there to move; copy it below
            continue;
        }
        if taken.contains(&fold(to)) {
            let park = tmp.join(format!("move-{}-{}", parked.len(), hash_of(to)));
            rename_on_mirror(&src, &park, from)?;
            remove_file_and_empty_dirs(root, &src)?;
            parked.push((park, to));
        } else {
            let dst = root.join(to);
            ensure_parent(&dst)?;
            rename_on_mirror(&src, &dst, from)?;
        }
        // Repair below reads `actual`, and would re-copy a file it still thinks
        // is at its old path.
        if let Some(size) = actual.remove(from) {
            actual.insert(to.clone(), size);
        }
        prog.tick(&format!("moving... {}/{}", i + 1, d.moved.len()));
    }
    // 2. Removals - preserve the old bytes for rollback, then drop from the tree.
    for (i, path) in d.removed.iter().enumerate() {
        if let Some(e) = remote_by_path.get(path.as_str()) {
            preserve(root, &e.hash, &root.join(path))?;
        }
        remove_file_and_empty_dirs(root, &root.join(path))?;
        prog.tick(&format!("removing... {}/{}", i + 1, d.removed.len()));
    }
    // 3. In-place changes - preserve the old version before the new one lands.
    for path in &d.modified {
        if let Some(e) = remote_by_path.get(path.as_str()) {
            preserve(root, &e.hash, &root.join(path))?;
        }
        copies.push((path, root.join(path)));
    }
    // 4. Spelling, where the filesystem ignores case: folders first, then the
    //    parked moves land, then files still under an old spelling.
    let mut respelled = 0;
    if folds {
        respell_dirs(root, target, &dirs_on_disk)?;
    }
    for (park, to) in &parked {
        let dst = root.join(to);
        ensure_parent(&dst)?;
        rename_on_mirror(park, &dst, to)?;
    }
    for (_, path) in &spelled {
        if target_by_path.contains_key(path.as_str()) && !d.modified.contains(path) {
            let park = tmp.join(format!("move-respell-{}", hash_of(path)));
            rename_on_mirror(&root.join(path), &park, path)?;
            rename_on_mirror(&park, &root.join(path), path)?;
            respelled += 1;
        }
    }
    // 5. New files.
    for path in &d.added {
        copies.push((path, root.join(path)));
    }
    // 6. Repair. The plan so far is a diff of two manifests, which is blind to the
    // mirror's real state: a file deleted or truncated on the drive still matches
    // between snapshots, so bring back anything the target wants and does not have.
    let queued: HashSet<&str> = copies.iter().map(|(p, _)| p.as_str()).collect();
    let repairs: Vec<&String> = target
        .iter()
        .filter(|e| actual.get(&e.path) != Some(&e.size) && !queued.contains(e.path.as_str()))
        .map(|e| &e.path)
        .collect();
    for path in repairs {
        copies.push((path, root.join(path)));
    }

    // Copying the bytes is the slow part, and on a network mirror (a phone over
    // sshfs) it's latency-bound: each file waits on a round-trip. Run a bounded
    // handful concurrently so the link stays busy. Bounded, not unbounded, since
    // a USB/FUSE mirror gains nothing from a stampede.
    if !copies.is_empty() {
        let total = copies.len();
        let done = AtomicUsize::new(0);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(COPY_CONCURRENCY)
            .build()?;
        pool.install(|| -> Result<()> {
            copies
                .par_iter()
                .enumerate()
                .map(|(i, (path, dst))| -> Result<()> {
                    let part = tmp.join(format!("copy-{i}"));
                    copy_in(repo, &by_hash, &target_by_path, path, dst, &part)?;
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_multiple_of(8) || n == total {
                        prog.tick(&format!("copying... {n}/{total}"));
                    }
                    Ok(())
                })
                .collect::<Result<()>>()
        })?;
    }

    // Renaming a folder relocates its files but leaves the old directory
    // behind, empty. Sweep every such ghost (this also heals a mirror that
    // accumulated them before this pass existed).
    prune_empty_dirs(root)?;
    let _ = std::fs::remove_dir(&tmp);

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
    prog.clear();

    Ok(SyncReport {
        added: d.added.len(),
        moved: d.moved.len() + respelled,
        modified: d.modified.len(),
        removed: d.removed.len(),
        new_commits,
    })
}

/// Copy the content for `path` (in the target snapshot) from the local working
/// tree into `dst` on the mirror, by way of `part`, so a copy cut short never
/// sits at `dst` looking like the real file.
fn copy_in(
    repo: &Repo,
    by_hash: &HashMap<&str, &str>,
    target_by_path: &HashMap<&str, &Entry>,
    path: &str,
    dst: &Path,
    part: &Path,
) -> Result<()> {
    let entry = target_by_path
        .get(path)
        .ok_or_else(|| anyhow!("internal: {path} not in target snapshot"))?;
    let src_rel = by_hash.get(entry.hash.as_str()).ok_or_else(|| {
        anyhow!(
            "content for `{path}` is no longer in the working tree (modified or deleted \
             since the commit) - restore it or commit the change before pushing"
        )
    })?;
    let shown = crate::names::display(path);
    std::fs::copy(repo.root.join(src_rel), part)
        .with_context(|| format!("copying {shown} to mirror"))?;
    ensure_parent(dst)?;
    // An sftp server without the posix-rename extension refuses to rename onto
    // an existing file, which a repair does.
    if std::fs::rename(part, dst).is_err() {
        if dst.is_file() {
            std::fs::remove_file(dst).with_context(|| format!("replacing {shown} on mirror"))?;
        }
        std::fs::rename(part, dst).with_context(|| format!("copying {shown} to mirror"))?;
    }
    Ok(())
}

/// Rename on the mirror, saying which file failed.
fn rename_on_mirror(from: &Path, to: &Path, path: &str) -> Result<()> {
    std::fs::rename(from, to)
        .with_context(|| format!("moving {} on mirror", crate::names::display(path)))
}

/// Clear out `.stowe/tmp/` after a push that was cut short: a half-written copy
/// is dropped, and a file parked mid-move becomes a preserved version when its
/// bytes still match the hash in its name, rather than sitting where no walk
/// ever looks.
fn recover_tmp(root: &Path) -> Result<()> {
    let Ok(rd) = std::fs::read_dir(dot(root).join("tmp")) else {
        return Ok(());
    };
    for entry in rd {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let hash = name
            .strip_prefix("move-")
            .and_then(|rest| rest.rsplit_once('-'))
            .map(|(_, h)| h)
            .filter(|h| h.len() == 64);
        match hash {
            Some(h) if !object_path(root, h).exists() && scan::hash_file(&path)? == h => {
                let obj = object_path(root, h);
                ensure_parent(&obj)?;
                std::fs::rename(&path, &obj)?;
            }
            _ => std::fs::remove_file(&path)?,
        }
    }
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
    std::fs::rename(current, &obj)
        .with_context(|| format!("preserving old {}", current.display()))?;
    Ok(())
}

/// Every directory holding a file in `actual`, keyed by its lowercase name,
/// spelled as it is on disk.
fn dir_spellings(actual: &HashMap<String, u64>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for path in actual.keys() {
        for (slash, _) in path.match_indices('/') {
            let dir = &path[..slash];
            out.entry(dir.to_lowercase())
                .or_insert_with(|| dir.to_string());
        }
    }
    out
}

/// Where the filesystem ignores case: recorded paths the mirror holds under
/// another spelling, as `(on_disk, recorded)` pairs, with `actual` rekeyed to
/// the recorded spelling, since it is the same file. Only a recorded path is
/// matched, so a file dropped on the mirror by hand stays drift.
fn adopt_spellings(
    actual: &mut HashMap<String, u64>,
    recorded: &Manifest,
) -> Vec<(String, String)> {
    let recorded_paths: HashSet<&str> = recorded.iter().map(|e| e.path.as_str()).collect();
    let mut by_fold: HashMap<String, Vec<String>> = HashMap::new();
    for p in actual.keys() {
        if !recorded_paths.contains(p.as_str()) {
            by_fold.entry(p.to_lowercase()).or_default().push(p.clone());
        }
    }
    let mut out = Vec::new();
    for e in recorded {
        if actual.contains_key(&e.path) {
            continue;
        }
        let Some([on_disk]) = by_fold.get(&e.path.to_lowercase()).map(Vec::as_slice) else {
            continue;
        };
        if let Some(size) = actual.remove(on_disk) {
            actual.insert(e.path.clone(), size);
            out.push((on_disk.clone(), e.path.clone()));
        }
    }
    out
}

/// Where the filesystem ignores case: give each folder the spelling `target`
/// uses. A rename that only changes case does nothing there, so it goes by a
/// temporary name beside the folder, taking whatever else is inside along.
/// `on_disk` is the spelling the walk found; a folder the moves already emptied
/// is gone, and the files landing in it create it spelled right.
fn respell_dirs(root: &Path, target: &Manifest, on_disk: &HashMap<String, String>) -> Result<()> {
    let mut wanted: Vec<&str> = target
        .iter()
        .flat_map(|e| e.path.match_indices('/').map(|(slash, _)| &e.path[..slash]))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    wanted.sort_by_key(|d| d.matches('/').count());
    for dir in wanted {
        let Some(spelled) = on_disk.get(&dir.to_lowercase()) else {
            continue;
        };
        let src = root.join(spelled);
        if spelled == dir || !src.is_dir() {
            continue;
        }
        let park = src.with_file_name(".stowe-respell");
        rename_on_mirror(&src, &park, spelled)?;
        rename_on_mirror(&park, &root.join(dir), dir)?;
    }
    Ok(())
}

fn ensure_parent(p: &Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Remove every empty directory on the mirror (except `.stowe`), deepest-first
/// so a parent is empty by the time we reach it. stowe tracks files, never
/// directories, so any empty directory on the mirror is an artifact of a rename
/// or delete and is safe to drop.
fn prune_empty_dirs(root: &Path) -> Result<()> {
    let mut dirs = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = match std::fs::read_dir(&d) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            if entry.file_name() == std::ffi::OsStr::new(".stowe") {
                continue;
            }
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let p = entry.path();
                stack.push(p.clone());
                dirs.push(p);
            }
        }
    }
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in dirs {
        let _ = std::fs::remove_dir(&d); // succeeds only if empty
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

/// What's *actually* on the mirror right now: repo-relative path -> size.
/// Cheap (no hashing), and the single source of truth for both drift detection
/// and repair, so we only walk the tree once.
///
/// `.stoweignore` applies here as well as to the working tree. That's the whole
/// point for a phone: a gallery app recreates `.thumbnails/` the moment it
/// indexes the folder, and junk stowe would never push must not read as drift
/// and demand `--force` on every single push.
fn mirror_sizes(root: &Path, ignore: &Ignore) -> Result<HashMap<String, u64>> {
    Ok(walk_mirror(root, ignore)?
        .into_iter()
        .map(|(rel, _, size)| (rel, size))
        .collect())
}

/// Every file on the mirror outside `.stowe/`: `(rel, abs, size)`. A folder that
/// can't be read is an error, not an empty folder, because what isn't seen here
/// reads as deleted.
fn walk_mirror(root: &Path, ignore: &Ignore) -> Result<Vec<(String, PathBuf, u64)>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = std::fs::read_dir(&dir)
            .with_context(|| format!("reading {} on mirror", crate::paths::short(&dir)))?;
        for entry in rd {
            let entry = entry?;
            if entry.file_name() == std::ffi::OsStr::new(".stowe") {
                continue;
            }
            let ft = entry.file_type()?;
            let abs = entry.path();
            let rel = scan::recordable_path(root, &abs)?;
            if ignore.is_ignored(&rel, ft.is_dir()) {
                continue;
            }
            if ft.is_dir() {
                stack.push(abs);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let size = entry.metadata()?.len();
            out.push((rel, abs, size));
        }
    }
    Ok(out)
}

/// Flag changes made to the mirror outside stowe.
///
/// Drift is measured against the mirror's *recorded* snapshot, but judged
/// against the snapshot we're about to push (`target`). A difference the target
/// already accounts for is not drift, it's reconciled: after `stowe adapt`
/// pulls a hand-dropped song into the repo and you commit it, pushing it back
/// must not trip over the very file we just adopted. We only block on changes
/// the push would actually clobber or resurrect.
fn detect_drift(actual: &HashMap<String, u64>, recorded: &Manifest, target: &Manifest) -> Drift {
    let target_size: HashMap<&str, u64> =
        target.iter().map(|e| (e.path.as_str(), e.size)).collect();
    let recorded_size: HashMap<&str, u64> =
        recorded.iter().map(|e| (e.path.as_str(), e.size)).collect();
    let mut drift = Drift::default();

    for (rel, size) in actual {
        // Already what we're about to push? Then it isn't drift.
        if target_size.get(rel.as_str()) == Some(size) {
            continue;
        }
        match recorded_size.get(rel.as_str()) {
            Some(rec) if rec != size => drift.changed.push(rel.clone()),
            Some(_) => {}
            None => drift.foreign.push(rel.clone()),
        }
    }
    // Recorded but gone from the tree: only a problem if the push would put it
    // back. If the target drops it too, the mirror simply got there first.
    for e in recorded {
        if !actual.contains_key(&e.path) && target_size.contains_key(e.path.as_str()) {
            drift.missing.push(e.path.clone());
        }
    }
    drift.foreign.sort();
    drift.missing.sort();
    drift.changed.sort();
    drift
}

/// The commit the mirror at `root` reflects, if anything was pushed there.
pub fn head(root: &Path) -> Result<Option<String>> {
    read_ref(root)
}

/// A commit's JSON as the mirror stores it.
pub fn commit_bytes(root: &Path, hash: &str) -> Result<Vec<u8>> {
    std::fs::read(dot(root).join("commits").join(format!("{hash}.json")))
        .with_context(|| format!("reading mirror commit {}", short(hash)))
}

/// What an adapt pulled in from the mirror.
#[derive(Default)]
pub struct AdaptReport {
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub moved: usize,
}

impl AdaptReport {
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.removed == 0 && self.modified == 0 && self.moved == 0
    }
}

/// Bring changes made on the mirror outside stowe (a song copy-pasted onto the
/// phone, one deleted by hand) into the local working tree. The reverse of
/// push: `remote ➜ local`.
///
/// What counts is only what changed on the mirror since stowe last wrote it
/// (its recorded snapshot against its real files), so a local file the mirror
/// never had - committed and not pushed yet, or untracked - is left alone. A
/// path changed on both sides is a conflict, and any conflict stops the adapt
/// before it touches anything. Only the working tree is changed; the caller
/// still `commit`s to record it. To stay cheap we trust the mirror's recorded
/// hashes for same-path/same-size files and only hash what actually differs.
pub fn adapt(repo: &Repo, root: &Path) -> Result<AdaptReport> {
    let recorded: Manifest = match read_ref(root)? {
        Some(h) => read_commit_files(root, &h)?,
        None => Vec::new(),
    };
    let rec_by_path: HashMap<&str, &Entry> =
        recorded.iter().map(|e| (e.path.as_str(), e)).collect();

    // The mirror's true current snapshot (captures manual drift). Ignored paths
    // stay out of it, so `adapt` never imports the drive's own junk.
    let ignore = Ignore::load(&repo.root).keeping(recorded.iter().map(|e| e.path.as_str()));
    let mut actual: Manifest = Vec::new();
    for (rel, abs, size) in walk_mirror(root, &ignore)? {
        let hash = match rec_by_path.get(rel.as_str()) {
            Some(e) if e.size == size => e.hash.clone(),
            _ => scan::hash_file(&abs)?,
        };
        actual.push(Entry {
            path: rel,
            size,
            mtime: 0, // unused: the diff keys on path+hash, and commit re-records it
            hash,
            fp: None,
        });
    }
    let on_mirror = scan::diff(&recorded, &actual);
    let new_hash: HashMap<&str, &str> = actual
        .iter()
        .map(|e| (e.path.as_str(), e.hash.as_str()))
        .collect();

    // A local path may change only while it still holds what the mirror recorded
    // there (or is already what the mirror holds now).
    let scanned = scan::scan(repo, &repo.head_manifest()?, false)?;
    let scanned: HashMap<&str, &str> = scanned
        .iter()
        .map(|e| (e.path.as_str(), e.hash.as_str()))
        .collect();
    let local = |path: &str| -> Result<Option<String>> {
        if let Some(h) = scanned.get(path) {
            return Ok(Some(h.to_string()));
        }
        let abs = repo.root.join(path);
        match std::fs::symlink_metadata(&abs) {
            Ok(m) if m.is_file() => Ok(Some(scan::hash_file(&abs)?)),
            Ok(_) => Ok(Some(String::new())),
            Err(_) => Ok(None),
        }
    };
    let was = |path: &str| rec_by_path.get(path).map(|e| e.hash.as_str());
    let now = |path: &str| new_hash.get(path).copied();

    let mut conflicts: Vec<&String> = Vec::new();
    let mut renames: Vec<(&String, &String)> = Vec::new();
    let mut deletes: Vec<&String> = Vec::new();
    let mut copies: Vec<&String> = Vec::new();
    let mut report = AdaptReport::default();
    for (from, to) in &on_mirror.moved {
        let (src, dst) = (local(from)?, local(to)?);
        if dst.is_some() && dst.as_deref() != now(to) {
            conflicts.push(to);
        } else if src.is_some() && src.as_deref() != was(from) {
            conflicts.push(from);
        } else {
            if src.is_some() && dst.is_none() && was(from) == now(to) {
                renames.push((from, to));
            } else {
                if src.is_some() {
                    deletes.push(from);
                }
                if dst.is_none() {
                    copies.push(to);
                }
            }
            report.moved += 1;
        }
    }
    for path in &on_mirror.removed {
        match local(path)? {
            None => {}
            Some(h) if Some(h.as_str()) == was(path) => {
                deletes.push(path);
                report.removed += 1;
            }
            Some(_) => conflicts.push(path),
        }
    }
    for path in &on_mirror.modified {
        match local(path)? {
            Some(h) if Some(h.as_str()) == now(path) => {}
            Some(h) if Some(h.as_str()) == was(path) => {
                copies.push(path);
                report.modified += 1;
            }
            _ => conflicts.push(path),
        }
    }
    for path in &on_mirror.added {
        match local(path)? {
            None => {
                copies.push(path);
                report.added += 1;
            }
            Some(h) if Some(h.as_str()) == now(path) => {}
            Some(_) => conflicts.push(path),
        }
    }
    if !conflicts.is_empty() {
        conflicts.sort();
        conflicts.dedup();
        eprintln!("changed both here and on the mirror:");
        for p in &conflicts {
            eprintln!("  {}", crate::names::display(p));
        }
        bail!(
            "nothing adapted - commit or restore those files here first, so a change on one \
             side doesn't overwrite the other"
        );
    }

    for (from, to) in renames {
        let dst = repo.root.join(to);
        ensure_parent(&dst)?;
        std::fs::rename(repo.root.join(from), &dst).with_context(|| {
            format!("moving {} in the working tree", crate::names::display(from))
        })?;
    }
    for path in deletes {
        std::fs::remove_file(repo.root.join(path))
            .with_context(|| format!("removing {}", crate::names::display(path)))?;
    }
    for path in copies {
        let want = now(path).unwrap_or_default();
        if !scan::copy_verified(&root.join(path), &repo.root.join(path), want)
            .with_context(|| format!("adopting {} from mirror", crate::names::display(path)))?
        {
            bail!(
                "`{}` changed on the mirror while adapting - run `stowe adapt` again",
                crate::names::display(path)
            );
        }
    }
    Ok(report)
}

/// Copy the bytes for content `hash` from the mirror into `dest`, checked
/// against `hash` on the way. Looks in the preserved-version store, then at
/// `path` on the mirror, then at any current file the mirror recorded with that
/// content. Returns `false` if none of them holds it.
pub fn fetch(root: &Path, path: &str, hash: &str, dest: &Path) -> Result<bool> {
    let mut candidates = vec![object_path(root, hash), root.join(path)];
    if let Some(h) = read_ref(root)? {
        candidates.extend(
            read_commit_files(root, &h)?
                .iter()
                .filter(|e| e.hash == hash)
                .map(|e| root.join(&e.path)),
        );
    }
    for src in candidates {
        if src.is_file() && scan::copy_verified(&src, dest, hash)? {
            return Ok(true);
        }
    }
    Ok(false)
}

// --- format conversion (backup <-> mirror, in place) ------------------------

/// The on-disk shape of a remote.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Format {
    /// Playable tree + hidden `.stowe/`.
    Mirror,
    /// Content-addressed blobs at the root (`objects/`, `commits/`, `refs/`).
    Backup,
    /// Neither - nothing pushed here yet.
    Empty,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Mirror => "mirror",
            Format::Backup => "backup",
            Format::Empty => "empty",
        }
    }
}

/// Sniff a local remote's current format.
pub fn detect_format(root: &Path) -> Format {
    if dot(root).join("refs").join("main").exists() {
        Format::Mirror
    } else if root.join("refs").join("main").exists() {
        Format::Backup
    } else {
        Format::Empty
    }
}

/// What a conversion did.
pub struct ConvertReport {
    /// Files placed at (or as) their real content.
    pub files: usize,
    /// Superseded versions relocated (kept for rollback).
    pub preserved: usize,
}

/// Convert an object-store backup into a playable mirror, in place. Blobs are
/// *renamed* into their real paths (a copy only when the same content is used
/// by several paths - dedup), so there's no bulk re-copy.
pub fn backup_to_mirror(root: &Path) -> Result<ConvertReport> {
    let head = std::fs::read_to_string(root.join("refs").join("main"))
        .context("reading remote refs/main")?
        .trim()
        .to_string();
    let commit: Commit = serde_json::from_slice(&std::fs::read(
        root.join("commits").join(format!("{head}.json")),
    )?)?;
    let manifest = commit.files;

    std::fs::create_dir_all(dot(root).join("objects"))?;

    // Materialize the playable tree from the blobs.
    let mut placed: HashMap<&str, &str> = HashMap::new(); // hash -> first real path
    let mut files = 0;
    for e in &manifest {
        let dest = root.join(&e.path);
        ensure_parent(&dest)?;
        if let Some(first) = placed.get(e.hash.as_str()) {
            // Same content already laid down elsewhere - copy it (dedup fan-out).
            std::fs::copy(root.join(first), &dest)?;
        } else {
            let blob = root.join("objects").join(&e.hash[..2]).join(&e.hash[2..]);
            std::fs::rename(&blob, &dest).with_context(|| format!("materializing {}", e.path))?;
            placed.insert(&e.hash, &e.path);
        }
        files += 1;
    }

    // Whatever blobs remain are superseded versions - keep them for rollback.
    let preserved = move_object_tree(&root.join("objects"), &dot(root).join("objects"))?;

    // Relocate history + ref under `.stowe/`.
    move_flat(&root.join("commits"), &dot(root).join("commits"))?;
    std::fs::create_dir_all(dot(root).join("refs"))?;
    std::fs::rename(
        root.join("refs").join("main"),
        dot(root).join("refs").join("main"),
    )?;
    for stale in ["objects", "commits", "refs"] {
        let _ = std::fs::remove_dir_all(root.join(stale));
    }

    Ok(ConvertReport { files, preserved })
}

/// Convert a playable mirror back into an object-store backup, in place. Real
/// files are *renamed* into content-addressed blobs (dropped when a duplicate
/// is already stored), and the folders that empties are removed; anything else
/// on the drive stays where it is.
pub fn mirror_to_backup(repo: &Repo, root: &Path) -> Result<ConvertReport> {
    let head = read_ref(root)?.ok_or_else(|| anyhow!("mirror is empty - nothing to convert"))?;
    let manifest = read_commit_files(root, &head)?;

    // A file edited or deleted on the mirror would be filed under the hash of
    // the version it replaced, or be missing from the backup.
    let ignore = Ignore::load(&repo.root).keeping(manifest.iter().map(|e| e.path.as_str()));
    let mut drift = detect_drift(&mirror_sizes(root, &ignore)?, &manifest, &manifest);
    drift.foreign.clear();
    if !drift.is_empty() {
        drift.report();
        bail!(
            "mirror `{}` has changes made outside stowe - `stowe adapt` or `stowe push --force` \
             it before converting",
            crate::paths::short(root)
        );
    }
    std::fs::create_dir_all(root.join("objects"))?;

    let mut files = 0;
    for e in &manifest {
        let real = root.join(&e.path);
        let blob = root.join("objects").join(&e.hash[..2]).join(&e.hash[2..]);
        if !blob.exists() && real.exists() {
            ensure_parent(&blob)?;
            std::fs::rename(&real, &blob)?;
            files += 1;
        }
        // Content already stored (dedup) goes with the file, as do the folders
        // it leaves empty.
        remove_file_and_empty_dirs(root, &real)?;
    }

    // Preserved old versions rejoin the flat object store.
    let preserved = move_object_tree(&dot(root).join("objects"), &root.join("objects"))?;

    // History + ref move back to the root.
    move_flat(&dot(root).join("commits"), &root.join("commits"))?;
    std::fs::create_dir_all(root.join("refs"))?;
    std::fs::rename(
        dot(root).join("refs").join("main"),
        root.join("refs").join("main"),
    )?;
    let _ = std::fs::remove_dir_all(dot(root));

    Ok(ConvertReport { files, preserved })
}

/// Move every `<shard>/<blob>` from one object tree to another (skip dups).
fn move_object_tree(src: &Path, dst: &Path) -> Result<usize> {
    if !src.exists() {
        return Ok(0);
    }
    let mut moved = 0;
    let shards: Vec<_> = std::fs::read_dir(src)?.collect::<std::result::Result<_, _>>()?;
    for shard in shards {
        if !shard.file_type()?.is_dir() {
            continue;
        }
        let dst_shard = dst.join(shard.file_name());
        let blobs: Vec<_> =
            std::fs::read_dir(shard.path())?.collect::<std::result::Result<_, _>>()?;
        for blob in blobs {
            std::fs::create_dir_all(&dst_shard)?;
            let target = dst_shard.join(blob.file_name());
            if target.exists() {
                std::fs::remove_file(blob.path())?;
            } else {
                std::fs::rename(blob.path(), target)?;
                moved += 1;
            }
        }
    }
    Ok(moved)
}

/// Move every file from `src` dir into `dst` dir.
fn move_flat(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    let entries: Vec<_> = std::fs::read_dir(src)?.collect::<std::result::Result<_, _>>()?;
    for e in entries {
        std::fs::rename(e.path(), dst.join(e.file_name()))?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn m(entries: &[(&str, &str, u64)]) -> Manifest {
        entries
            .iter()
            .map(|(path, hash, size)| Entry {
                path: (*path).into(),
                size: *size,
                mtime: 0,
                hash: (*hash).into(),
                fp: None,
            })
            .collect()
    }

    fn sizes(entries: &[(&str, u64)]) -> HashMap<String, u64> {
        entries.iter().map(|(p, s)| ((*p).into(), *s)).collect()
    }

    #[test]
    fn local_paths_are_mirrors_and_urls_are_not() {
        assert_eq!(
            local_root("local:/mnt/drive"),
            Some(PathBuf::from("/mnt/drive"))
        );
        assert_eq!(local_root("/mnt/drive"), Some(PathBuf::from("/mnt/drive")));
        assert_eq!(local_root("s3://bucket/music"), None);
    }

    #[test]
    fn an_untouched_mirror_has_no_drift() {
        let recorded = m(&[("a.mp3", "h1", 1)]);
        let actual = sizes(&[("a.mp3", 1)]);
        assert!(detect_drift(&actual, &recorded, &recorded).is_empty());
    }

    #[test]
    fn a_file_dropped_on_the_mirror_by_hand_is_drift() {
        let recorded = m(&[("a.mp3", "h1", 1)]);
        let actual = sizes(&[("a.mp3", 1), ("byhand.mp3", 9)]);
        let d = detect_drift(&actual, &recorded, &recorded);
        assert_eq!(d.foreign, ["byhand.mp3"]);
    }

    #[test]
    fn a_file_deleted_on_the_mirror_is_drift_when_we_would_put_it_back() {
        let recorded = m(&[("a.mp3", "h1", 1)]);
        let actual = sizes(&[]);
        let d = detect_drift(&actual, &recorded, &recorded);
        assert_eq!(d.missing, ["a.mp3"]);
    }

    #[test]
    fn a_deletion_we_are_also_making_is_not_drift() {
        // The mirror just got there first: our target drops it too.
        let recorded = m(&[("a.mp3", "h1", 1)]);
        let target = m(&[]);
        let actual = sizes(&[]);
        assert!(detect_drift(&actual, &recorded, &target).is_empty());
    }

    #[test]
    fn a_file_we_already_adopted_is_not_drift() {
        // Regression: `adapt` pulled a hand-dropped song into the repo, but the
        // drift check still flagged it, so pushing it back was impossible.
        let recorded = m(&[("a.mp3", "h1", 1)]);
        let target = m(&[("a.mp3", "h1", 1), ("byhand.mp3", "h2", 9)]);
        let actual = sizes(&[("a.mp3", 1), ("byhand.mp3", 9)]);
        assert!(
            detect_drift(&actual, &recorded, &target).is_empty(),
            "the file we just adopted must not read as foreign"
        );
    }
}
