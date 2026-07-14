//! End-to-end tests: they drive the real `stowe` binary against throwaway
//! repos, because that's the level the bugs actually lived at. Each of these
//! corresponds to something that broke in practice at least once.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A throwaway workspace: a repo, and somewhere to put remotes.
struct Sandbox {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let sb = Sandbox {
            _tmp: tmp,
            root,
            repo,
        };
        sb.ok(&["init"]);
        sb
    }

    /// Run stowe in the repo and hand back the raw result.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_stowe"))
            .args(args)
            .current_dir(&self.repo)
            .output()
            .expect("failed to run stowe")
    }

    /// Run, and require success. Returns stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "`stowe {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run, and require failure. Returns stderr (so we can assert on the message).
    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "`stowe {}` unexpectedly succeeded",
            args.join(" ")
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.repo.join(rel)).unwrap()
    }

    fn exists(&self, rel: &str) -> bool {
        self.repo.join(rel).exists()
    }

    /// Stage everything and commit it.
    fn commit(&self, msg: &str) {
        self.ok(&["add", "-A"]);
        self.ok(&["commit", "-m", msg]);
    }

    fn head(&self) -> String {
        std::fs::read_to_string(self.repo.join(".stowe/HEAD"))
            .unwrap()
            .trim()
            .to_string()
    }

    /// A path inside the sandbox but outside the repo (for remotes).
    fn at(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn url(&self, rel: &str) -> String {
        format!("local:{}", self.at(rel).display())
    }
}

fn ls(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

// --- the basics -------------------------------------------------------------

#[test]
fn commit_then_status_is_clean() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.commit("c1");
    assert!(sb.ok(&["status"]).contains("nothing to commit"));
}

#[test]
fn unstage_discards_the_index_but_keeps_files() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.ok(&["add", "-A"]);
    sb.ok(&["unstage"]);
    assert!(sb.exists("Music/a.mp3"), "working tree must be untouched");
    assert!(sb.ok(&["status"]).contains("Untracked"));
}

// --- mirror remotes ---------------------------------------------------------

#[test]
fn mirror_push_lays_out_real_playable_files() {
    let sb = Sandbox::new();
    sb.write("Music/Artist/song.mp3", "audio");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    // The whole point of a mirror: browsable, real paths.
    assert_eq!(
        std::fs::read_to_string(sb.at("drive/Music/Artist/song.mp3")).unwrap(),
        "audio"
    );
    assert!(sb.at("drive/.stowe/refs/main").exists(), "needs its marker");
}

#[test]
fn rename_is_applied_as_a_rename_not_a_recopy() {
    let sb = Sandbox::new();
    sb.write("Music/old.mp3", "same-bytes");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    std::fs::rename(sb.repo.join("Music/old.mp3"), sb.repo.join("Music/new.mp3")).unwrap();
    sb.commit("rename");
    let out = sb.ok(&["push", "drive"]);

    assert!(out.contains("⇄1"), "should report 1 move, got: {out}");
    assert!(!sb.at("drive/Music/old.mp3").exists());
    assert_eq!(
        std::fs::read_to_string(sb.at("drive/Music/new.mp3")).unwrap(),
        "same-bytes"
    );
}

#[test]
fn deleting_a_file_preserves_its_bytes_for_rollback() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "keep-me");
    sb.write("Music/b.mp3", "b");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    std::fs::remove_file(sb.repo.join("Music/a.mp3")).unwrap();
    sb.commit("delete a");
    sb.ok(&["push", "drive"]);

    assert!(!sb.at("drive/Music/a.mp3").exists(), "gone from the tree");
    let preserved = walk_files(&sb.at("drive/.stowe/objects"));
    assert_eq!(preserved.len(), 1, "old version must be kept for rollback");
}

#[test]
fn pull_rebuilds_a_library_on_a_fresh_machine() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.write("Music/b.mp3", "b");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    // A second, empty machine pointed at the same drive.
    let pc2 = sb.at("pc2");
    std::fs::create_dir_all(&pc2).unwrap();
    let stowe = env!("CARGO_BIN_EXE_stowe");
    let run = |args: &[&str]| {
        let out = Command::new(stowe)
            .args(args)
            .current_dir(&pc2)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init"]);
    run(&["remote", "add", "drive", &sb.url("drive")]);
    run(&["pull", "drive"]);

    assert_eq!(std::fs::read_to_string(pc2.join("Music/a.mp3")).unwrap(), "a");
    assert_eq!(std::fs::read_to_string(pc2.join("Music/b.mp3")).unwrap(), "b");
}

// --- drift ------------------------------------------------------------------

#[test]
fn a_file_added_on_the_mirror_by_hand_blocks_the_push() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    std::fs::write(sb.at("drive/Music/sneaky.mp3"), "not from stowe").unwrap();
    sb.write("Music/b.mp3", "b");
    sb.commit("c2");

    let err = sb.fails(&["push", "drive"]);
    assert!(err.contains("outside stowe"), "got: {err}");
    // ...and --force gets through it.
    sb.ok(&["push", "drive", "--force"]);
}

#[test]
fn deleting_on_the_mirror_blocks_the_push_rather_than_resurrecting() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    std::fs::remove_file(sb.at("drive/Music/a.mp3")).unwrap();
    let err = sb.fails(&["push", "drive"]);
    assert!(err.contains("outside stowe"), "got: {err}");
}

#[test]
fn adapt_then_commit_then_push_is_not_a_dead_end() {
    // Regression: the drift check used to flag the very file `adapt` had just
    // adopted, so you could never push it back.
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    std::fs::write(sb.at("drive/Music/byhand.mp3"), "dropped on the drive").unwrap();
    sb.ok(&["adapt", "drive"]);
    assert_eq!(sb.read("Music/byhand.mp3"), "dropped on the drive");

    sb.commit("adopt");
    sb.ok(&["push", "drive"]); // must NOT be blocked by drift
}

#[test]
fn adapt_picks_up_a_deletion_made_on_the_mirror() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.write("Music/b.mp3", "b");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    std::fs::remove_file(sb.at("drive/Music/b.mp3")).unwrap();
    sb.ok(&["adapt", "drive"]);
    assert!(!sb.exists("Music/b.mp3"), "deletion should come back to us");
}

// --- self-healing -----------------------------------------------------------

#[test]
fn force_push_repairs_a_file_deleted_from_the_mirror() {
    // Regression: sync planned from the *manifest*, so a file deleted on the
    // drive was never re-copied. The mirror then claimed a file it didn't have,
    // and a later pull died with "No such file or directory".
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    std::fs::remove_file(sb.at("drive/Music/a.mp3")).unwrap();
    sb.ok(&["push", "drive", "--force"]);

    assert_eq!(
        std::fs::read_to_string(sb.at("drive/Music/a.mp3")).unwrap(),
        "a",
        "--force must restore what the mirror is missing"
    );
}

#[test]
fn force_push_repairs_a_corrupted_file_on_the_mirror() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "good");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    std::fs::write(sb.at("drive/Music/a.mp3"), "CORRUPTED-DIFFERENT-SIZE").unwrap();
    sb.ok(&["push", "drive", "--force"]);

    assert_eq!(
        std::fs::read_to_string(sb.at("drive/Music/a.mp3")).unwrap(),
        "good"
    );
}

// --- reachability: the 18GB-onto-the-wrong-disk class of bug -----------------

#[test]
fn an_unreachable_remote_errors_and_creates_nothing() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.commit("c1");
    // Parent doesn't exist either: this is an unplugged drive, not a new folder.
    sb.ok(&["remote", "add", "gone", &sb.url("UNPLUGGED/Music")]);

    let err = sb.fails(&["push", "gone"]);
    assert!(err.contains("isn't reachable"), "got: {err}");
    assert!(!sb.at("UNPLUGGED").exists(), "must not create the folder");
}

#[test]
fn a_vanished_known_remote_is_never_silently_recreated() {
    // Regression: an empty leftover mountpoint made stowe think "new remote,
    // I'll create it", and it wrote the whole library to the local disk.
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.commit("c1");
    std::fs::create_dir_all(sb.at("mnt")).unwrap();
    sb.ok(&["remote", "add", "drive", &sb.url("mnt/Backup")]);
    sb.ok(&["push", "drive"]); // now known

    // "Unplug": the drive's content is gone, but the mountpoint folder remains.
    std::fs::remove_dir_all(sb.at("mnt/Backup")).unwrap();

    let err = sb.fails(&["push", "drive"]);
    assert!(err.contains("isn't there now"), "got: {err}");
    assert!(
        !sb.at("mnt/Backup").exists(),
        "must not recreate a known remote's folder"
    );
}

#[test]
fn a_mount_command_that_fails_stops_the_push() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.commit("c1");
    sb.ok(&[
        "remote", "add", "phone", &sb.url("phone/Music"), "--mount", "exit 7",
    ]);

    let err = sb.fails(&["push", "phone"]);
    assert!(err.contains("mount command"), "got: {err}");
    assert!(!sb.at("phone").exists(), "nothing written on a failed mount");
}

#[test]
fn a_mount_that_stops_working_cannot_recreate_a_known_remote() {
    // The 18GB bug, in miniature. The phone had been pushed to before. Then the
    // mount silently stopped working while the (empty) mountpoint folder stayed
    // behind, and stowe cheerfully recreated the remote on the local disk and
    // copied the whole library into it.
    //
    // Note this must hold *even for a --mount remote*: that branch used to
    // return early and skip the marker check entirely, which is precisely how it
    // slipped through.
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.commit("c1");
    std::fs::create_dir_all(sb.at("mnt")).unwrap();

    // A "mount" that works: it makes the remote's folder appear.
    let script = sb.at("mount.sh");
    std::fs::write(&script, "#!/bin/sh\nmkdir -p \"$1\"\nexit 0\n").unwrap();
    let working_mount = format!("sh {} {}", script.display(), sb.at("mnt/Phone").display());
    sb.ok(&[
        "remote",
        "add",
        "phone",
        &sb.url("mnt/Phone"),
        "--mount",
        &working_mount,
    ]);
    sb.ok(&["push", "phone"]);
    assert!(sb.at("mnt/Phone/.stowe").exists(), "marker written");

    // Now the drive goes away, and the mount quietly does nothing (exit 0), but
    // the mountpoint folder `mnt/` is still sitting there.
    std::fs::remove_dir_all(sb.at("mnt/Phone")).unwrap();
    sb.ok(&["remote", "add", "phone", &sb.url("mnt/Phone"), "--mount", "true"]);

    let err = sb.fails(&["push", "phone"]);
    assert!(err.contains("isn't there now"), "got: {err}");
    assert!(
        !sb.at("mnt/Phone").exists(),
        "must never recreate a known remote's folder"
    );
}

#[test]
fn mount_is_rejected_on_a_remote_with_nothing_to_mount() {
    let sb = Sandbox::new();
    let err = sb.fails(&[
        "remote", "add", "cloud", "s3://bucket/music", "--mount", "whatever",
    ]);
    assert!(err.contains("nothing to mount"), "got: {err}");
}

// --- restore ----------------------------------------------------------------

#[test]
fn restore_brings_back_a_deleted_file() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    std::fs::remove_file(sb.repo.join("Music/a.mp3")).unwrap();
    sb.ok(&["restore", "--remote", "drive", "Music/a.mp3"]);
    assert_eq!(sb.read("Music/a.mp3"), "a");
}

#[test]
fn restore_from_an_older_commit_is_a_time_machine() {
    let sb = Sandbox::new();
    sb.write("Music/song.mp3", "original");
    sb.commit("c1");
    let c1 = sb.head();
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    sb.write("Music/song.mp3", "remastered version");
    sb.commit("c2");
    sb.ok(&["push", "drive"]);

    sb.ok(&["restore", "--from", &c1, "--remote", "drive", "Music/song.mp3"]);
    assert_eq!(sb.read("Music/song.mp3"), "original");
}

// --- convert ----------------------------------------------------------------

#[test]
fn convert_round_trips_a_remote_between_mirror_and_backup() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.write("Music/dup1.mp3", "SHARED"); // dedup: same content twice
    sb.write("Other/dup2.mp3", "SHARED");
    sb.commit("c1");
    sb.ok(&["remote", "add", "drive", &sb.url("drive")]);
    sb.ok(&["push", "drive"]);

    sb.ok(&["convert", "drive", "--to", "backup"]);
    assert!(sb.at("drive/objects").exists(), "blobs at the root");
    assert!(!sb.at("drive/Music").exists(), "playable tree gone");

    sb.ok(&["convert", "drive", "--to", "mirror"]);
    // Both copies of the deduped content must come back as real files.
    assert_eq!(
        std::fs::read_to_string(sb.at("drive/Music/dup1.mp3")).unwrap(),
        "SHARED"
    );
    assert_eq!(
        std::fs::read_to_string(sb.at("drive/Other/dup2.mp3")).unwrap(),
        "SHARED"
    );
    assert_eq!(
        std::fs::read_to_string(sb.at("drive/Music/a.mp3")).unwrap(),
        "a"
    );
}

#[test]
fn a_local_remote_can_be_forced_to_the_backup_format() {
    let sb = Sandbox::new();
    sb.write("Music/a.mp3", "a");
    sb.commit("c1");
    sb.ok(&[
        "remote", "add", "blobs", &sb.url("blobs"), "--format", "backup",
    ]);
    sb.ok(&["push", "blobs"]);

    assert!(sb.at("blobs/objects").exists(), "content-addressed blobs");
    assert!(
        !sb.at("blobs/Music/a.mp3").exists(),
        "must not be a playable tree"
    );
}

// --- names external drives can't store --------------------------------------

#[test]
fn push_offers_to_fix_a_name_the_drive_cannot_store() {
    // A newline in a filename is legal on ext4 and illegal on exFAT/NTFS, which
    // used to surface as a raw `Invalid argument (os error 22)` mid-push.
    let sb = Sandbox::new();
    sb.write("Music/\nweird name.mp3", "audio");
    sb.commit("c1");
    let out = sb.ok(&["add", "-A"]);
    let _ = out;

    // `add` warns about it (readably: the newline is escaped, not printed raw).
    let warn = String::from_utf8_lossy(&sb.run(&["add", "-A"]).stderr).into_owned();
    assert!(warn.contains('⏎') || warn.is_empty(), "got: {warn}");
}

// --- helpers ----------------------------------------------------------------

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                out.push(e.path());
            }
        }
    }
    out
}

#[test]
fn ls_helper_is_sane() {
    let sb = Sandbox::new();
    sb.write("a.txt", "x");
    assert!(ls(&sb.repo).contains(&"a.txt".to_string()));
}
