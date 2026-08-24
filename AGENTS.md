<!--
AI-ONLY DOCUMENT. This file exists to give an AI agent the COMPLETE operating picture for this repo. Optimize for completeness and precision for the agent, not for human readability. Humans read README.md instead. Do not remove detail to make this nicer, err toward more explicit, not less. FORMAT: machine-read, not a formatted human doc. Do NOT hard-wrap lines to a column width for readability; put each rule/point on ONE line, however long.
-->
# AGENTS.md

Working brief for an AI coding agent, not documentation for people (the README covers that): the rules, invariants and gotchas needed to change this project correctly without rediscovering them.

## Hard rules
- Commit, push, and publish only when the user says to ship; a mid-work commit is never the deliverable, because the user tests interactively first.
- Commit messages are short single-line conventional ones (`feat:`, `fix:`, `chore:`, ...), never with a `Co-Authored-By` trailer and never with a verbose body.
- Release flow, in this exact order: ask whether this shipment gets tests and write them only if the user says yes -> bump `version` in `Cargo.toml` -> `cargo fmt --check` clean, `cargo clippy-all` clean and `cargo test` green, which is also what refreshes `Cargo.lock` with the new version -> `cargo +1.88 msrv` clean, the only thing that proves the `rust-version` floor in `Cargo.toml` is real -> one commit -> `git push origin main` -> `cargo publish` (dry-run first, publishing is irreversible) -> tag only after publish succeeds with `git tag vX.Y.Z && git push origin --tags`; a tag must never point at a version that failed to publish, and the bump comes first because `cargo publish` fails on a `Cargo.lock` that still holds the old version.
- Tests are proposed at ship time and never before: the first step of the release flow is to ask the user, in plain words, whether this shipment gets tests, and they are written only on a yes, so the decision is always theirs but the question is never forgotten.
- Never write a test for behaviour that has not shipped yet, because code that is not in the last release tag is still being designed, and a test pinning a shape that is about to change is how a suite starts lying.
- A test may only assert something the README or `--help` promises, or a pure-logic invariant (parsing, generation, path resolution, validation); never the shape of a private function and never the specific diff that was just made, since those rot on the next refactor and teach nothing about whether the program works.
- Removing a promise from the README removes its tests in the same commit.
- A test may only write inside a temp directory it deletes, never a real config, data, cache or content directory and never a fixed path, so a machine is left exactly as it was before the suite ran.
- Never drive the interface to test it: build it, say what changed and what to look at, and let the user run it, because they see the screen instantly while an agent driving a pty or a tmux pane is slow and wrong about what it looks like; logic that is not visual can still be checked directly from `tests/`.
- Never `cargo install` to test: run the release binary at `./target/release/stowe` directly, because installing replaces the binary on PATH with a work-in-progress build; install only when the user asks.
- `main` is protected: no force-push and no history rewrite, so a mistake is fixed with a forward commit.
- No em-dashes anywhere (code, comments, README, `--help`, crate description, commit messages, prose), because they read as AI-generated text; use `-` instead.
- Fix the root cause, and if a workaround must ship say the word "workaround" out loud so a silent patch never passes as a real fix; the same goes for lints, where an `#[allow]` is never the answer and the code it points at gets fixed or deleted.
- `TODO-LIST.md` (gitignored) holds one-line ideas, and the line is deleted when the idea ships.
- `.nocommit/` (gitignored) holds reference material used only to inform work here - other projects, notes, drafts - and never ships; keep it out of anything user-facing (commit messages, code, comments, README, `--help`), since a reference to material nobody outside this machine can see means nothing to them and just clutters the record.
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
- `cargo clippy-all` is the lint pass, aliased in `.cargo/config.toml` to `clippy --release --all-targets -- -D warnings`; use it rather than a bare `cargo clippy`, which skips `tests/` and `examples/` and only warns where the release flow wants a failure.
- `cargo test`.
- `cargo fmt` formats the crate and `cargo fmt --check` fails when anything has drifted; the whole crate is rustfmt clean, so formatting is never a judgement call and never a review comment.
- `cargo +1.88 msrv` checks the crate against the `rust-version` floor it advertises (alias in `.cargo/config.toml`), and when the code starts needing a newer compiler both that floor and the toolchain in this line move together.
- Unit tests sit in the source files, end-to-end tests in `tests/cli.rs`.

## The demo rig
- Every image in the README is rendered by `demo/`, never captured by hand: `stage.sh` builds a fake home (an invented media archive and a drive to back it up to), `shots.tape` and `shots-short.tape` render the stills, `demo.tape` renders the tour GIF. Run them from inside `demo/` (`./stage.sh mid`, `vhs shots.tape`, ...), one tape at a time, and they write straight into `readme-assets/`. Needs `vhs`, `ttyd` and `ffmpeg` on PATH.
- Two stills tapes because VHS fixes the frame size for a whole tape: `shots.tape` is the tall transcripts, `shots-short.tape` the handful-of-lines ones, and a shot in the wrong tape is a mostly empty picture.
- `stage.sh` redirects `HOME` and every XDG variable into `demo/home` and sets `GIT_CEILING_DIRECTORIES` there, because the stage sits inside this repository's working tree and without a ceiling a git-aware prompt would report *stowe's* branch and dirty count from inside a media archive. Its teardown unmounts everything under the stage before deleting and refuses if anything is still mounted, because a `--mount` remote can put a real drive or phone under that path and `rm -rf` walks straight through a mountpoint.
- Nothing in the fixtures is real: invented artists and shoots, `s3://example-archive/...` (RFC 2606), audio synthesised by ffmpeg so the fingerprint is fingerprinting real audio. Keep it that way, and never point the rig at a personal library.
- The rig is a contributor's tool and never appears in the README, which is read by people who installed a package with `demo/` excluded. Everything it creates lives inside `demo/home` (gitignored), the `stowe` symlink and the staged shell's rc included, so one guarded delete takes all of it; the tapes and `stage.sh` are committed, because they are the source of the assets.
- The demo rig is standardised across every crate in this directory, and the three parts that make the frames match must stay identical: the staged shell wears the invented `user@host` prompt written by `write_demorc`, every tape sets `Set Theme "Catppuccin Mocha"` (VHS's own default is a near-black that is harsher to read than Catppuccin's `#1e1e2e`), and every tape sets `Set FontFamily "JetBrainsMono NF"`. It is not about hiding a username, which is no leak - the pictures are a build output, and one that comes out different on every machine that regenerates it is not reproducible.
- Everything staged runs under `env -i` with a complete allowlist, never the real environment plus overrides, because an exported variable nobody thought of is exactly how a real config dir, a real endpoint or a real key ends up in a frame. The rig also writes only inside the stage, so nothing of the renderer's is touched on disk.
- Teardown goes through `assert_safe_to_delete`, word-for-word the same function in every crate's rig: the stage path must be absolute, must not be a system directory or the renderer's home, must resolve through its symlinks to a tree carrying the marker file the build stamped, and must have nothing mounted under it, with `rm -rf --one-file-system` behind that. Test the refusals after touching them, never just the happy path - an sshfs mount inside a staged home, torn down with a plain `rm -rf`, has already deleted the dotfiles on the machine at the far end.
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
