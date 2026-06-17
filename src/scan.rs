//! Scanning the working tree into a manifest, and diffing two manifests.

use anyhow::Result;
use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

use crate::model::{Entry, Manifest};
use crate::repo::Repo;

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

/// Walk the working tree and build a fresh, path-sorted manifest.
///
/// `cache_source` (typically HEAD's manifest) provides hashes we can reuse for
/// files that look unchanged, so only new/modified files are actually read.
pub fn scan(repo: &Repo, cache_source: &Manifest) -> Result<Manifest> {
    let cache = cache_from(cache_source);
    let mut out: Manifest = Vec::new();

    for entry in WalkDir::new(&repo.root).into_iter().filter_entry(|e| {
        // Never descend into our own metadata directory.
        e.file_name() != ".stowe"
    }) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path();
        let rel = abs
            .strip_prefix(&repo.root)
            .unwrap_or(abs)
            .to_string_lossy()
            .replace('\\', "/");

        let meta = entry.metadata()?;
        let size = meta.len();
        let mtime = meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Reuse the cached hash *and* fingerprint when the file looks unchanged;
        // otherwise re-read it: hash the bytes, and (for audio) fingerprint the
        // decoded PCM.
        let (hash, fp) = match cache.get(&rel) {
            Some(c) if c.size == size && c.mtime == mtime => (c.hash.clone(), c.fp.clone()),
            _ => (hash_file(abs)?, crate::audio::fingerprint(abs)?),
        };

        out.push(Entry {
            path: rel,
            size,
            mtime,
            hash,
            fp,
        });
    }

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
/// interesting work is the rest — paths that vanished and paths that appeared
/// — which we try to pair up as moves before declaring them removed/added:
///
///  1. **exact content** (same `hash`) — a plain rename, any file type;
///  2. **same audio** (same `fp`) — a song that was renamed *and* re-tagged,
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
    // for what's left — so a perfect content match always wins over an fp one.
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

/// Pretty-print a diff to stdout. Returns false if there was nothing to show.
pub fn print_diff(d: &Diff) -> bool {
    if d.is_empty() {
        println!("No changes.");
        return false;
    }
    let group = |label: &str, items: &[String]| {
        if !items.is_empty() {
            println!("\n{label} ({}):", items.len());
            for i in items {
                println!("  {i}");
            }
        }
    };
    group("🟢 added", &d.added);
    group("🔴 removed", &d.removed);
    group("🟡 modified", &d.modified);
    if !d.moved.is_empty() {
        println!("\n🔵 moved/renamed ({}):", d.moved.len());
        for (from, to) in &d.moved {
            println!("  {from}  ->  {to}");
        }
    }
    println!(
        "\nsummary: +{} added, -{} removed, ~{} modified, {} moved",
        d.added.len(),
        d.removed.len(),
        d.modified.len(),
        d.moved.len()
    );
    true
}
