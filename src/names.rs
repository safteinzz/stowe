//! Portable file names: catch names that external drives (FAT/exFAT/NTFS)
//! can't store *before* they become a raw `Invalid argument` halfway through a
//! push. Human-made names (a stray newline from a copy-paste, a `:` in a song
//! title) are expected input, not an error stowe is allowed to be cryptic about.

use std::path::Path;

/// Printable characters the Windows family of filesystems refuses in names.
const ILLEGAL: &[char] = &['"', '*', ':', '<', '>', '?', '|', '\\'];

/// True if one path component can't be stored: control characters are trouble
/// everywhere; `strict` adds the FAT/exFAT/NTFS set and trailing dots/spaces.
fn bad_component(comp: &str, strict: bool) -> bool {
    comp.chars().any(|c| c.is_control())
        || (strict
            && (comp.chars().any(|c| ILLEGAL.contains(&c)) || comp.ends_with([' ', '.'])))
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
                .map(|c| if strict && ILLEGAL.contains(&c) { '_' } else { c })
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
