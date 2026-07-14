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
            assert!(unportable(&format!("x{bad}.mp3"), true), "{bad} should fail");
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
