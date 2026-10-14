//! Portable file names: catch names that external drives (FAT/exFAT/NTFS)
//! can't store *before* they become a raw `Invalid argument` halfway through a
//! push. Human-made names (a stray newline from a copy-paste, a `:` in a song
//! title) are expected input, not an error stowe is allowed to be cryptic about.

use std::path::Path;

use anyhow::Result;

use crate::prompt::confirm_default_yes;
use crate::repo::Repo;
use crate::scan;
use crate::time::now;
use anyhow::bail;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::model::{Commit, count, short};

/// Printable characters the Windows family of filesystems refuses in names.
const ILLEGAL: &[char] = &['"', '*', ':', '<', '>', '?', '|', '\\'];

/// True if one path component can't be stored: control characters are trouble
/// everywhere; `strict` adds the FAT/exFAT/NTFS set and trailing dots/spaces.
fn bad_component(comp: &str, strict: bool) -> bool {
    comp.chars().any(|c| c.is_control())
        || (strict && (comp.chars().any(|c| ILLEGAL.contains(&c)) || comp.ends_with([' ', '.'])))
}

/// True if any component of a repo-relative path is unstorable.
pub fn unportable(path: &str, strict: bool) -> bool {
    path.split('/').any(|c| bad_component(c, strict))
}

/// Escape control characters so a bad name prints on one readable line
/// (a raw newline in a filename would otherwise split the message in two).
pub fn display(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            '\n' => "⏎".to_string(),
            '\t' => "⇥".to_string(),
            '\r' => "␍".to_string(),
            c if c.is_control() => format!("\\u{{{:x}}}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

/// A safe rename target: control characters dropped; under `strict` the
/// Windows-illegal characters become `_` and trailing dots/spaces are trimmed.
/// Components are cleaned independently so a bad *directory* name heals too.
pub fn sanitize(path: &str, strict: bool) -> String {
    path.split('/')
        .map(|comp| {
            let mut s: String = comp
                .chars()
                .filter(|c| !c.is_control())
                .map(|c| {
                    if strict && ILLEGAL.contains(&c) {
                        '_'
                    } else {
                        c
                    }
                })
                .collect();
            if strict {
                while s.ends_with([' ', '.']) {
                    s.pop();
                }
            }
            let s = s.trim().to_string();
            if s.is_empty() { "_".to_string() } else { s }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Does the filesystem at `root` refuse Windows-illegal characters? Probed
/// empirically, not guessed from mount tables: try to create a file bearing
/// them inside `root/.stowe/`. exFAT/NTFS (and Windows itself) reject it;
/// ext4 and friends accept. Failing to probe assumes the worst.
pub fn probe_restrictive(root: &Path) -> bool {
    let dir = root.join(".stowe");
    if std::fs::create_dir_all(&dir).is_err() {
        return true;
    }
    let probe = dir.join(".probe:*?");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            false
        }
        Err(_) => true,
    }
}

/// Does the filesystem at `root` ignore case (exFAT, FAT, a phone's shared
/// storage)? Probed like [`probe_restrictive`]: a file created in lowercase is
/// looked up in uppercase. Failing to probe assumes it doesn't.
pub fn probe_case_insensitive(root: &Path) -> bool {
    let dir = root.join(".stowe");
    let probe = dir.join(".probe-case");
    if std::fs::create_dir_all(&dir).is_err() || std::fs::write(&probe, b"").is_err() {
        return false;
    }
    let folds = dir.join(".PROBE-CASE").symlink_metadata().is_ok();
    let _ = std::fs::remove_file(&probe);
    folds
}

/// Paths in `manifest`, and the folders holding them, that another path there
/// spells the same apart from case: groups of two or more spellings.
fn case_clashes(manifest: &crate::model::Manifest) -> Vec<Vec<String>> {
    let mut by_fold: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for e in manifest {
        let dirs = e.path.match_indices('/').map(|(slash, _)| &e.path[..slash]);
        for p in dirs.chain([e.path.as_str()]) {
            by_fold.entry(p.to_lowercase()).or_default().insert(p);
        }
    }
    by_fold
        .into_values()
        .filter(|spellings| spellings.len() > 1)
        .map(|spellings| spellings.into_iter().map(str::to_string).collect())
        .collect()
}

/// Warn (non-blocking) when freshly staged names may not be storable on an
/// external drive, so the surprise comes at `add` time, not mid-push weeks
/// later. The strict character set is used: local ext4 may accept these, but
/// a FAT/exFAT/NTFS mirror will not.
pub(crate) fn warn_unportable(d: &scan::Diff) {
    use colored::Colorize;
    let mut bad: Vec<&String> = d.added.iter().filter(|p| unportable(p, true)).collect();
    bad.extend(
        d.moved
            .iter()
            .map(|(_, to)| to)
            .filter(|p| unportable(p, true)),
    );
    if bad.is_empty() {
        return;
    }
    eprintln!(
        "\n{} {} may not be storable on external drives (FAT/exFAT/NTFS):",
        "warning:".yellow().bold(),
        count(bad.len(), "name", "names")
    );
    for p in bad {
        eprintln!("  {}", display(p).yellow());
    }
    eprintln!(
        "{}",
        "(a push to such a drive will offer to rename them)".dimmed()
    );
}

/// Committed names a drive can't hold: groups that differ only in case, on one
/// that ignores case, and names its filesystem refuses.
pub(crate) struct Unstorable {
    pub clashes: Vec<Vec<String>>,
    pub offenders: Vec<String>,
    strict: bool,
    folds: bool,
}

/// Which of HEAD's names the drive at `root` can't store.
pub(crate) fn unstorable(repo: &Repo, root: &Path) -> Result<Unstorable> {
    let strict = probe_restrictive(root);
    let manifest = repo.head_manifest()?;
    // Two names a drive that ignores case can't tell apart would be written to
    // one file there, and the second would overwrite the first.
    let folds = probe_case_insensitive(root);
    let clashes = if folds {
        case_clashes(&manifest)
    } else {
        Vec::new()
    };
    let offenders = manifest
        .iter()
        .map(|e| e.path.clone())
        .filter(|p| unportable(p, strict))
        .collect();
    Ok(Unstorable {
        clashes,
        offenders,
        strict,
        folds,
    })
}

/// Before mirroring, verify every committed name can exist on the target
/// drive. If not: list the offenders readably and offer (default yes) to
/// rename them locally to safe names, recorded as a rename commit, so the
/// push proceeds instead of dying on a raw `Invalid argument` mid-copy.
pub(crate) fn preflight_names(repo: &Repo, name: &str, root: &Path) -> Result<()> {
    use colored::Colorize;

    let found = unstorable(repo, root)?;
    if !found.clashes.is_empty() {
        eprintln!(
            "{} names that differ only in case:",
            "note:".yellow().bold()
        );
        for group in &found.clashes {
            let shown: Vec<String> = group.iter().map(|p| display(p)).collect();
            eprintln!("  {}", shown.join("  /  ").yellow());
        }
        bail!(
            "`{name}` ignores case, so it can't hold both of each pair - rename one, commit, \
             and push again"
        );
    }
    if found.offenders.is_empty() {
        return Ok(());
    }

    eprintln!(
        "{} {} can't be stored on `{name}`:",
        "note:".yellow().bold(),
        count(found.offenders.len(), "committed name", "committed names")
    );
    for p in &found.offenders {
        eprintln!("  {}", display(p).yellow());
    }
    if !confirm_default_yes("Rename them locally to safe names and continue?") {
        bail!("push aborted - fix the names and push again");
    }
    let (renamed, hash) = rename_unportable(repo, &found)?;
    for (old, new) in &renamed {
        outln!("renamed {} -> {new}", display(old));
    }
    outln!("committed {} \"fix: portable file names\"", short(&hash));
    Ok(())
}

/// Rename `found.offenders` in the working tree to names the drive can store
/// and record that as a commit. Returns each `(old, new)` and the commit.
pub(crate) fn rename_unportable(
    repo: &Repo,
    found: &Unstorable,
) -> Result<(Vec<(String, String)>, String)> {
    let manifest = repo.head_manifest()?;
    let strict = found.strict;
    let offenders = &found.offenders;

    // Collision-free targets: sanitize, then bump with " (n)" if taken. Taken
    // ignores case, since `found.strict` drives (exFAT) usually fold it too.
    let mut used: HashSet<String> = manifest.iter().map(|e| e.path.to_lowercase()).collect();
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for old in offenders {
        let base = sanitize(old, strict);
        let mut target = base.clone();
        let mut n = 1;
        while used.contains(&target.to_lowercase()) || repo.root.join(&target).exists() {
            target = bump_name(&base, n);
            n += 1;
        }
        used.insert(target.to_lowercase());
        map.insert(old.clone(), target);
    }
    let mut files = manifest;
    for e in &mut files {
        if let Some(new) = map.get(&e.path) {
            e.path = new.clone();
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    // A bump only renames the file, so a sanitized folder can still meet one
    // spelled the same apart from case.
    let clashes = if found.folds {
        case_clashes(&files)
    } else {
        Vec::new()
    };
    if !clashes.is_empty() {
        let pairs: Vec<String> = clashes
            .iter()
            .map(|g| {
                g.iter()
                    .map(|p| display(p))
                    .collect::<Vec<_>>()
                    .join("  /  ")
            })
            .collect();
        bail!(
            "the safe names would clash with names already here once case is ignored: {}; \
             rename one of each by hand, commit, and push again",
            pairs.join(", ")
        );
    }

    // Rename on disk (pruning any directory the rename empties).
    for old in map.keys() {
        if !repo.root.join(old).exists() {
            bail!(
                "`{}` is no longer in the working tree - commit your changes, then push again",
                display(old)
            );
        }
    }
    for (old, new) in &map {
        let src = repo.root.join(old);
        let dst = repo.root.join(new);
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::rename(&src, &dst)?;
        let mut dir = src.parent();
        while let Some(d) = dir {
            if d == repo.root || std::fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }

    // Record the renames as a commit. Content is untouched (same hashes), so
    // history reads them as moves. A rename keeps size+mtime, so the next scan
    // still cache-hits on these entries.
    let commit = Commit {
        parent: repo.head()?,
        message: "fix: portable file names".to_string(),
        time: now(),
        files,
    };
    let hash = repo.write_commit(&commit)?;
    repo.set_head(&hash)?;

    // If a snapshot is staged, carry the renames into it too, so committing it
    // later doesn't resurrect the old paths as delete+add.
    if let Some(mut idx) = repo.read_index()? {
        for e in &mut idx {
            if let Some(new) = map.get(&e.path) {
                e.path = new.clone();
            }
        }
        idx.sort_by(|a, b| a.path.cmp(&b.path));
        repo.write_index(&idx)?;
    }
    Ok((map.into_iter().collect(), hash))
}

/// `dir/name.mp3` -> `dir/name (n).mp3` (extension kept; no extension, append).
pub(crate) fn bump_name(path: &str, n: usize) -> String {
    let (dir, file) = match path.rsplit_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, path),
    };
    let bumped = match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{file} ({n})"),
    };
    match dir {
        Some(d) => format!("{d}/{bumped}"),
        None => bumped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_safe_name_never_matches_an_existing_one_ignoring_case() {
        // exFAT both refuses `?` and ignores case, so `Song_.mp3` beside
        // `song_.mp3` was one file there and the push lost a song.
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repo::init(tmp.path()).unwrap();
        let mut files = Vec::new();
        for (path, body) in [("song_.mp3", "a"), ("Song?.mp3", "b")] {
            std::fs::write(tmp.path().join(path), body).unwrap();
            files.push(crate::model::Entry {
                path: path.to_string(),
                size: 1,
                mtime: 0,
                hash: body.to_string(),
                fp: None,
            });
        }
        let commit = Commit {
            parent: None,
            message: "songs".to_string(),
            time: 0,
            files,
        };
        repo.set_head(&repo.write_commit(&commit).unwrap()).unwrap();
        let found = Unstorable {
            clashes: Vec::new(),
            offenders: vec!["Song?.mp3".to_string()],
            strict: true,
            folds: false,
        };

        let (renamed, _) = rename_unportable(&repo, &found).unwrap();
        let new = &renamed[0].1;
        assert_ne!(
            new.to_lowercase(),
            "song_.mp3",
            "`Song?.mp3` was renamed to `{new}`, which a drive that ignores case reads as `song_.mp3`"
        );
    }

    #[test]
    fn control_characters_are_never_portable() {
        // The real one: a song whose name began with a newline. Illegal on
        // exFAT even though ext4 stored it happily.
        assert!(unportable("Music/\nt.A.T.u. - Remix", false));
        assert!(unportable("a\tb.mp3", true));
    }

    #[test]
    fn windows_illegal_characters_only_matter_in_strict_mode() {
        assert!(unportable("Album: The Best.mp3", true));
        assert!(!unportable("Album: The Best.mp3", false), "fine on ext4");
        for bad in ['"', '*', ':', '<', '>', '?', '|', '\\'] {
            assert!(
                unportable(&format!("x{bad}.mp3"), true),
                "{bad} should fail"
            );
        }
    }

    #[test]
    fn trailing_dots_and_spaces_are_strict_only() {
        assert!(unportable("song .mp3/x", true) || unportable("dir /x", true));
        assert!(unportable("trailing.", true));
        assert!(!unportable("trailing.", false));
    }

    #[test]
    fn ordinary_names_are_left_alone() {
        assert!(!unportable("Music/Artist/Song (Remix).mp3", true));
        assert!(!unportable("Nas Ne Dogonyat.mp3", true));
    }

    #[test]
    fn display_escapes_control_characters_onto_one_line() {
        // Printing the raw name would split the error message in half.
        assert_eq!(display("a\nb"), "a⏎b");
        assert!(!display("a\nb").contains('\n'));
    }

    #[test]
    fn sanitize_produces_a_storable_name() {
        assert_eq!(sanitize("Music/\nsong.mp3", true), "Music/song.mp3");
        assert_eq!(sanitize("Album: Best.mp3", true), "Album_ Best.mp3");
        // Directory components are healed independently.
        assert_eq!(sanitize("bad:dir/ok.mp3", true), "bad_dir/ok.mp3");
        // And the result is, by definition, portable.
        for raw in ["a\nb.mp3", "x:y?z.mp3", "trailing. "] {
            assert!(!unportable(&sanitize(raw, true), true), "{raw}");
        }
    }

    #[test]
    fn sanitize_never_yields_an_empty_component() {
        assert_eq!(sanitize("\n\n", true), "_");
    }
}
