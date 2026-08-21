# stowe

> **Canonical:** [gitlab.com/safteinzz/stowe](https://gitlab.com/safteinzz/stowe) · **Mirror:** [github.com/safteinzz/stowe](https://github.com/safteinzz/stowe)

<!-- desc:start -->
where git chokes, stowe stows - versioned, deduped big and binary files, pushed to backups you can still play (mirror) or compact blob stores (S3)
<!-- desc:end -->

```sh
cargo install stowe

stowe init
stowe add -A
stowe commit -m "import"
stowe remote add origin local:/mnt/drive
stowe push
```

## See what changed

Move a folder, rename a file, drop new ones in. stowe works out what actually
happened instead of reporting a wall of deletes and adds.

![stowe status showing six files detected as renames after two folders were moved, two untracked files, and a summary line reading +2 -0 ~0 and 6 moved](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/status.png)

The summary counts new, deleted, changed, and moved. Six files changed folder
here and not one of them counts as new.

## Commit and back it up

![stowe add -A, stowe commit and stowe push drive run in sequence, ending in a report reading plus 2 new, 0 changed, 6 moved, 0 removed](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/push.png)

`+2 new, ~0 changed, ⇄6 moved` is the point: the six relocated files were
renamed in place on the drive. Only the two genuinely new files crossed the
wire. Reorganising a terabyte archive stays cheap.

## The backup is just your files

![ls -la of the mirror showing Datasets, Photos, Renders and Video directories alongside a hidden .stowe directory](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/mirror.png)

A mirror is your real tree at real paths. Plug the drive into anything, open the
folders, play or edit what is inside. The history lives in `.stowe/` beside it,
so one drive is both a working copy and a time machine.

## Two shapes of remote

![stowe remote listing a drive remote marked mirror and an offsite s3 remote marked backup](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/remote.png)

- **mirror** (`local:`): real, browsable folders on a drive or phone.
- **backup** (`s3://`, or `--format backup`): deduped content-addressed blobs.
  Compact, not browsable.

Push to as many as you like. Each tracks its own progress, and `stowe convert`
flips a remote between the two **in place**, no re-upload.

## It will not overwrite what it did not put there

![stowe push halting with a report that the mirror was changed outside stowe, listing one file added on the mirror, and an error telling you to reconcile or use --force](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/drift.png)

If a remote changed behind stowe's back, the push stops and tells you what it
found. Nothing is written until you decide: `stowe adapt` pulls those changes
back into the repo, `--force` overwrites them.

## Commands

```
init     create a repo (.stowe/) in the current folder
status   what changed since the last commit
add      stage files            <paths...> | -A for everything
unstage  discard the staging index (working tree untouched)
commit   record the staged snapshot        -m MSG
log      commit history, newest first
remote   manage remotes - no subcommand lists them
           add NAME URL  --format mirror|backup  --mount CMD
push     sync remote(s) to the latest commit   [remotes...]  --force
pull     rebuild the working tree from a remote          [remote]
adapt    pull a remote's by-hand changes into local      [remote]
restore  recover committed files  <paths...> -A --from COMMIT --remote R
convert  flip a remote between mirror and backup, in place  --to FORMAT
self     update or check stowe itself
           update [-y]   reinstall the latest release
           check         is a newer release out?
```

## Ignoring junk

Media folders fill up with things nobody wants versioned. Put a `.stoweignore`
at the repo root:

```
# comments and blank lines are skipped
.DS_Store           # a bare name matches that file or folder anywhere
*.tmp               # `*` matches any run, `?` exactly one, within a segment
.thumbnails/        # a trailing slash matches directories only
Renders/proxies/    # a pattern with a slash is anchored at the repo root
```

The rules apply to every scan, the working tree **and** your remotes. That
second part matters: a phone's gallery recreates `.thumbnails/` every time it
indexes the folder, and without this it would read as drift and demand `--force`
on every single push.

## Good to know

- Renames and re-tagged audio are tracked as **moves**, not re-uploads. Audio is
  fingerprinted from the decoded signal, so a move survives a tag edit.
- Naming a file outright (`stowe add junk.tmp`) stages it even if it is ignored.
  An exact path you typed wins.
- Linear history: one `main`, no branches, no content diffs.
- `--mount CMD` runs your own script when a remote is not reachable, so pushing
  to an external drive or a phone over sshfs mounts it on demand.
- Replaced and deleted versions are kept in the remote's `.stowe/objects/`, so
  `stowe restore --from <commit>` can reach back for them.
- Names that are legal locally but unstorable on exFAT or NTFS are detected
  before the push, with a rename offered.
- Linux-first. Windows is deliberately unsupported until it can be tested there.

## License

AGPL-3.0-only
