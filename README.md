# stowe

**Git for files, any remote.** Version-control large/binary files (music,
photos, video, datasets) where git chokes — content-addressed, deduped, with a
dumb pluggable remote. No branches, no content diffs.

```sh
cargo install stowe

stowe init
stowe add -A                          # or: stowe add <paths...>
stowe commit -m "import"
stowe remote add origin local:/mnt/backup/music
stowe push                            # upload changed content + history
stowe pull                            # sync another machine to the latest
```

## Features

- **Dedup** — every file is named by its blake3 hash, so identical/unchanged
  content is stored once.
- **Fast rescans** — a file is re-read only when its size or mtime changes.
- **Move tracking** — renames are matched by content hash; audio also carries a
  fingerprint (hash of the *decoded* PCM, via
  [Symphonia](https://github.com/pdeljanov/Symphonia)), so a song that's renamed
  *and* re-tagged still reads as a move, not a delete + add.
- **git-style status** — staged / unstaged / untracked, with
  `deleted/modified/renamed/new`.

## Remotes

The backend is picked from the URL:

- `local:<path>` — folder, mounted drive, or NAS. Anything you can mount is a
  remote, so Google Drive / Dropbox / FTP work today by mounting them
  (`rclone mount gdrive: ~/gdrive`) and pointing `local:` at the mount.
- `s3://<bucket>[/<root>]` — any S3-compatible store (AWS, Backblaze B2, MinIO).
  Credentials from the `AWS_*` env vars; set `AWS_ENDPOINT_URL` for B2/MinIO.

A populated remote is a *bare* repo — `refs/main`, `commits/<hash>.json`,
`objects/<ab>/<rest>` — so any machine can read it and rebuild the library.
Native `gdrive:` / `rclone:` / `ftp://` are planned, behind the same interface.

## Status

Early: linear history (one `main`), last-write-wins push, whole-file objects (no
chunking). `stowe restore`/`checkout` (recover a committed file) and `stowe gc`
(prune unreferenced history) are planned.

## License

AGPL-3.0-only.
