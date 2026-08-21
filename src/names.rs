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
use std::collections::{BTreeMap, HashSet};

use crate::model::{Commit, short};

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
        "\n{} {} name(s) may not be storable on external drives (FAT/exFAT/NTFS):",
        "warning:".yellow().bold(),
        bad.len()
    );
    for p in bad {
        eprintln!("  {}", display(p).yellow());
    }
    eprintln!(
        "{}",
        "(a push to such a drive will offer to rename them)".dimmed()
    );
}

/// Before mirroring, verify every committed name can exist on the target
/// drive. If not: list the offenders readably and offer (default yes) to
/// rename them locally to safe names, recorded as a rename commit, so the
/// push proceeds instead of dying on a raw `Invalid argument` mid-copy.
pub(crate) fn preflight_names(repo: &Repo, name: &str, root: &Path) -> Result<()> {
    use colored::Colorize;

    let strict = probe_restrictive(root);
    let manifest = repo.head_manifest()?;
    let offenders: Vec<String> = manifest
        .iter()
        .map(|e| e.path.clone())
        .filter(|p| unportable(p, strict))
        .collect();
    if offenders.is_empty() {
        return Ok(());
    }

    eprintln!(
        "{} {} committed name(s) can't be stored on `{name}`:",
        "note:".yellow().bold(),
        offenders.len()
    );
    for p in &offenders {
        eprintln!("  {}", display(p).yellow());
    }
    if !confirm_default_yes("Rename them locally to safe names and continue?") {
        bail!("push aborted - fix the names and push again");
    }

    // Collision-free targets: sanitize, then bump with " (n)" if taken.
    let mut used: HashSet<String> = manifest.iter().map(|e| e.path.clone()).collect();
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for old in &offenders {
        let base = sanitize(old, strict);
        let mut target = base.clone();
        let mut n = 1;
        while used.contains(&target) {
            target = bump_name(&base, n);
            n += 1;
        }
        used.insert(target.clone());
        map.insert(old.clone(), target);
    }

    // Rename on disk (pruning any directory the rename empties).
    for (old, new) in &map {
        let src = repo.root.join(old);
        if !src.exists() {
            bail!(
                "`{}` is no longer in the working tree - commit your changes, then push again",
                display(old)
            );
        }
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
        println!("renamed {} -> {new}", display(old));
    }

    // Record the renames as a commit. Content is untouched (same hashes), so
    // history reads them as moves. A rename keeps size+mtime, so the next scan
    // still cache-hits on these entries.
    let mut files = manifest;
    for e in &mut files {
        if let Some(new) = map.get(&e.path) {
            e.path = new.clone();
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let commit = Commit {
        parent: repo.head()?,
        message: "fix: portable file names".to_string(),
        time: now(),
        files,
    };
    let hash = repo.write_commit(&commit)?;
    repo.set_head(&hash)?;
    println!("committed {} \"fix: portable file names\"", short(&hash));

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
    Ok(())
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
