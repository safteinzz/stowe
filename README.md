# stowe

**Git for files, any remote.** A git-shaped CLI for version-controlling large or
binary files (music, datasets, photos, video) — where git itself chokes and
hosts charge for storage.

Same muscle memory, minus the parts that don't fit big files (no branches, no
content diffs):

```sh
stowe init
stowe remote add origin local:/mnt/backup/music
stowe status                      # +3 added, -1 removed, 1 modified, 1 renamed
stowe add -A
stowe commit -m "summer tracks"
stowe log
stowe push                        # upload changed file contents + history
stowe pull                        # sync another machine to the latest commit
```

## How it works

- **Content-addressed dedup.** Every file is named by the blake3 hash of its
  contents, so identical/unchanged files are stored once. Added / removed /
  modified / renamed all fall out of comparing snapshots by hash.
- **Fast rescans.** A file is only re-hashed when its size or mtime changed.
- **The remote is a dumb file store.** It only needs to hold files — all the
  brains run client-side. A populated remote is a *bare* stowe repo:

  ```
  <remote>/
    refs/main          # the commit the remote is at — answers "where is the server?"
    commits/<hash>.json
    objects/<ab>/<rest>
  ```

  So any machine can read `refs/main` and reconstruct the library. Today the
  only backend is `local:<path>` (a folder / mounted drive / NAS); `rclone:`
  (Google Drive, FTP, S3, Dropbox, …) and native `ftp://` are next, behind the
  same four-method interface.

## On-disk layout (local repo)

```
.stowe/
  config             # remotes
  HEAD               # current commit hash
  index              # staged snapshot
  commits/<hash>.json
```

## Status

Early. Linear history only ("main"), last-write-wins on push. Whole-file
objects (no chunking yet). `stowe gc` (prune unreferenced history) and more
backends are planned.

## License

AGPL-3.0-only.
