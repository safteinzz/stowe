//! The status level's lower pane: the selected file's diff, written the way
//! `git diff` writes it so `tui::highlight` can draw it, and the two
//! files `git difftool` is handed for the same comparison.

use std::path::{Path, PathBuf};

use similar::TextDiff;

use super::load::Side;
use crate::history::{Change, Kind};
use crate::kept;
use crate::model::Manifest;
use crate::repo::Repo;
use crate::tui::highlight::{Blob, DiffContext};

/// Lines of context around each change, as `git diff` gives.
const CONTEXT: usize = 3;

/// One side of a change, as far as this machine knows it.
enum Body {
    Absent,
    Text(String),
    /// Not text, or too large to keep: only its size is known.
    Opaque(u64),
    /// Small enough to keep, but no copy was kept: staged before copies were
    /// kept, or not text.
    Unkept(u64),
}

/// Both sides of one status row's file, and the paths they had.
pub struct Sides {
    old: Body,
    new: Body,
    old_path: String,
    new_path: String,
    /// Both sides hold the same bytes, text or not.
    same: bool,
}

/// `[staged]`, `[worktree]` or `[untracked]`: which comparison the pane shows.
pub fn tag(side: Side) -> &'static str {
    match side {
        Side::Staged => "[staged]",
        Side::Unstaged => "[worktree]",
        Side::Untracked => "[untracked]",
    }
}

/// What `change`, on `side` of the index, holds on each side: HEAD against the
/// index for a staged row, the index against the disk for any other.
pub fn sides(repo: &Repo, side: Side, change: &Change) -> anyhow::Result<Sides> {
    let head = repo.head_manifest()?;
    let index = repo.read_index()?;
    let base = index.as_ref().unwrap_or(&head);
    let old_path = change.from.clone().unwrap_or_else(|| change.path.clone());
    let hash = |m: &Manifest, p: &str| m.iter().find(|e| e.path == p).map(|e| e.hash.clone());
    let (old, new, same) = match side {
        Side::Staged => (
            stored(repo, &head, &old_path),
            stored(repo, base, &change.path),
            hash(&head, &old_path).is_some_and(|h| hash(base, &change.path) == Some(h)),
        ),
        // `status` never fingerprints, so a move off the index is a hash match.
        Side::Unstaged | Side::Untracked => (
            stored(repo, base, &old_path),
            on_disk(repo, &change.path),
            change.kind == Kind::Moved,
        ),
    };
    Ok(Sides {
        old,
        new,
        old_path,
        new_path: change.path.clone(),
        same,
    })
}

fn stored(repo: &Repo, manifest: &Manifest, path: &str) -> Body {
    let Some(e) = manifest.iter().find(|e| e.path == path) else {
        return Body::Absent;
    };
    if e.size > kept::MAX {
        return Body::Opaque(e.size);
    }
    match kept::read(repo, &e.hash) {
        Some(text) => Body::Text(text),
        None => Body::Unkept(e.size),
    }
}

fn on_disk(repo: &Repo, path: &str) -> Body {
    let abs = repo.root.join(path);
    let Ok(meta) = fs_err::metadata(&abs) else {
        return Body::Absent;
    };
    if meta.len() > kept::MAX {
        return Body::Opaque(meta.len());
    }
    match fs_err::read(&abs) {
        Ok(bytes) if kept::is_text(&bytes) => Body::Text(String::from_utf8_lossy(&bytes).into()),
        _ => Body::Opaque(meta.len()),
    }
}

/// The diff as `git diff` would print it, and both sides whole for the
/// highlighter. Anything that is not a diff is a few lines of words, which the
/// renderer shows as they are.
pub fn raw(s: &Sides) -> (String, DiffContext) {
    let words = |text: &str| (text.to_string(), DiffContext::default());
    let unchanged = s.same || matches!((&s.old, &s.new), (Body::Text(a), Body::Text(b)) if a == b);
    if unchanged && s.old_path != s.new_path {
        return words("moved, its contents unchanged");
    }
    if unchanged {
        return words("its contents are unchanged");
    }
    let (old, new) = match (&s.old, &s.new) {
        (Body::Text(a), Body::Text(b)) => (Some(a), Some(b)),
        (Body::Absent, Body::Text(b)) => (None, Some(b)),
        (Body::Text(a), Body::Absent) => (Some(a), None),
        (Body::Unkept(_), Body::Text(b)) => {
            let (diff, ctx) = unified(s, None, Some(b));
            let note =
                "no copy of the earlier version is kept here, so every line shows as new\n\n";
            return (format!("{note}{diff}"), ctx);
        }
        (Body::Text(_), Body::Unkept(_)) => {
            return words(
                "no copy of the staged version is kept here: unstage it and stage it again to \
                 see what changed",
            );
        }
        (Body::Absent, Body::Absent) => return words("nothing here to compare"),
        (a @ (Body::Unkept(_) | Body::Absent), b @ (Body::Unkept(_) | Body::Absent)) => {
            return words(&format!("no text copy kept here: {}", sizes(a, b)));
        }
        (a, b) => return words(&format!("not text: {}", sizes(a, b))),
    };
    unified(s, old, new)
}

fn unified(s: &Sides, old: Option<&String>, new: Option<&String>) -> (String, DiffContext) {
    let (a, b) = (old.map_or("", |t| t), new.map_or("", |t| t));
    let mut out = format!("diff --git a/{} b/{}\n", s.old_path, s.new_path);
    match (old, new) {
        (None, _) => out.push_str("new file\n"),
        (_, None) => out.push_str("deleted file\n"),
        _ if s.old_path != s.new_path => out.push_str(&format!(
            "rename from {}\nrename to {}\n",
            s.old_path, s.new_path
        )),
        _ => {}
    }
    let from = old.map_or("/dev/null".to_string(), |_| format!("a/{}", s.old_path));
    let to = new.map_or("/dev/null".to_string(), |_| format!("b/{}", s.new_path));
    let diff = TextDiff::from_lines(a, b);
    out.push_str(
        &diff
            .unified_diff()
            .context_radius(CONTEXT)
            .header(&from, &to)
            .to_string(),
    );
    let blob = |t: Option<&String>| -> Option<Blob> {
        let t = t?.clone();
        Some(Box::new(move || Some(t)))
    };
    (
        out,
        DiffContext {
            old: blob(old),
            new: blob(new),
        },
    )
}

/// `3.2 MB → 3.4 MB`, `new, 3.4 MB`, `removed, 3.2 MB`.
fn sizes(old: &Body, new: &Body) -> String {
    let size = |b: &Body| match b {
        Body::Absent => None,
        Body::Text(t) => Some(t.len() as u64),
        Body::Opaque(n) | Body::Unkept(n) => Some(*n),
    };
    match (size(old), size(new)) {
        (Some(a), Some(b)) if a == b => format!("{}, the same size", human(b)),
        (Some(a), Some(b)) => format!("{} → {}", human(a), human(b)),
        (None, Some(b)) => format!("new, {}", human(b)),
        (Some(a), None) => format!("removed, {}", human(a)),
        (None, None) => String::new(),
    }
}

fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit + 1 < UNITS.len() {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// What Enter opens for a row.
pub enum Open {
    /// Both versions, for `git difftool`.
    Diff(PathBuf, PathBuf),
    /// The working file alone, for the editor: there is no other version here.
    Edit(PathBuf),
    /// Nothing can be opened, and why.
    Nothing(String),
}

/// What Enter opens for `s`. Both versions go to the difftool, the working file
/// itself when a side is on disk, else its kept copy written under `scratch`
/// with its own name so the tool picks the right syntax. With only one
/// version here, the working file goes to the editor.
pub fn open(repo: &Repo, s: &Sides, side: Side, scratch: &Path) -> Open {
    let working = repo.root.join(&s.new_path);
    let (Body::Text(a), Body::Text(b)) = (&s.old, &s.new) else {
        return match on_disk(repo, &s.new_path) {
            Body::Text(_) => Open::Edit(working),
            Body::Absent => Open::Nothing("it is deleted, so there is no file to open".into()),
            _ => Open::Nothing("it is not text, so there is nothing to read".into()),
        };
    };
    let write = |dir: &str, path: &str, text: &str| -> Result<PathBuf, String> {
        let leaf = path.rsplit('/').next().unwrap_or(path);
        let at = scratch.join(dir).join(leaf);
        fs_err::create_dir_all(scratch.join(dir))
            .and_then(|()| fs_err::write(&at, text))
            .map_err(|_| {
                format!(
                    "could not write a copy for the difftool in `{}`",
                    scratch.display()
                )
            })?;
        Ok(at)
    };
    let new = match side {
        Side::Staged => write("b", &s.new_path, b),
        Side::Unstaged | Side::Untracked => Ok(working),
    };
    match (write("a", &s.old_path, a), new) {
        (Ok(old), Ok(new)) => Open::Diff(old, new),
        (Err(why), _) | (_, Err(why)) => Open::Nothing(why),
    }
}
