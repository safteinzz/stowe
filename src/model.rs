//! On-disk data model: config, file entries, manifests, commits.
//!
//! Everything is plain JSON so the repo (and the remote) stays inspectable
//! with nothing more than `cat`. The remote is a *bare* stowe repo: the same
//! `commits/` + `objects/` + `refs/main`, just without a working tree.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `.stowe/config` - named remotes (`name -> url`) and, optionally, an explicit
/// on-disk format per remote (`name -> "mirror"|"backup"`). A remote with no
/// entry in `formats` falls back to the scheme default (local → mirror, s3 →
/// backup). `serde(default)` keeps configs written before `formats` existed
/// readable.
#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub remotes: BTreeMap<String, String>,
    #[serde(default)]
    pub formats: BTreeMap<String, String>,
}

/// One tracked file in a snapshot.
///
/// `hash` is the blake3 of the file's full content - it *is* the object's name
/// in the store, so identical content is stored once (that's the dedup).
/// `size` + `mtime` are only used to skip re-hashing files that look unchanged.
///
/// `fp` is an optional *audio* fingerprint: the blake3 of the file's decoded
/// PCM, which (unlike `hash`) is stable across tag edits. It's `None` for
/// non-audio files and only ever used by `diff` to recognise a re-tagged song
/// as a move. `serde(default)` keeps manifests written before `fp` existed
/// readable.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub hash: String,
    #[serde(default)]
    pub fp: Option<String>,
}

/// A snapshot of the whole working tree, sorted by path.
pub type Manifest = Vec<Entry>;

/// A commit: a message, a time, the parent it descends from, and the full
/// snapshot of files at that point. Linear history only (one parent, no merges).
#[derive(Serialize, Deserialize, Clone)]
pub struct Commit {
    pub parent: Option<String>,
    pub message: String,
    /// Unix seconds.
    pub time: i64,
    pub files: Manifest,
}
