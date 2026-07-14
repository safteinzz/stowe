//! Scanning the working tree into a manifest, and diffing two manifests.

use anyhow::Result;
use rayon::prelude::*;
use std::collections::HashMap;
use std::io::{BufReader, IsTerminal, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

use crate::model::{Entry, Manifest};
use crate::repo::Repo;

/// A one-line, in-place progress indicator for the long phases of a scan.
///
/// Big trees make `status`/`add` sit for a few seconds; without a sign of life
/// that reads as a hang. So we stream a live count to *stderr*, and only when
/// it's a terminal - piped/redirected output stays byte-for-byte clean, and the
/// real result always goes to stdout. Ticking is throttled by the caller so the
/// print cost never shows up in the timing.
pub struct Progress {
    show: bool,
}

impl Progress {
    pub fn new() -> Self {
        Progress {
            show: std::io::stderr().is_terminal(),
        }
    }
    /// Overwrite the current line with `msg` (only if attached to a terminal).
    pub fn tick(&self, msg: &str) {
        if self.show {
            eprint!("\r\x1b[K{msg}");
            let _ = std::io::stderr().flush();
        }
    }
    /// Wipe the progress line so it doesn't linger above the real output.
    pub fn clear(&self) {
        if self.show {
            eprint!("\r\x1b[K");
            let _ = std::io::stderr().flush();
        }
    }
}

/// blake3 of a file's full content, streamed so we never load big files whole.
pub fn hash_file(path: &std::path::Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut reader = BufReader::new(std::fs::File::open(path)?);
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

/// Repo-relative, forward-slash path string for `abs` under `root`. Purely
/// lexical, so it also works for files that no longer exist (staged deletions).
pub fn rel_path(root: &std::path::Path, abs: &std::path::Path) -> String {
    abs.strip_prefix(root)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Build a single manifest [`Entry`] for an existing file (used by per-file
/// `add`). `fingerprint` decodes audio to record its fingerprint, same as a
/// full scan.
pub fn entry_for(root: &std::path::Path, abs: &std::path::Path, fingerprint: bool) -> Result<Entry> {
    let meta = std::fs::metadata(abs)?;
    let mtime = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let fp = if fingerprint {
        crate::audio::fingerprint(abs)?
    } else {
        None
    };
    Ok(Entry {
        path: rel_path(root, abs),
        size: meta.len(),
        mtime,
        hash: hash_file(abs)?,
        fp,
    })
}

/// All tracked files under `dir` (recursive), as absolute paths, skipping the
/// `.stowe` metadata dir. Used to stage a directory argument.
pub fn files_under(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".stowe")
    {
        let entry = entry?;
        if entry.file_type().is_file() {
            out.push(entry.path().to_path_buf());
        }
    }
    Ok(out)
}

/// Cached entry reused when a file's size+mtime are unchanged, so we skip both
/// re-hashing *and* re-fingerprinting it. Keyed by relative path.
struct Cached {
    size: u64,
    mtime: i64,
    hash: String,
    fp: Option<String>,
}

/// Build the size/mtime reuse cache from a manifest (typically HEAD's).
fn cache_from(manifest: &Manifest) -> HashMap<String, Cached> {
    manifest
        .iter()
        .map(|e| {
            (
                e.path.clone(),
                Cached {
                    size: e.size,
                    mtime: e.mtime,
                    hash: e.hash.clone(),
                    fp: e.fp.clone(),
                },
            )
        })
        .collect()
}

/// One walked working-tree file, with the cheap metadata (size+mtime) already
/// captured by the walk. Content hashing is what's deferred to the parallel
/// pass in [`scan`].
struct Found {
    rel: String,
    abs: std::path::PathBuf,
    size: u64,
    mtime: i64,
}

/// Recursively collect every file under `dir` into `out`, skipping `.stowe`.
///
/// Uses `read_dir` + [`std::fs::DirEntry::metadata`], so each file's size/mtime
/// comes from an `fstatat` relative to the already-open directory fd. That
/// matters enormously on deep trees over FUSE/network mounts: WalkDir's
/// `metadata()` instead `stat`s the full absolute path, making the kernel
/// re-resolve *every* directory component per file - e.g. ~5× slower on an
/// NTFS-over-FUSE tree 10+ levels deep. Symlinks (and other non-regular
/// entries) are skipped, matching the old non-following WalkDir behavior.
fn collect_files(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<Found>,
    prog: &Progress,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == std::ffi::OsStr::new(".stowe") {
            continue;
        }
        let ft = entry.file_type()?;
        if ft.is_dir() {
            collect_files(root, &entry.path(), out, prog)?;
        } else if ft.is_file() {
            let meta = entry.metadata()?;
            let mtime = meta
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let abs = entry.path();
            out.push(Found {
                rel: rel_path(root, &abs),
                abs,
                size: meta.len(),
                mtime,
            });
            // Cheap to print, but throttle so it's ~invisible in the timing.
            if out.len().is_multiple_of(1000) {
                prog.tick(&format!("scanning... {} files", out.len()));
            }
        }
    }
    Ok(())
}

/// Walk the working tree and build a fresh, path-sorted manifest.
///
/// `cache_source` (typically HEAD's manifest) provides hashes we can reuse for
/// files that look unchanged, so only new/modified files are actually read.
///
/// `fingerprint` controls whether *new/changed* audio files are decoded to
/// produce their audio fingerprint. That decode is by far the most expensive
/// thing stowe does, so `status` skips it (`false`) and just hashes - plain
/// renames are still caught by hash. `add` passes `true` to record fingerprints
/// in the snapshot, which is what lets a later re-tag+rename read as a move.
pub fn scan(repo: &Repo, cache_source: &Manifest, fingerprint: bool) -> Result<Manifest> {
    let cache = cache_from(cache_source);
    let prog = Progress::new();

    // Walk the tree, capturing each file's size+mtime via a dirfd-relative stat
    // (see `collect_files`). This is the dominant cost of a clean-tree `status`,
    // and - because it goes through a single FUSE daemon on mounted drives -
    // parallelising it only adds contention, so the walk stays sequential.
    let mut found: Vec<Found> = Vec::new();
    collect_files(&repo.root, &repo.root, &mut found, &prog)?;

    // Content hashing *is* worth parallelising (CPU-bound, per-file independent),
    // so fan it across cores. A cache hit (unchanged size+mtime) reuses the
    // stored hash/fingerprint and reads no content - so a clean tree does no I/O
    // here at all. `hashed` counts only the files we actually read, which is the
    // slow work worth reporting (a re-`add` of a big library, cold cache, etc.).
    let hashed = AtomicUsize::new(0);
    let mut out: Manifest = found
        .par_iter()
        .map(|f| -> Result<Entry> {
            let (hash, fp) = match cache.get(&f.rel) {
                Some(c) if c.size == f.size && c.mtime == f.mtime => (c.hash.clone(), c.fp.clone()),
                _ => {
                    let hash = hash_file(&f.abs)?;
                    let fp = if fingerprint {
                        crate::audio::fingerprint(&f.abs)?
                    } else {
                        None
                    };
                    let n = hashed.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_multiple_of(256) {
                        let what = if fingerprint { "hashing + fingerprinting" } else { "hashing" };
                        prog.tick(&format!("{what}... {n} files"));
                    }
                    (hash, fp)
                }
            };
            Ok(Entry {
                path: f.rel.clone(),
                size: f.size,
                mtime: f.mtime,
                hash,
                fp,
            })
        })
        .collect::<Result<_>>()?;

    prog.clear();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
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
    let old_by_path: HashMap<&str, &Entry> =
        old.iter().map(|e| (e.path.as_str(), e)).collect();
    let new_by_path: HashMap<&str, &Entry> =
        new.iter().map(|e| (e.path.as_str(), e)).collect();

    let mut d = Diff::default();

    // Same path, different bytes → modified in place.
    for e in new {
        if let Some(old_e) = old_by_path.get(e.path.as_str())
            && old_e.hash != e.hash
        {
            d.modified.push(e.path.clone());
        }
    }

    // Paths that exist on only one side are move/rename candidates.
    let gone: Vec<&Entry> = old
        .iter()
        .filter(|e| !new_by_path.contains_key(e.path.as_str()))
        .collect();
    let fresh: Vec<&Entry> = new
        .iter()
        .filter(|e| !old_by_path.contains_key(e.path.as_str()))
        .collect();

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
        if let Some(fi) = by_hash.get_mut(g.hash.as_str()).and_then(|q| claim(q, &taken)) {
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

/// Render a git-style `status`: a staged section (HEAD → index), an unstaged
/// section and an untracked list (both from index → working tree), then the
/// summary line. Only `main` exists, so the branch is always `main`.
pub fn print_status(staged: &Diff, unstaged: &Diff, summary: &Diff) {
    use colored::Colorize;

    println!("On branch {}", "main".green());

    let unstaged_changes =
        !unstaged.modified.is_empty() || !unstaged.removed.is_empty() || !unstaged.moved.is_empty();
    if staged.is_empty() && !unstaged_changes && unstaged.added.is_empty() {
        println!("nothing to commit, working tree clean");
        return;
    }

    // Colour is by *change type*, not by section: added/new is green, modified
    // yellow, deleted red, renamed blue - consistent everywhere it appears.
    let added = |s: String| s.green();
    let modified = |s: String| s.yellow();
    let deleted = |s: String| s.red();
    let renamed = |s: String| s.blue();

    // `label: path`, indented and padded like git.
    let line = |label: &str, text: &str, paint: &dyn Fn(String) -> colored::ColoredString| {
        println!("        {}", paint(format!("{label:<12}{text}")));
    };
    // Renames carry two long names, so split them over two aligned lines (the
    // new path under the old) instead of one wrapping `old -> new`.
    let rename = |from: &str, to: &str| {
        println!("        {}", renamed(format!("{:<12}{from}", "renamed:")));
        println!("        {}", renamed(format!("         -> {to}")));
    };

    // Group order: deleted → modified → renamed → new (destructive first,
    // additive last); items are already sorted alphabetically within each.
    if !staged.is_empty() {
        println!("\nChanges to be committed:");
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
        println!("\nChanges not staged for commit:");
        println!("  {}", "(use \"stowe add <file>...\" to stage changes)".dimmed());
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
        println!("\nUntracked files:");
        println!("  {}", "(use \"stowe add <file>...\" to include in commit)".dimmed());
        for p in &unstaged.added {
            println!("        {}", added(p.clone()));
        }
    }

    println!(
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
        println!("{}", "No changes.".dimmed());
        return false;
    }

    let group = |title: colored::ColoredString, items: &[String], paint: &dyn Fn(&str) -> colored::ColoredString| {
        if items.is_empty() {
            return;
        }
        println!("\n{} {}", title, format!("({})", items.len()).dimmed());
        for i in items {
            println!("  {}", paint(i));
        }
    };

    group("added".green().bold(), &d.added, &|s| format!("+ {s}").green());
    group("removed".red().bold(), &d.removed, &|s| format!("- {s}").red());
    group("modified".yellow().bold(), &d.modified, &|s| {
        format!("~ {s}").yellow()
    });

    if !d.moved.is_empty() {
        println!(
            "\n{} {}",
            "moved/renamed".blue().bold(),
            format!("({})", d.moved.len()).dimmed()
        );
        for (from, to) in &d.moved {
            println!("  {}", from.dimmed());
            println!("    {} {}", "→".blue(), to.cyan());
        }
    }

    println!(
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
        let old = vec![e("keep.mp3", "h1"), e("gone.mp3", "h2"), e("edit.mp3", "h3")];
        let new = vec![e("keep.mp3", "h1"), e("edit.mp3", "CHANGED"), e("new.mp3", "h4")];
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
