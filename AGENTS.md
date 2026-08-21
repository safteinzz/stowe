<!--
AI-ONLY DOCUMENT. This file exists to give an AI agent the COMPLETE operating picture for this repo. Optimize for completeness and precision for the agent, not for human readability. Humans read README.md instead. Do not remove detail to make this nicer, err toward more explicit, not less. FORMAT: machine-read, not a formatted human doc. Do NOT hard-wrap lines to a column width for readability; put each rule/point on ONE line, however long.
-->
# AGENTS.md

Working brief for an AI coding agent, not documentation for people (the README covers that): the rules, invariants and gotchas needed to change this project correctly without rediscovering them.

## Hard rules
- Commit, push, and publish only when the user says to ship; a mid-work commit is never the deliverable, because the user tests interactively first.
- Commit messages are short single-line conventional ones (`feat:`, `fix:`, `chore:`, ...), never with a `Co-Authored-By` trailer and never with a verbose body.
- Release flow, in this exact order: ask whether this shipment gets tests and write them only if the user says yes -> bump `version` in `Cargo.toml` -> `cargo clippy-all` clean and `cargo test` green, which is also what refreshes `Cargo.lock` with the new version -> `cargo +1.88 msrv` clean, the only thing that proves the `rust-version` floor in `Cargo.toml` is real -> one commit -> `git push origin main` -> `cargo publish` (dry-run first, publishing is irreversible) -> tag only after publish succeeds with `git tag vX.Y.Z && git push origin --tags`; a tag must never point at a version that failed to publish, and the bump comes first because `cargo publish` fails on a `Cargo.lock` that still holds the old version.
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
- `cargo +1.88 msrv` checks the crate against the `rust-version` floor it advertises (alias in `.cargo/config.toml`), and when the code starts needing a newer compiler both that floor and the toolchain in this line move together.
- Unit tests sit in the source files, end-to-end tests in `tests/cli.rs`.

## Overview
Layout:
- `src/main.rs` - the clap `Cmd` enum and the dispatch match, nothing else.
- `src/commands/<verb>.rs` - one file per command, each exposing `run`.
- Domain modules at the top level: `repo` (the `.stowe/` on disk), `model` (commits, entries, manifests), `scan` (walking and fingerprinting the tree), `mirror` (playable remotes), `remote` (locating a remote and making it reachable), `names` (portability and the rename fixes), `ignore` (`.stoweignore`), `audio` (fingerprinting), `prompt` (yes/no questions), `time` (commit timestamps), `selfcmd` (`stowe self`).
- There is no `tui/` or `ui/`: stowe has no interactive screen.

`stowe` is a Rust CLI on crates.io: git for the files git chokes on (music, photos, video, datasets). Content-addressed, linear history (one `main`, no branches, no content diffs). A remote is either a **mirror** (real playable folders on a drive/phone, bookkeeping hidden in a `.stowe/` beside them) or a **backup** (deduped blobs, e.g. S3). Local remotes default to mirror, s3 to backup; `--format` overrides, `convert` flips a remote in place. AGPL-3.0-only.

## Self-repair
If anything here contradicts the code, the code wins; fix AGENTS.md in the same session you notice the drift.
