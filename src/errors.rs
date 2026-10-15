//! An error as a person reads it: our own words, then what the system said in
//! words too, never an errno.

use std::io;

/// `e` and every cause under it, joined with `: `. An OS error becomes words
/// with what to do next, and a wrapper that repeats its cause in its own text
/// (an object store's error, a directory walk's) is replaced by what it names,
/// so nothing is said twice.
pub fn words(e: &anyhow::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in e.chain() {
        let part = if let Some(io) = cause.downcast_ref::<io::Error>() {
            io_words(io)
        } else if let Some(walk) = cause.downcast_ref::<walkdir::Error>() {
            match walk.path() {
                Some(p) => format!("could not read `{}`", p.display()),
                None => "could not walk the folder".to_string(),
            }
        } else if let Some(store) = cause.downcast_ref::<opendal::Error>() {
            if cause.source().is_some() {
                continue;
            }
            store_kind(store.kind()).to_string()
        } else {
            cause.to_string()
        };
        parts.push(part);
    }
    parts.join(": ")
}

/// An `io::Error` that never became an `anyhow::Error`, such as a command
/// that would not start.
pub fn io_words(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(_) => os(e.kind()),
        None => e.to_string(),
    }
}

fn os(kind: io::ErrorKind) -> String {
    use io::ErrorKind::*;
    match kind {
        NotFound => "it does not exist",
        PermissionDenied => "permission denied, so check who owns it and its mode",
        AlreadyExists => "something by that name is already there",
        StorageFull => "the disk is full, so free some space and run it again",
        QuotaExceeded => "the disk quota is used up, so free some space and run it again",
        ReadOnlyFilesystem => "the disk is mounted read-only",
        IsADirectory => "it is a folder",
        NotADirectory => "a part of its path is not a folder",
        DirectoryNotEmpty => "the folder is not empty",
        InvalidFilename => "the disk cannot store that name",
        InvalidInput => "the disk refused the name or the request",
        CrossesDevices => "it would have to move across disks",
        FileTooLarge => "the file is too large for that disk",
        ResourceBusy => "it is in use, so try again once it is free",
        StaleNetworkFileHandle => "the network share went away, so mount it again",
        TimedOut => "it timed out",
        Interrupted => "it was interrupted",
        UnexpectedEof => "it ended early, so it may be cut short",
        BrokenPipe => "the other end closed",
        other => return other.to_string(),
    }
    .to_string()
}

fn store_kind(kind: opendal::ErrorKind) -> &'static str {
    use opendal::ErrorKind::*;
    match kind {
        NotFound => "the remote has no such file",
        PermissionDenied => "the remote refused access, so check its credentials",
        AlreadyExists => "the remote already has that file",
        RateLimited => "the remote is limiting requests, so try again later",
        ConfigInvalid => "the remote's settings are not valid",
        _ => "the remote failed",
    }
}
