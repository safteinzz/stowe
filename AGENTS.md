# AGENTS.md

## Hard rules
- **Commit only when the user says ship.** Commits go in once the changes are
  tested, which is normally right when they're about to ship; never as a mid-work
  checkpoint.
- Release flow, in this exact order: `cargo clippy` warning-clean + `cargo test`
  green → bump `version` in `Cargo.toml` → one commit (short conventional
  message, never co-authored) → `git push origin main` → `cargo publish`
  (dry-run first; publishing is irreversible) → **tag only after publish
  succeeds**: `git tag vX.Y.Z && git push origin --tags`. A tag must never
  point at a version that failed to publish.
- Fix the root cause. If a workaround must ship, say the word "workaround" out
  loud, so a silent patch never passes as a real fix. Same for lints: never
  `#[allow]` a warning away; delete or fix the code it points at.
- **Never test against real user data.** Use throwaway scratch dirs and the
  release binary, never the real library or an external local copy; don't
  reinstall to test.
- **No em-dashes** anywhere user-facing (README, --help, crate description,
  commit messages, prose) - they read as AI-generated text.
- Every bug fix gets a test - throwaway bash checks let real regressions
  through; that's why tests/cli.rs exists.
- Linux-first; Windows deliberately unsupported until it can actually be
  tested on Windows.

## Invariants and gotchas
- When checking whether a remote is available: **a folder existing proves
  nothing** - unmounting leaves the mountpoint dir behind, so a bare directory
  check can target the wrong disk. Proof is the remote's on-disk `.stowe/`
  marker plus the local last-push record (`.stowe/remotes/<name>`); a known
  remote whose marker is gone must error, never be recreated.
- When touching `--mount` handling: the script is the sole authority - stowe
  runs it and trusts the exit code, no folder-based second-guessing. Scripts
  must be idempotent (instant no-op when already mounted).
- When changing mirror sync: plan against the mirror's *actual* files, not
  only its manifest - otherwise it can't repair a drive someone deleted or
  corrupted files on.
- When changing drift detection: judge drift against the commit being
  *pushed*, not just the recorded snapshot - otherwise adapt → commit → push
  dead-ends on the file that was just adopted.
- When touching scan/status: `status` never decodes audio; only `add`
  fingerprints (decoding dominates import cost), cached by size+mtime. The
  fingerprint is blake3 of the first ~30s of decoded PCM - survives
  rename/re-tag, not re-encode.
- When optimizing tree walks: keep `read_dir` + `DirEntry::metadata`
  (dirfd-relative stat; full-path stats are ~5x slower on deep FUSE trees) and
  keep the walk sequential - a FUSE daemon serializes, so parallel walking is
  *slower*. Only content hashing is parallel.
- When writing files to a mirror: names legal on ext4 can be unstorable on
  exFAT/NTFS (control chars etc.) - push probes the target FS empirically and
  offers a rename fix; never let a raw `os error 22` reach the user.
- tokio stays quarantined in the object-store remote code; the rest of the
  program is synchronous by design.
- Known, accepted bug: mtime is cached at whole-second resolution, so a
  same-size in-place edit within one second is missed.

## Build / test (verified)
    cargo build --release       # binary at target/release/stowe
    cargo test                  # unit tests in src files + end-to-end in tests/cli.rs

## Overview
`stowe` is a Rust CLI on crates.io: git for the files git chokes on (music,
photos, video, datasets). Content-addressed, linear history (one `main`, no
branches, no content diffs). A remote is either a **mirror** (real playable
folders on a drive/phone, bookkeeping hidden in a `.stowe/` beside them) or a
**backup** (deduped blobs, e.g. S3). Local remotes default to mirror, s3 to
backup; `--format` overrides, `convert` flips a remote in place. AGPL-3.0-only.

## Self-repair
If anything here contradicts the code, the code wins - fix this file in the
same session you notice the drift.
