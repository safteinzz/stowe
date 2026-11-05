//! Scanning the working tree into a manifest, and diffing two manifests.

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

use serde::{Deserialize, Serialize};

use crate::ignore::Ignore;
use crate::model::{Entry, Manifest, UNTRUSTED_MTIME};
use crate::repo::{HashCache, Repo};

/// The progress line a TUI draws in its footer, once it has taken the terminal
/// over: from then on every [`Progress`] writes here instead of to stderr,
/// which would print over the screen.
static SCREEN: OnceLock<Arc<Mutex<String>>> = OnceLock::new();

/// Send every later [`Progress`] to the line this returns rather than stderr.
pub fn progress_to_screen() -> Arc<Mutex<String>> {
    SCREEN.get_or_init(Arc::default).clone()
}

/// A one-line, in-place progress indicator for the long phases of a scan.
///
/// Big trees make `status`/`add` sit for a few seconds; without a sign of life
/// that reads as a hang. So we stream a live count to *stderr*, and only when
/// it's a terminal - piped/redirected output stays byte-for-byte clean, and the
/// real result always goes to stdout. Ticking is throttled by the caller so the
/// print cost never shows up in the timing. Under a TUI the count goes to the
/// line [`progress_to_screen`] handed it.
pub struct Progress {
    show: bool,
    screen: Option<Arc<Mutex<String>>>,
}

impl Progress {
    pub fn new() -> Self {
        let screen = SCREEN.get().cloned();
        Progress {
            show: screen.is_none() && std::io::stderr().is_terminal(),
            screen,
        }
    }
    /// Overwrite the current line with `msg` (only if attached to a terminal).
    pub fn tick(&self, msg: &str) {
        if let Some(line) = &self.screen {
            if let Ok(mut line) = line.lock() {
                *line = msg.to_string();
            }
        } else if self.show {
            eprint!("\r\x1b[K{msg}");
            let _ = std::io::stderr().flush();
        }
    }
    /// Wipe the progress line so it doesn't linger above the real output.
    pub fn clear(&self) {
        if let Some(line) = &self.screen {
            if let Ok(mut line) = line.lock() {
                line.clear();
            }
        } else if self.show {
            eprint!("\r\x1b[K");
            let _ = std::io::stderr().flush();
        }
    }
}

/// blake3 of a file's full content, streamed so we never load big files whole.
pub fn hash_file(path: &std::path::Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut reader = BufReader::new(fs_err::File::open(path)?);
    let mut buf = [0u8; 65536];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// `arg` as typed, made absolute against `cwd` and resolved through symlinks,
/// refused unless it is inside `root` (itself resolved). A path that no longer
/// exists, as a deletion being staged, is resolved through its nearest existing
/// ancestor, with the rest appended and its `..` applied lexically.
pub fn resolve_arg(
    root: &std::path::Path,
    cwd: &std::path::Path,
    arg: &std::path::Path,
) -> Result<std::path::PathBuf> {
    use std::path::Component;
    let lexical = if arg.is_absolute() {
        arg.to_path_buf()
    } else {
        cwd.join(arg)
    };
    let mut base = lexical.as_path();
    let mut abs = loop {
        match base.canonicalize() {
            Ok(c) => break c,
            Err(_) => {
                base = base
                    .parent()
                    .with_context(|| format!("no such path: {}", arg.display()))?
            }
        }
    };
    for c in lexical.strip_prefix(base).unwrap_or(&lexical).components() {
        match c {
            Component::Normal(name) => abs.push(name),
            Component::ParentDir => {
                abs.pop();
            }
            _ => {}
        }
    }
    if abs.strip_prefix(root).is_err() {
        bail!("{} is outside the repo", arg.display());
    }
    Ok(abs)
}

/// Repo-relative, forward-slash path string for `abs` under `root`. Purely
/// lexical, so it also works for files that no longer exist (staged deletions).
pub fn rel_path(root: &std::path::Path, abs: &std::path::Path) -> String {
    abs.strip_prefix(root)
        .unwrap_or(abs)
        .to_string_lossy()
        .into_owned()
}

/// [`rel_path`] for a file that is about to be recorded. A manifest holds
/// UTF-8, and a lossy spelling would name a file that doesn't exist, so a name
/// that isn't UTF-8 is refused instead.
pub fn recordable_path(root: &std::path::Path, abs: &std::path::Path) -> Result<String> {
    let rel = abs.strip_prefix(root).unwrap_or(abs);
    match rel.to_str() {
        Some(s) => Ok(s.to_string()),
        None => bail!(
            "`{}` isn't a valid UTF-8 name, so stowe can't record it - rename it",
            rel.to_string_lossy()
        ),
    }
}

/// Write `dest` from `src` through a temporary file beside it, and only rename
/// it into place when the bytes hash to `want`. A copy cut short, or bytes
/// edited on a remote since they were recorded, never land under a name that
/// claims to be that version. Returns `false`, writing nothing, on a mismatch.
pub fn copy_verified(src: &std::path::Path, dest: &std::path::Path, want: &str) -> Result<bool> {
    let mut reader = BufReader::new(fs_err::File::open(src)?);
    let mut landing = Landing::create(dest, want)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        landing.write(&buf[..n])?;
    }
    landing.finish()
}

/// A file being written under a temporary name, hashed as it goes; see
/// [`copy_verified`].
pub struct Landing {
    tmp: std::path::PathBuf,
    dest: std::path::PathBuf,
    file: fs_err::File,
    hasher: blake3::Hasher,
    want: String,
}

impl Landing {
    pub fn create(dest: &std::path::Path, want: &str) -> Result<Landing> {
        let parent = dest.parent().unwrap_or(std::path::Path::new("."));
        fs_err::create_dir_all(parent)?;
        let name = dest.file_name().unwrap_or_default().to_string_lossy();
        let tmp = parent.join(format!(".{name}.stowe-part"));
        let file = fs_err::File::create(&tmp)
            .with_context(|| format!("writing {}", crate::paths::short(dest)))?;
        Ok(Landing {
            tmp,
            dest: dest.to_path_buf(),
            file,
            hasher: blake3::Hasher::new(),
            want: want.to_string(),
        })
    }

    pub fn write(&mut self, buf: &[u8]) -> Result<()> {
        self.hasher.update(buf);
        self.file
            .write_all(buf)
            .with_context(|| format!("writing {}", crate::paths::short(&self.dest)))
    }

    pub fn finish(self) -> Result<bool> {
        drop(self.file);
        if self.hasher.finalize().to_hex().as_str() != self.want {
            let _ = fs_err::remove_file(&self.tmp);
            return Ok(false);
        }
        fs_err::rename(&self.tmp, &self.dest)
            .with_context(|| format!("writing {}", crate::paths::short(&self.dest)))?;
        Ok(true)
    }
}

/// Build a single manifest [`Entry`] for an existing file (used by per-file
/// `add`). `fingerprint` decodes audio to record its fingerprint, same as a
/// full scan.
pub fn entry_for(
    root: &std::path::Path,
    abs: &std::path::Path,
    fingerprint: bool,
) -> Result<Entry> {
    let since = crate::time::now();
    let meta = fs_err::metadata(abs)?;
    let mtime = recorded_mtime(mtime_secs(&meta)?, since);
    let fp = if fingerprint {
        crate::audio::fingerprint(abs)?
    } else {
        None
    };
    Ok(Entry {
        path: recordable_path(root, abs)?,
        size: meta.len(),
        mtime,
        hash: hash_file(abs)?,
        fp,
    })
}

/// All tracked files under `dir` (recursive), as absolute paths, skipping the
/// `.stowe` metadata dir and anything `.stoweignore` excludes. Used to stage a
/// directory argument.
pub fn files_under(
    root: &std::path::Path,
    dir: &std::path::Path,
    ignore: &Ignore,
) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    for entry in WalkDir::new(dir).into_iter().filter_entry(|e| {
        e.file_name() != ".stowe"
            && !ignore.is_ignored(&rel_path(root, e.path()), e.file_type().is_dir())
    }) {
        let entry = entry?;
        if entry.file_type().is_file() {
            out.push(entry.path().to_path_buf());
        }
    }
    Ok(out)
}

/// `meta`'s mtime in whole seconds.
fn mtime_secs(meta: &std::fs::Metadata) -> Result<i64> {
    Ok(meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0))
}

/// `mtime` as an [`Entry`] records it, or [`UNTRUSTED_MTIME`] when it is not
/// older than `since`, the second its file was stat'd in: an edit later in that
/// second keeps the same mtime, and a same-size one would then read as
/// unchanged.
fn recorded_mtime(mtime: i64, since: i64) -> i64 {
    if mtime < since {
        mtime
    } else {
        UNTRUSTED_MTIME
    }
}

/// Hashes already worked out for the files a walk found, each trusted only
/// while its file keeps the size and mtime it was recorded with, so an
/// unchanged file is never read again.
struct Known<'a> {
    /// What each manifest given holds at each found file's path, in the order
    /// given, whose `fp` is what `add` decoded.
    recorded: Vec<Vec<Option<&'a Entry>>>,
    /// What scans hashed since, from `.stowe/cache/hashes`, with no `fp`.
    cached: Vec<Option<&'a Entry>>,
    /// Which of `cached` still hold.
    cache: &'a HashCache,
    /// The second the walk began in.
    now: i64,
    /// The sizes those manifests hold at paths no file was found at.
    missing_sizes: HashSet<u64>,
}

/// Where a file's hash came from.
enum Source<'a> {
    Recorded(&'a Entry),
    Cached(&'a Entry),
}

impl<'a> Known<'a> {
    /// `found` and each of `recorded` are path-sorted, as [`walk`] and
    /// [`in_path_order`] give them, and `now` is the second the walk began in.
    fn new(
        found: &[Found],
        recorded: &[Vec<&'a Entry>],
        cache: &'a HashCache,
        now: i64,
    ) -> Known<'a> {
        let mut missing_sizes = HashSet::new();
        let recorded = recorded
            .iter()
            .map(|m| {
                line_up(found, m, |e| {
                    missing_sizes.insert(e.size);
                })
            })
            .collect();
        let cached = line_up(found, &in_path_order(&cache.files), |_| {});
        Known {
            recorded,
            cached,
            cache,
            now,
            missing_sizes,
        }
    }

    /// The hash of `f`, the `i`th file found, if one is known.
    fn get(&self, i: usize, f: &Found) -> Option<Source<'a>> {
        let same = |e: &&Entry| e.size == f.size && e.mtime == f.mtime;
        self.recorded
            .iter()
            .find_map(|m| m[i].filter(same))
            .map(Source::Recorded)
            .or_else(|| {
                self.cached[i]
                    .filter(|e| self.cache.holds(e, f.size, f.mtime, self.now))
                    .map(Source::Cached)
            })
    }
}

/// `m`'s entries in path order: as they are, since stowe writes them that way,
/// unless something wrote them otherwise.
fn in_path_order(m: &Manifest) -> Vec<&Entry> {
    let mut refs: Vec<&Entry> = m.iter().collect();
    if !refs.is_sorted_by(|a, b| a.path <= b.path) {
        refs.sort_by(|a, b| a.path.cmp(&b.path));
    }
    refs
}

/// What `entries` holds at each of `found`'s paths, the last of any it holds
/// twice, both path-sorted, handing `missing` each entry at a path where no
/// file was found. One pass over both, where a lookup per path would hash
/// every path in the tree.
fn line_up<'a>(
    found: &[Found],
    entries: &[&'a Entry],
    mut missing: impl FnMut(&'a Entry),
) -> Vec<Option<&'a Entry>> {
    let mut rest = entries.iter().copied().peekable();
    let mut lined = Vec::with_capacity(found.len());
    for f in found {
        while let Some(e) = rest.next_if(|e| e.path < f.rel) {
            missing(e);
        }
        let mut hit = None;
        while let Some(e) = rest.next_if(|e| e.path == f.rel) {
            hit = Some(e);
        }
        lined.push(hit);
    }
    rest.for_each(missing);
    lined
}

/// Whether one of `tracked` (each path-sorted) holds `rel`, or a path in the
/// folder `rel`.
fn holds(tracked: &[Vec<&str>], rel: &str) -> bool {
    let inside = format!("{rel}/");
    tracked.iter().any(|paths| {
        paths.binary_search(&rel).is_ok()
            || paths
                .get(paths.partition_point(|p| *p < inside.as_str()))
                .is_some_and(|p| p.starts_with(&inside))
    })
}

/// HEAD and, when something is staged, the index, each in path order.
fn bases_of<'a>(head: &'a Manifest, index: Option<&'a Manifest>) -> Vec<Vec<&'a Entry>> {
    std::iter::once(head)
        .chain(index)
        .map(in_path_order)
        .collect()
}

/// The paths of each of `bases`.
fn paths_of<'a>(bases: &[Vec<&'a Entry>]) -> Vec<Vec<&'a str>> {
    bases
        .iter()
        .map(|m| m.iter().map(|e| e.path.as_str()).collect())
        .collect()
}

/// Keep in `.stowe/cache/hashes` the hashes in `taken` (path-sorted, each
/// with its file's real mtime), taken by a scan that began in second `since`,
/// unless nothing was `read` and the cache already holds exactly those. A file
/// read again because its hash couldn't be trusted is rewritten with the newer
/// `since`, or it would be read on every scan. Nothing fails over it, since a
/// hash it lacks is only read again.
fn keep_hashes(repo: &Repo, before: &HashCache, taken: Manifest, read: bool, since: i64) {
    if read || taken != before.files {
        let _ = repo.write_hash_cache(&HashCache {
            since,
            files: taken,
        });
    }
}

/// One walked working-tree file, with the cheap metadata (size+mtime) already
/// captured by the walk. Content hashing is what's deferred to the parallel
/// pass in [`scan`] and [`working`].
struct Found {
    rel: String,
    size: u64,
    mtime: i64,
}

/// A folder the walk has still to read.
struct Folder {
    abs: PathBuf,
    /// Its repo-relative path, empty for the root.
    rel: String,
    /// Whether a `.stoweignore` rule names it or a folder it is in.
    named: bool,
}

/// The working-tree walk, which skips `.stowe` and anything `.stoweignore`
/// excludes, pruning an excluded folder whole so nothing in it is even stat'd.
struct Walker<'a> {
    root: &'a Path,
    ignore: Ignore,
    /// Paths, each list path-sorted, never excluded: what HEAD and the index
    /// hold, or at least each of those `.stoweignore` names.
    tracked: &'a [Vec<&'a str>],
    prog: &'a Progress,
    files: AtomicUsize,
}

impl Walker<'_> {
    /// The files `folder` holds, and the folders in it still to read.
    ///
    /// Uses `read_dir` + [`fs_err::DirEntry::metadata`], so each file's
    /// size/mtime comes from an `fstatat` relative to the already-open
    /// directory fd. That matters enormously on deep trees over FUSE/network
    /// mounts: a `stat` of the full absolute path makes the kernel re-resolve
    /// *every* directory component per file, e.g. ~5× slower on an
    /// NTFS-over-FUSE tree 10+ levels deep. Symlinks (and other non-regular
    /// entries) are skipped.
    fn read(&self, folder: &Folder) -> Result<(Vec<Found>, Vec<Folder>)> {
        let mut files = Vec::new();
        let mut folders = Vec::new();
        for entry in fs_err::read_dir(&folder.abs)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == ".stowe" {
                continue;
            }
            let ft = entry.file_type()?;
            if !ft.is_dir() && !ft.is_file() {
                continue;
            }
            let Some(name) = name.to_str() else {
                // `recordable_path` refuses it, saying which one it is.
                recordable_path(self.root, &entry.path())?;
                continue;
            };
            // What `recordable_path` makes of it, built on the folder's instead
            // of cut from the whole path, since this runs for every name in the
            // tree.
            let mut rel = String::with_capacity(folder.rel.len() + 1 + name.len());
            if !folder.rel.is_empty() {
                rel.push_str(&folder.rel);
                rel.push('/');
            }
            rel.push_str(name);
            let named = self.ignore.walking(&rel, ft.is_dir(), folder.named);
            if named && !holds(self.tracked, &rel) {
                continue;
            }
            if ft.is_dir() {
                folders.push(Folder {
                    abs: entry.path(),
                    rel,
                    named,
                });
            } else {
                let meta = entry.metadata()?;
                let mtime = mtime_secs(&meta)?;
                files.push(Found {
                    rel,
                    size: meta.len(),
                    mtime,
                });
            }
        }
        let before = self.files.fetch_add(files.len(), Ordering::Relaxed);
        let now = before + files.len();
        if now / 1000 > before / 1000 {
            self.prog.tick(&format!("scanning... {now} files"));
        }
        Ok((files, folders))
    }

    /// Every file under `top`, one folder after another.
    fn in_turn(&self, top: Folder) -> Result<Vec<Found>> {
        let mut found = Vec::new();
        let mut todo = vec![top];
        while let Some(folder) = todo.pop() {
            let (files, folders) = self.read(&folder)?;
            found.extend(files);
            todo.extend(folders);
        }
        Ok(found)
    }

    /// Every file under `folder` into `found`, the folders in it read side by
    /// side.
    fn at_once(&self, folder: Folder, found: &Mutex<Vec<Found>>) -> Result<()> {
        let (files, folders) = self.read(&folder)?;
        found
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(files);
        folders
            .into_par_iter()
            .try_for_each(|folder| self.at_once(folder, found))
    }
}

/// Whether `path` is on a FUSE filesystem, found in `/proc/self/mountinfo` by
/// its device number, and `false` where that cannot be read.
fn on_fuse(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(dev) = fs_err::metadata(path).map(|m| m.dev()) else {
        return false;
    };
    // How Linux splits a `dev_t` into the `major:minor` mountinfo shows.
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xffff_f000);
    let minor = (dev & 0xff) | ((dev >> 12) & 0xffff_ff00);
    let device = format!("{major}:{minor}");
    let Ok(mounts) = fs_err::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    mounts.lines().any(|line| {
        let fstype = line
            .split_once(" - ")
            .and_then(|(_, after)| after.split(' ').next());
        line.split(' ').nth(2) == Some(device.as_str())
            && fstype.is_some_and(|t| t == "fuse" || t == "fuseblk" || t.starts_with("fuse."))
    })
}

/// Every file under the root that `.stoweignore` lets through, path-sorted,
/// with each path `tracked` holds among them, and the second the walk began
/// in, for [`recorded_mtime`].
///
/// Folders are read side by side, except on FUSE, where one daemon answers
/// every stat in turn, so reading in parallel only adds contention.
fn walk(repo: &Repo, tracked: &[Vec<&str>], prog: &Progress) -> Result<(Vec<Found>, i64)> {
    let since = crate::time::now();
    let walker = Walker {
        root: &repo.root,
        ignore: Ignore::load(&repo.root),
        tracked,
        prog,
        files: AtomicUsize::new(0),
    };
    let top = Folder {
        abs: repo.root.clone(),
        rel: String::new(),
        named: false,
    };
    let mut found = if on_fuse(&repo.root) {
        walker.in_turn(top)?
    } else {
        let found = Mutex::new(Vec::new());
        walker.at_once(top, &found)?;
        found.into_inner().unwrap_or_else(PoisonError::into_inner)
    };
    found.par_sort_unstable_by(|a, b| a.rel.cmp(&b.rel));
    Ok((found, since))
}

/// Counts the files a scan reads under `root`, ticking `prog` now and then.
struct Reading<'a> {
    root: &'a std::path::Path,
    prog: &'a Progress,
    what: &'static str,
    n: AtomicUsize,
}

impl Reading<'_> {
    fn hash(&self, f: &Found) -> Result<String> {
        let n = self.n.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(256) {
            self.prog.tick(&format!("{}... {n} files", self.what));
        }
        hash_file(&self.root.join(&f.rel))
    }
}

/// Walk the working tree and build a fresh, path-sorted manifest, every file
/// hashed.
///
/// `cache_source` (typically HEAD's manifest), the index and
/// `.stowe/cache/hashes` hold hashes reused for files whose size and mtime are
/// unchanged, so only new and modified files are read.
///
/// `fingerprint` controls whether *new/changed* audio files are decoded to
/// produce their audio fingerprint. That decode is by far the most expensive
/// thing stowe does, so a scan that only compares skips it (`false`) and just
/// hashes: plain renames are still caught by hash. `add` passes `true` to
/// record fingerprints in the snapshot, which is what lets a later
/// re-tag+rename read as a move.
pub fn scan(repo: &Repo, cache_source: &Manifest, fingerprint: bool) -> Result<Manifest> {
    let prog = Progress::new();
    let staged = repo.read_index()?.unwrap_or_default();
    let cached = repo.read_hash_cache();
    let bases = [in_path_order(cache_source), in_path_order(&staged)];
    let (found, since) = walk(repo, &paths_of(&bases), &prog)?;
    let known = Known::new(&found, &bases, &cached, since);

    // Hashing is CPU-bound and per-file independent, so fan it across cores.
    let reading = Reading {
        root: &repo.root,
        prog: &prog,
        what: if fingerprint {
            "hashing + fingerprinting"
        } else {
            "hashing"
        },
        n: AtomicUsize::new(0),
    };
    let decode = |f: &Found| -> Result<Option<String>> {
        if fingerprint {
            crate::audio::fingerprint(&repo.root.join(&f.rel))
        } else {
            Ok(None)
        }
    };
    // In `found`'s order, so path-sorted.
    let out: Manifest = found
        .into_par_iter()
        .enumerate()
        .map(|(i, f)| -> Result<Entry> {
            let (hash, fp) = match known.get(i, &f) {
                Some(Source::Recorded(e)) => (e.hash.clone(), e.fp.clone()),
                Some(Source::Cached(e)) => (e.hash.clone(), decode(&f)?),
                None => (reading.hash(&f)?, decode(&f)?),
            };
            Ok(Entry {
                mtime: recorded_mtime(f.mtime, since),
                path: f.rel,
                size: f.size,
                hash,
                fp,
            })
        })
        .collect::<Result<_>>()?;

    prog.clear();
    Ok(out)
}

/// Where in the working tree the bytes of each of `wanted`'s entries are: by
/// hash, the first path in path order holding them. stowe keeps no store of
/// its own, so a push copies from here, and a file renamed since the commit
/// is found under its new name.
///
/// Only files whose hash is already known are looked up, and the rest are read
/// only when their size is that of an entry not yet found, so a folder of new
/// files nobody asked for is walked, not read.
pub fn locate(repo: &Repo, wanted: &Manifest) -> Result<HashMap<String, String>> {
    let prog = Progress::new();
    let head = repo.head_manifest()?;
    let staged = repo.read_index()?.unwrap_or_default();
    let cached = repo.read_hash_cache();
    let bases = [in_path_order(&head), in_path_order(&staged)];
    let (found, since) = walk(repo, &paths_of(&bases), &prog)?;
    let known = Known::new(&found, &bases, &cached, since);
    let mut hashes: Vec<Option<String>> = (0..found.len())
        .map(|i| match known.get(i, &found[i]) {
            Some(Source::Recorded(e) | Source::Cached(e)) => Some(e.hash.clone()),
            None => None,
        })
        .collect();
    let have: HashSet<&str> = hashes.iter().flatten().map(String::as_str).collect();
    let sizes: HashSet<u64> = wanted
        .iter()
        .filter(|e| !have.contains(e.hash.as_str()))
        .map(|e| e.size)
        .collect();

    let reading = Reading {
        root: &repo.root,
        prog: &prog,
        what: "hashing",
        n: AtomicUsize::new(0),
    };
    let read: Vec<(usize, String)> = found
        .par_iter()
        .enumerate()
        .filter(|(i, f)| hashes[*i].is_none() && sizes.contains(&f.size))
        .map(|(i, f)| Ok((i, reading.hash(f)?)))
        .collect::<Result<_>>()?;
    prog.clear();
    for (i, hash) in read {
        hashes[i] = Some(hash);
    }

    let mut by_hash = HashMap::new();
    for (f, hash) in found.into_iter().zip(hashes) {
        if let Some(hash) = hash {
            by_hash.entry(hash).or_insert(f.rel);
        }
    }
    Ok(by_hash)
}

/// The hashes HEAD, the index and `.stowe/cache/hashes` hold, to tell what is
/// at a handful of paths now without walking the tree to find out.
pub struct Hashes {
    /// Each path-sorted: HEAD's, the index's, then the cache's.
    lists: [Manifest; 3],
    cache: HashCache,
    /// The second these were loaded in.
    now: i64,
}

impl Hashes {
    pub fn load(repo: &Repo) -> Result<Hashes> {
        let now = crate::time::now();
        let sorted = |mut m: Manifest| {
            if !m.is_sorted_by(|a, b| a.path <= b.path) {
                m.sort_by(|a, b| a.path.cmp(&b.path));
            }
            m
        };
        let mut cache = repo.read_hash_cache();
        let lists = [
            sorted(repo.head_manifest()?),
            sorted(repo.read_index()?.unwrap_or_default()),
            sorted(std::mem::take(&mut cache.files)),
        ];
        Ok(Hashes { lists, cache, now })
    }

    /// The hash of the file at `path` under `root`, when one was recorded for
    /// its size and mtime and can be trusted, so its bytes need no reading.
    /// `None` for anything else: no file there, a symlink, or one to read.
    pub fn at(&self, root: &Path, path: &str) -> Option<String> {
        let meta = fs_err::symlink_metadata(root.join(path)).ok()?;
        if !meta.is_file() {
            return None;
        }
        let (size, mtime) = (meta.len(), mtime_secs(&meta).ok()?);
        let [head, index, cached] = &self.lists;
        [head, index]
            .into_iter()
            .filter_map(|m| entry_at(m, path))
            .find(|e| e.size == size && e.mtime == mtime)
            .or_else(|| {
                entry_at(cached, path).filter(|e| self.cache.holds(e, size, mtime, self.now))
            })
            .map(|e| e.hash.clone())
    }
}

/// What path-sorted `m` holds at `path`.
fn entry_at<'a>(m: &'a Manifest, path: &str) -> Option<&'a Entry> {
    m.binary_search_by(|e| e.path.as_str().cmp(path))
        .ok()
        .map(|i| &m[i])
}

/// The working tree as compared with HEAD and the index, read only as far as
/// [`working`] needed to.
pub struct Working<'a> {
    head: &'a Manifest,
    index: Option<&'a Manifest>,
    /// A file whose bytes neither comparison depends on keeps an empty `hash`,
    /// which is why the tree never leaves this struct.
    tree: Manifest,
}

impl Working<'_> {
    /// What changed since the last commit.
    pub fn since_head(&self) -> Diff {
        diff(self.head, &self.tree)
    }

    /// What changed since what is staged, or since the last commit when
    /// nothing is.
    pub fn since_staged(&self) -> Diff {
        diff(self.index.unwrap_or(self.head), &self.tree)
    }
}

/// Walk the working tree to compare it with `head` and `index`, reading only
/// the files whose bytes decide what [`diff`] says. A file at a path HEAD or
/// the index holds is read only when its size is the same there, since a new
/// size is already a change, and a file at a new path only when its size is
/// one of a missing file's, since only then can it be that file moved. So a
/// folder of new files costs a walk, not a read of every byte in it.
///
/// What it does read it keeps in `.stowe/cache/hashes`, so the next scan reads
/// it only if it changes again. No audio is decoded.
pub fn working<'a>(
    repo: &Repo,
    head: &'a Manifest,
    index: Option<&'a Manifest>,
) -> Result<Working<'a>> {
    let prog = Progress::new();
    let bases = bases_of(head, index);
    let (found, since) = walk(repo, &paths_of(&bases), &prog)?;
    compare(repo, head, index, &bases, found, since, &prog)
}

/// [`working`] once its walk found `found` from second `since`, with `bases`
/// as [`bases_of`] gives them.
fn compare<'a>(
    repo: &Repo,
    head: &'a Manifest,
    index: Option<&'a Manifest>,
    bases: &[Vec<&'a Entry>],
    found: Vec<Found>,
    since: i64,
    prog: &Progress,
) -> Result<Working<'a>> {
    let cached = repo.read_hash_cache();
    let known = Known::new(&found, bases, &cached, since);
    let needed = |i: usize, f: &Found| {
        known.recorded.iter().any(|m| match m[i] {
            Some(e) => e.size == f.size,
            None => known.missing_sizes.contains(&f.size),
        })
    };

    let reading = Reading {
        root: &repo.root,
        prog,
        what: "hashing",
        n: AtomicUsize::new(0),
    };
    let read: Vec<(Entry, bool)> = found
        .into_par_iter()
        .enumerate()
        .map(|(i, f)| -> Result<(Entry, bool)> {
            let (hash, fp, cache) = match known.get(i, &f) {
                Some(Source::Recorded(e)) => (e.hash.clone(), e.fp.clone(), false),
                Some(Source::Cached(e)) => (e.hash.clone(), None, true),
                None if needed(i, &f) => (reading.hash(&f)?, None, true),
                None => (String::new(), None, false),
            };
            // The real mtime, which `HashCache::holds` judges by when it was taken.
            let entry = Entry {
                mtime: f.mtime,
                path: f.rel,
                size: f.size,
                hash,
                fp,
            };
            Ok((entry, cache))
        })
        .collect::<Result<_>>()?;
    prog.clear();

    // Both path-sorted, as `found` was.
    let mut tree = Manifest::with_capacity(read.len());
    let mut taken = Manifest::new();
    for (entry, cache) in read {
        if cache {
            taken.push(entry.clone());
        }
        tree.push(entry);
    }
    let read = reading.n.load(Ordering::Relaxed) > 0;
    keep_hashes(repo, &cached, taken, read, since);
    Ok(Working { head, index, tree })
}

/// How many paths changed, each kind counted.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Changes {
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub moved: usize,
}

impl Changes {
    pub fn total(self) -> usize {
        self.added + self.removed + self.modified + self.moved
    }
}

/// What `.stowe/cache/changes` holds: the last [`changes`] counted, and what
/// they were counted from.
#[derive(Serialize, Deserialize)]
struct Counted {
    from: Basis,
    /// The tracked paths `.stoweignore` names, path-sorted, which a walk keeps
    /// whatever the rules say.
    named: Vec<String>,
    /// [`tree_digest`] of what the walk found.
    tree: String,
    /// The second the walk began in.
    since: i64,
    changes: Changes,
}

/// Everything besides the files found that decides what [`changes`] counts:
/// this format, HEAD, and the bytes of the index and of `.stoweignore`.
#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Basis {
    format: u32,
    head: Option<String>,
    index: Option<String>,
    rules: String,
}

/// blake3 of each found file's path, size and mtime, in order.
fn tree_digest(found: &[Found]) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut buf = Vec::with_capacity(1 << 16);
    for f in found {
        buf.extend_from_slice(f.rel.as_bytes());
        // A name never holds a NUL, so this ends the path unambiguously.
        buf.push(0);
        buf.extend_from_slice(&f.size.to_le_bytes());
        buf.extend_from_slice(&f.mtime.to_le_bytes());
        if buf.len() >= 1 << 16 {
            hasher.update(&buf);
            buf.clear();
        }
    }
    hasher.update(&buf);
    hasher.finalize().to_hex().to_string()
}

/// What [`working`] and [`Working::since_head`] would count, kept in
/// `.stowe/cache/changes` with what it was counted from. While HEAD, the index
/// and `.stoweignore` stay the same, the next count walks the tree keeping only
/// the tracked paths the rules name, and when every file it finds has kept
/// its size and mtime it neither reads HEAD nor compares a thing.
pub fn changes(repo: &Repo) -> Result<Changes> {
    let prog = Progress::new();
    let index_bytes = repo.index_bytes()?;
    let digest = |bytes: &[u8]| blake3::hash(bytes).to_hex().to_string();
    let rules = fs_err::read(repo.root.join(".stoweignore")).unwrap_or_default();
    let basis = Basis {
        format: 1,
        head: repo.head()?,
        index: index_bytes.as_deref().map(digest),
        rules: digest(&rules),
    };
    let last = repo
        .read_cache("changes")
        .and_then(|bytes| serde_json::from_slice::<Counted>(&bytes).ok())
        .filter(|last| last.from == basis);
    let mut walked = None;
    if let Some(last) = &last {
        let named: Vec<&str> = last.named.iter().map(String::as_str).collect();
        let (found, since) = walk(repo, &[named], &prog)?;
        // As `HashCache::holds` judges a hash: a file whose mtime is neither
        // older than the last count nor still ahead could have been written
        // since, in the second its mtime names.
        let unchanged = found
            .iter()
            .all(|f| f.mtime < last.since || f.mtime > since);
        if unchanged && tree_digest(&found) == last.tree {
            return Ok(last.changes);
        }
        walked = Some((found, since));
    }

    let head = repo.snapshot(basis.head.as_deref())?;
    let index: Option<Manifest> = index_bytes
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()?;
    let bases = bases_of(&head, index.as_ref());
    let tracked = paths_of(&bases);
    // What the rules name is what decides the walk, so the one just made with
    // only those paths kept found what one keeping them all would.
    let (found, since) = match walked {
        Some(walked) => walked,
        None => walk(repo, &tracked, &prog)?,
    };
    let tree = tree_digest(&found);
    let mut named: Vec<String> = Ignore::load(&repo.root)
        .naming(tracked.iter().flatten().copied())
        .into_iter()
        .map(str::to_string)
        .collect();
    named.sort();
    named.dedup();
    let changes = compare(repo, &head, index.as_ref(), &bases, found, since, &prog)?
        .since_head()
        .changes();
    let counted = Counted {
        from: basis,
        named,
        tree,
        since,
        changes,
    };
    if let Ok(bytes) = serde_json::to_vec(&counted) {
        let _ = repo.write_cache("changes", &bytes);
    }
    Ok(changes)
}

/// The result of comparing an old snapshot to a new one.
#[derive(Default)]
pub struct Diff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<String>,
    /// (old_path -> new_path) for content that moved/renamed.
    pub moved: Vec<(String, String)>,
}

impl Diff {
    pub fn changes(&self) -> Changes {
        Changes {
            added: self.added.len(),
            removed: self.removed.len(),
            modified: self.modified.len(),
            moved: self.moved.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.modified.is_empty()
            && self.moved.is_empty()
    }
}

/// Compare two manifests.
///
/// A file at the *same* path with different content is `modified`. The
/// interesting work is the rest - paths that vanished and paths that appeared
/// - which we try to pair up as moves before declaring them removed/added:
///
///  1. **exact content** (same `hash`) - a plain rename, any file type;
///  2. **same audio** (same `fp`) - a song that was renamed *and* re-tagged,
///     whose container bytes (and thus `hash`) changed but whose audio didn't.
///
/// Pairing is greedy and one-to-one (each appeared path matches at most one
/// disappeared path), so duplicate content and several simultaneous moves are
/// handled instead of collapsing to a single representative path.
pub fn diff(old: &Manifest, new: &Manifest) -> Diff {
    let mut d = Diff::default();

    // Same path, different bytes → modified in place. Paths that exist on only
    // one side are move/rename candidates.
    let in_order = |m: &Manifest| m.is_sorted_by(|a, b| a.path < b.path);
    let (gone, fresh) = if in_order(old) && in_order(new) {
        sides_in_order(old, new, &mut d.modified)
    } else {
        sides_by_path(old, new, &mut d.modified)
    };

    // Index the fresh paths so each gone path can claim a match. `taken` keeps
    // the pairing one-to-one across both passes.
    let mut by_hash: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut by_fp: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, e) in fresh.iter().enumerate() {
        by_hash.entry(e.hash.as_str()).or_default().push(i);
        if let Some(fp) = &e.fp {
            by_fp.entry(fp.as_str()).or_default().push(i);
        }
    }
    let mut taken = vec![false; fresh.len()];
    let mut claimed = vec![false; gone.len()];

    // Pop the next not-yet-taken fresh index from a candidate list.
    let claim = |q: &mut Vec<usize>, taken: &[bool]| -> Option<usize> {
        while let Some(i) = q.pop() {
            if !taken[i] {
                return Some(i);
            }
        }
        None
    };

    // Pass 1: exact-content moves (hash). Pass 2: same-audio moves (fp), only
    // for what's left - so a perfect content match always wins over an fp one.
    for (gi, g) in gone.iter().enumerate() {
        if let Some(fi) = by_hash
            .get_mut(g.hash.as_str())
            .and_then(|q| claim(q, &taken))
        {
            taken[fi] = true;
            claimed[gi] = true;
            d.moved.push((g.path.clone(), fresh[fi].path.clone()));
        }
    }
    for (gi, g) in gone.iter().enumerate() {
        if claimed[gi] {
            continue;
        }
        let Some(fp) = g.fp.as_deref() else { continue };
        if let Some(fi) = by_fp.get_mut(fp).and_then(|q| claim(q, &taken)) {
            taken[fi] = true;
            claimed[gi] = true;
            d.moved.push((g.path.clone(), fresh[fi].path.clone()));
        }
    }

    // Whatever stayed unpaired is a genuine removal / addition.
    for (gi, g) in gone.iter().enumerate() {
        if !claimed[gi] {
            d.removed.push(g.path.clone());
        }
    }
    for (fi, f) in fresh.iter().enumerate() {
        if !taken[fi] {
            d.added.push(f.path.clone());
        }
    }

    d.added.sort();
    d.removed.sort();
    d.modified.sort();
    d.moved.sort();
    d
}

/// The entries at paths only `old` holds and only `new` holds, each in its
/// manifest's order, with the paths both hold at different bytes pushed onto
/// `modified` in `new`'s order. Both must be in strict path order, so one pass
/// over the two finds them where [`sides_by_path`] hashes every path twice.
fn sides_in_order<'a>(
    old: &'a Manifest,
    new: &'a Manifest,
    modified: &mut Vec<String>,
) -> (Vec<&'a Entry>, Vec<&'a Entry>) {
    let (mut gone, mut fresh) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while let (Some(o), Some(n)) = (old.get(i), new.get(j)) {
        match o.path.cmp(&n.path) {
            std::cmp::Ordering::Less => {
                gone.push(o);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                fresh.push(n);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                if o.hash != n.hash {
                    modified.push(n.path.clone());
                }
                i += 1;
                j += 1;
            }
        }
    }
    gone.extend(&old[i..]);
    fresh.extend(&new[j..]);
    (gone, fresh)
}

/// [`sides_in_order`] for manifests in any order.
fn sides_by_path<'a>(
    old: &'a Manifest,
    new: &'a Manifest,
    modified: &mut Vec<String>,
) -> (Vec<&'a Entry>, Vec<&'a Entry>) {
    let by_path = |m: &'a Manifest| -> HashMap<&'a str, &'a Entry> {
        m.iter().map(|e| (e.path.as_str(), e)).collect()
    };
    let (old_by_path, new_by_path) = (by_path(old), by_path(new));
    for e in new {
        if let Some(old_e) = old_by_path.get(e.path.as_str())
            && old_e.hash != e.hash
        {
            modified.push(e.path.clone());
        }
    }
    let gone = old
        .iter()
        .filter(|e| !new_by_path.contains_key(e.path.as_str()))
        .collect();
    let fresh = new
        .iter()
        .filter(|e| !old_by_path.contains_key(e.path.as_str()))
        .collect();
    (gone, fresh)
}

/// Render a git-style `status`: a staged section (HEAD → index), an unstaged
/// section and an untracked list (both from index → working tree), then the
/// summary line. Only `main` exists, so the branch is always `main`.
pub fn print_status(staged: &Diff, unstaged: &Diff, summary: &Diff) {
    use colored::Colorize;

    let unstaged_changes =
        !unstaged.modified.is_empty() || !unstaged.removed.is_empty() || !unstaged.moved.is_empty();
    if staged.is_empty() && !unstaged_changes && unstaged.added.is_empty() {
        outln!("nothing to commit, working tree clean");
        return;
    }
    // A blank line between sections, none above the first.
    let mut first = true;
    let mut heading = |text: &str| {
        if !std::mem::take(&mut first) {
            outln!();
        }
        outln!("{text}");
    };

    // Colour is by *change type*, not by section: added/new is green, modified
    // yellow, deleted red, renamed blue - consistent everywhere it appears.
    let added = |s: String| s.green();
    let modified = |s: String| s.yellow();
    let deleted = |s: String| s.red();
    let renamed = |s: String| s.blue();

    // Red for what the rename took away, green for what it added, the rest in the
    // blue every rename line uses. The old name is dimmed so the two lines of a
    // rename are told apart by weight as well as by the arrow.
    let marked = |text: &str, kept: &[bool], added: bool| -> String {
        let mut out = String::new();
        for (i, ch) in text.chars().enumerate() {
            let cell = ch.to_string();
            if kept.get(i).copied().unwrap_or(false) {
                out.push_str(&if added { cell.blue() } else { cell.dimmed() }.to_string());
            } else if added {
                out.push_str(&cell.black().on_green().bold().to_string());
            } else {
                out.push_str(&cell.black().on_red().bold().to_string());
            }
        }
        out
    };

    // `label: path`, indented and padded like git.
    let line = |label: &str, text: &str, paint: &dyn Fn(String) -> colored::ColoredString| {
        outln!("        {}", paint(format!("{label:<12}{text}")));
    };
    // Two long names, so the new path goes under the old rather than wrapping one
    // `old -> new`, with only the changed characters marked.
    let rename = |from: &str, to: &str| {
        let (kept_from, kept_to) = crate::diff::common(from, to);
        outln!(
            "        {}{}",
            renamed(format!("{:<12}", "renamed:")),
            marked(from, &kept_from, false)
        );
        outln!(
            "        {}{}",
            renamed("         -> ".to_string()),
            marked(to, &kept_to, true)
        );
    };

    // Group order: deleted → modified → renamed → new (destructive first,
    // additive last); items are already sorted alphabetically within each.
    if !staged.is_empty() {
        heading("Changes to be committed:");
        for p in &staged.removed {
            line("deleted:", p, &deleted);
        }
        for p in &staged.modified {
            line("modified:", p, &modified);
        }
        for (from, to) in &staged.moved {
            rename(from, to);
        }
        for p in &staged.added {
            line("new file:", p, &added);
        }
    }

    if unstaged_changes {
        heading("Changes not staged for commit:");
        outln!(
            "  {}",
            "(use \"stowe add <file>...\" to stage changes)".dimmed()
        );
        for p in &unstaged.removed {
            line("deleted:", p, &deleted);
        }
        for p in &unstaged.modified {
            line("modified:", p, &modified);
        }
        for (from, to) in &unstaged.moved {
            rename(from, to);
        }
    }

    if !unstaged.added.is_empty() {
        heading("Untracked files:");
        outln!(
            "  {}",
            "(use \"stowe add <file>...\" to include in commit)".dimmed()
        );
        for p in &unstaged.added {
            outln!("        {}", added(p.clone()));
        }
    }

    outln!(
        "\n{} {}  {}  {}  {}",
        "summary:".dimmed(),
        format!("+{}", summary.added.len()).green(),
        format!("-{}", summary.removed.len()).red(),
        format!("~{}", summary.modified.len()).yellow(),
        format!("⇄{}", summary.moved.len()).blue(),
    );
}

/// Pretty-print a diff to stdout. Returns false if there was nothing to show.
///
/// `colored` auto-strips the ANSI codes when stdout isn't a terminal, so piping
/// stays clean. Moves print over two lines (old, then an indented `→ new`) so a
/// long rename doesn't smear into one unreadable line.
pub fn print_diff(d: &Diff) -> bool {
    use colored::Colorize;

    if d.is_empty() {
        outln!("{}", "No changes.".dimmed());
        return false;
    }

    let group = |title: colored::ColoredString,
                 items: &[String],
                 paint: &dyn Fn(&str) -> colored::ColoredString| {
        if items.is_empty() {
            return;
        }
        outln!("\n{} {}", title, format!("({})", items.len()).dimmed());
        for i in items {
            outln!("  {}", paint(i));
        }
    };

    group("added".green().bold(), &d.added, &|s| {
        format!("+ {s}").green()
    });
    group("removed".red().bold(), &d.removed, &|s| {
        format!("- {s}").red()
    });
    group("modified".yellow().bold(), &d.modified, &|s| {
        format!("~ {s}").yellow()
    });

    if !d.moved.is_empty() {
        outln!(
            "\n{} {}",
            "moved/renamed".blue().bold(),
            format!("({})", d.moved.len()).dimmed()
        );
        for (from, to) in &d.moved {
            outln!("  {}", from.dimmed());
            outln!("    {} {}", "→".blue(), to.cyan());
        }
    }

    outln!(
        "\n{} {}  {}  {}  {}",
        "summary:".dimmed(),
        format!("+{}", d.added.len()).green(),
        format!("-{}", d.removed.len()).red(),
        format!("~{}", d.modified.len()).yellow(),
        format!("⇄{}", d.moved.len()).blue(),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Entry;

    fn e(path: &str, hash: &str) -> Entry {
        Entry {
            path: path.into(),
            size: hash.len() as u64,
            mtime: 0,
            hash: hash.into(),
            fp: None,
        }
    }

    /// Same as `e`, but carrying an audio fingerprint: the content bytes differ
    /// (a re-tag rewrites the container) while the decoded audio does not.
    fn audio(path: &str, hash: &str, fp: &str) -> Entry {
        Entry {
            fp: Some(fp.into()),
            ..e(path, hash)
        }
    }

    #[test]
    fn added_removed_and_modified_are_classified() {
        let old = vec![
            e("keep.mp3", "h1"),
            e("gone.mp3", "h2"),
            e("edit.mp3", "h3"),
        ];
        let new = vec![
            e("keep.mp3", "h1"),
            e("edit.mp3", "CHANGED"),
            e("new.mp3", "h4"),
        ];
        let d = diff(&old, &new);
        assert_eq!(d.added, ["new.mp3"]);
        assert_eq!(d.removed, ["gone.mp3"]);
        assert_eq!(d.modified, ["edit.mp3"]);
        assert!(d.moved.is_empty());
    }

    #[test]
    fn a_rename_is_a_move_not_a_delete_plus_add() {
        let old = vec![e("old.mp3", "same")];
        let new = vec![e("new.mp3", "same")];
        let d = diff(&old, &new);
        assert_eq!(d.moved, [("old.mp3".to_string(), "new.mp3".to_string())]);
        assert!(d.added.is_empty() && d.removed.is_empty());
    }

    #[test]
    fn a_retagged_and_renamed_song_is_still_a_move() {
        // The reason the audio fingerprint exists: editing a tag changes the
        // file's bytes, so the content hash can't recognise it. The decoded
        // audio is identical, so the fingerprint can.
        let old = vec![audio("old.mp3", "bytes-v1", "audio-fp")];
        let new = vec![audio("Artist - Song.mp3", "bytes-v2", "audio-fp")];
        let d = diff(&old, &new);
        assert_eq!(d.moved.len(), 1, "should pair via fingerprint");
        assert!(d.added.is_empty() && d.removed.is_empty());
    }

    #[test]
    fn an_exact_content_match_wins_over_a_fingerprint_match() {
        let old = vec![audio("a.mp3", "same-bytes", "fp")];
        let new = vec![
            audio("exact.mp3", "same-bytes", "fp"),
            audio("retagged.mp3", "other-bytes", "fp"),
        ];
        let d = diff(&old, &new);
        assert_eq!(d.moved, [("a.mp3".to_string(), "exact.mp3".to_string())]);
        assert_eq!(d.added, ["retagged.mp3"]);
    }

    #[test]
    fn duplicate_content_pairs_one_to_one() {
        // Two identical files moved to two new paths: both are moves, and each
        // old path claims exactly one new path (no collapsing to a single pair).
        let old = vec![e("a1.mp3", "dup"), e("a2.mp3", "dup")];
        let new = vec![e("b1.mp3", "dup"), e("b2.mp3", "dup")];
        let d = diff(&old, &new);
        assert_eq!(d.moved.len(), 2);
        assert!(d.added.is_empty() && d.removed.is_empty());
    }

    #[test]
    fn nothing_changed_means_an_empty_diff() {
        let m = vec![e("a.mp3", "h1"), e("b.mp3", "h2")];
        assert!(diff(&m, &m).is_empty());
    }
}
