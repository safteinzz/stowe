<!--
AI-ONLY DOCUMENT. This file exists to give an AI agent the COMPLETE operating picture for this repo. Optimize for completeness and precision for the agent, not for human readability. Humans read README.md instead. Do not remove detail to make this nicer, err toward more explicit, not less. FORMAT: machine-read, not a formatted human doc. Do NOT hard-wrap lines to a column width for readability; put each rule/point on ONE line, however long.
-->
# AGENTS.md

Working brief for an AI coding agent, not documentation for people (the README covers that): the rules, invariants and gotchas needed to change this project correctly without rediscovering them.

## Hard rules
- Linux-first; Windows deliberately unsupported until it can actually be tested on Windows.

## Invariants and gotchas
- When checking whether a remote is available: **a folder existing proves nothing** - unmounting leaves the mountpoint dir behind, so a bare directory check can target the wrong disk. Proof is the remote's on-disk `.stowe/` marker plus the local last-push record (`.stowe/remotes/<name>`); a known remote whose marker is gone must error, never be recreated.
- When touching `--mount` handling: the script is the sole authority - stowe runs it and trusts the exit code, no folder-based second-guessing. Scripts must be idempotent (instant no-op when already mounted).
- When changing mirror sync: plan against the mirror's *actual* files, not only its manifest - otherwise it can't repair a drive someone deleted or corrupted files on.
- When changing drift detection: judge drift against the commit being *pushed*, not just the recorded snapshot - otherwise adapt → commit → push dead-ends on the file that was just adopted.
- When touching any tree walk: `.stoweignore` (parsed in `src/ignore.rs`) applies to the working tree *and* to every mirror walk (`mirror_sizes`, `adapt`) - a phone regenerates `.thumbnails/` constantly, so junk that is ignored locally but not remotely reads as drift and demands `--force` on every push. Ancestors are checked, so a file inside an ignored dir is ignored even when the caller didn't prune. An explicitly named path (`stowe add junk.tmp`) is still staged.
- When touching scan/status: `status` never decodes audio; only `add` fingerprints (decoding dominates import cost), cached by size+mtime. The fingerprint is blake3 of the first ~30s of decoded PCM - survives rename/re-tag, not re-encode.
- When optimizing tree walks: keep `read_dir` + `DirEntry::metadata` (dirfd-relative stat; full-path stats are ~5x slower on deep FUSE trees) and keep the walk sequential - a FUSE daemon serializes, so parallel walking is *slower*. Only content hashing is parallel.
- When writing files to a mirror: names legal on ext4 can be unstorable on exFAT/NTFS (control chars etc.) - push probes the target FS empirically and offers a rename fix; never let a raw `os error 22` reach the user.
- tokio stays quarantined in the object-store remote code; the rest of the program is synchronous by design.
- Known, accepted bug: mtime is cached at whole-second resolution, so a same-size in-place edit within one second is missed.

## Build / lint / test
- `cargo build --release`, binary at `target/release/stowe`.
- Unit tests sit in the source files, end-to-end tests in `tests/cli.rs`.

## The demo rig
- Every image in the README is rendered by `demo/`, never captured by hand: `stage.sh` builds a fake home (an invented media archive and a drive to back it up to), `shots.tape` and `shots-short.tape` render the stills, `demo.tape` renders the tour GIF. Run them from inside `demo/` (`./stage.sh mid`, `vhs shots.tape`, ...), one tape at a time, and they write straight into `readme-assets/`. Needs `vhs`, `ttyd` and `ffmpeg` on PATH.
- Two stills tapes because VHS fixes the frame size for a whole tape: `shots.tape` is the tall transcripts, `shots-short.tape` the handful-of-lines ones, and a shot in the wrong tape is a mostly empty picture.
- `stage.sh` redirects `HOME` and every XDG variable into `demo/home` and sets `GIT_CEILING_DIRECTORIES` there, because the stage sits inside this repository's working tree and without a ceiling a git-aware prompt would report *stowe's* branch and dirty count from inside a media archive. Its teardown unmounts everything under the stage before deleting and refuses if anything is still mounted, because a `--mount` remote can put a real drive or phone under that path and `rm -rf` walks straight through a mountpoint.
- Nothing in the fixtures is real: invented artists and shoots, `s3://example-archive/...` (RFC 2606), audio synthesised by ffmpeg so the fingerprint is fingerprinting real audio. Keep it that way, and never point the rig at a personal library.
- The rig is a contributor's tool and never appears in the README, which is read by people who installed a package with `demo/` excluded. Everything it creates lives inside `demo/home` (gitignored), the `stowe` symlink and the staged shell's rc included, so one guarded delete takes all of it; the tapes and `stage.sh` are committed, because they are the source of the assets.
- A path on screen that looks like a dev machine is a bug in the tool, not in the picture: that is why `paths::short` prints `~/drive` and `paths::expand` accepts it back.

## Overview
Layout:
- `src/main.rs` - the clap `Cmd` enum and the dispatch match, nothing else.
- `src/commands/<verb>.rs` - one file per command, each exposing `run`.
- Domain modules at the top level: `repo` (the `.stowe/` on disk), `model` (commits, entries, manifests), `scan` (walking and fingerprinting the tree), `mirror` (playable remotes), `remote` (locating a remote and making it reachable), `names` (portability and the rename fixes), `ignore` (`.stoweignore`), `audio` (fingerprinting), `paths` (how a path is shown to a person, and `~` back to a path), `prompt` (yes/no questions), `time` (commit timestamps), `selfcmd` (`stowe self`).
- There is no `tui/` or `ui/`: stowe has no interactive screen.

`stowe` is a Rust CLI on crates.io: git for the files git chokes on (music, photos, video, datasets). Content-addressed, linear history (one `main`, no branches, no content diffs). A remote is either a **mirror** (real playable folders on a drive/phone, bookkeeping hidden in a `.stowe/` beside them) or a **backup** (deduped blobs, e.g. S3). Local remotes default to mirror, s3 to backup; `--format` overrides, `convert` flips a remote in place. AGPL-3.0-only.

## Self-repair
If anything here contradicts the code, the code wins; fix AGENTS.md in the same session you notice the drift.
