//! How a path is shown to a person.
//!
//! A remote root is stored and used absolute - that is the only form that is
//! unambiguous once a drive is mounted somewhere else. But printing
//! `/home/you/Archive/drive` in every message wastes half a terminal line on
//! the part the reader already knows, so anything under `$HOME` is shown with
//! the `~` a shell would use. Display only: nothing here is ever parsed back.

use std::path::{Path, PathBuf};

/// `$HOME/Music/drive` as `~/Music/drive`; anything else unchanged.
pub fn short(path: &Path) -> String {
    short_under(path, std::env::var_os("HOME").as_deref())
}

/// The decision on its own, so it can be checked without setting `$HOME` on a
/// running process.
fn short_under(path: &Path, home: Option<&std::ffi::OsStr>) -> String {
    let text = path.to_string_lossy();
    let Some(home) = home else {
        return text.into_owned();
    };
    let home = home.to_string_lossy();
    // A `$HOME` of `/` (or empty) would turn every absolute path into `~...`.
    if home.len() < 2 || !home.starts_with('/') {
        return text.into_owned();
    }
    let home = home.trim_end_matches('/');
    match text.strip_prefix(home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        // `/home/youssef` must not match a `$HOME` of `/home/you`.
        _ => text.into_owned(),
    }
}

/// The other direction: `~/drive` as `$HOME/drive`, so a path a person typed
/// (or copied out of one of the messages above) names the same place a stored
/// absolute path does. A `~` that isn't the whole first segment is left alone -
/// it is a legal file name, and only a shell's leading `~` means "home".
pub fn expand(path: &str) -> PathBuf {
    expand_under(path, std::env::var_os("HOME").as_deref())
}

fn expand_under(path: &str, home: Option<&std::ffi::OsStr>) -> PathBuf {
    let Some(rest) = path.strip_prefix('~') else {
        return PathBuf::from(path);
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        return PathBuf::from(path);
    }
    match home {
        Some(home) => PathBuf::from(home).join(rest.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

/// The same, for a remote URL: only the path part of a local URL is shortened,
/// the scheme is left exactly as the user typed it, and an `s3://` URL has no
/// local path to shorten at all.
pub fn short_url(url: &str) -> String {
    short_url_under(url, std::env::var_os("HOME").as_deref())
}

fn short_url_under(url: &str, home: Option<&std::ffi::OsStr>) -> String {
    match url.strip_prefix("local:") {
        Some(path) => format!("local:{}", short_under(Path::new(path), home)),
        None if url.contains("://") => url.to_string(),
        None => short_under(Path::new(url), home),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn home(h: &str) -> Option<&OsStr> {
        Some(OsStr::new(h))
    }

    #[test]
    fn home_collapses_to_tilde() {
        assert_eq!(
            short_under(Path::new("/home/you/drive"), home("/home/you")),
            "~/drive"
        );
        assert_eq!(short_under(Path::new("/home/you"), home("/home/you")), "~");
    }

    #[test]
    fn a_longer_user_name_is_not_a_prefix_match() {
        // `/home/youssef` starts with `/home/you` as a string, but is somebody
        // else's home.
        assert_eq!(
            short_under(Path::new("/home/youssef/drive"), home("/home/you")),
            "/home/youssef/drive"
        );
    }

    #[test]
    fn a_useless_home_is_ignored() {
        // A `$HOME` of `/` would otherwise turn every absolute path into `~...`.
        for h in ["/", ""] {
            assert_eq!(short_under(Path::new("/mnt/drive"), home(h)), "/mnt/drive");
        }
        assert_eq!(short_under(Path::new("/mnt/drive"), None), "/mnt/drive");
    }

    #[test]
    fn paths_outside_home_are_untouched() {
        assert_eq!(
            short_under(Path::new("/mnt/backup"), home("/home/you")),
            "/mnt/backup"
        );
    }

    #[test]
    fn expand_puts_home_back() {
        assert_eq!(
            expand_under("~/drive", home("/home/you")),
            PathBuf::from("/home/you/drive")
        );
        assert_eq!(
            expand_under("~", home("/home/you")),
            PathBuf::from("/home/you")
        );
    }

    #[test]
    fn a_tilde_that_is_not_the_home_segment_is_a_file_name() {
        // `~` is legal in a file name; only a leading one means home.
        assert_eq!(
            expand_under("~backup/drive", home("/home/you")),
            PathBuf::from("~backup/drive")
        );
        assert_eq!(
            expand_under("/mnt/~/drive", home("/home/you")),
            PathBuf::from("/mnt/~/drive")
        );
    }

    #[test]
    fn short_and_expand_round_trip() {
        // The property that matters: a path printed to the user, typed back in,
        // names the same place. Anything else writes a backup somewhere other
        // than where the message said it went.
        for p in ["/home/you/drive", "/home/you", "/mnt/backup"] {
            let shown = short_under(Path::new(p), home("/home/you"));
            assert_eq!(expand_under(&shown, home("/home/you")), PathBuf::from(p));
        }
    }

    #[test]
    fn only_the_path_part_of_a_url_is_shortened() {
        let h = home("/home/you");
        assert_eq!(short_url_under("local:/home/you/drive", h), "local:~/drive");
        assert_eq!(short_url_under("/home/you/drive", h), "~/drive");
        // An s3 URL has no local path in it to shorten.
        assert_eq!(
            short_url_under("s3://example-archive/stowe", h),
            "s3://example-archive/stowe"
        );
    }
}
