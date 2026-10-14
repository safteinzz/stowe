# stowe

> **Canonical:** [gitlab.com/safteinzz/stowe](https://gitlab.com/safteinzz/stowe) · **Mirror:** [github.com/safteinzz/stowe](https://github.com/safteinzz/stowe)

<!-- desc:start -->
where git chokes, stowe stows - versioned, deduped big and binary files, pushed to backups you can still play (mirror) or compact blob stores (S3)
<!-- desc:end -->

## Install

```bash
cargo install stowe
stowe self check   # is a newer release out?
stowe self update  # install the latest
```

No cargo yet? Rust installs the same way on every distro: [rustup.rs](https://rustup.rs).

## Back up a folder

![stowe remote add pointing a remote at a drive, then stowe push copying the ten files of a freshly committed archive onto it](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/import.png)

```bash
stowe init                            # make this folder a repo
stowe add -A                          # stage everything in it
stowe commit -m "import the archive"  # record that snapshot
stowe remote add drive local:~/drive  # somewhere to keep it
stowe push drive                      # copy it there
```

## See what changed

![stowe istatus staging one new file, then the rest, and committing them through a form](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/status.gif)

```bash
stowe istatus    # stage, unstage and commit in one screen
stowe status     # what changed since the last commit
stowe status -s  # the same in one line, for a shell prompt
```

![stowe status after two folders were moved and a song retitled: six renames, each with the changed characters highlighted, two untracked files, and a summary line reading +2 -0 ~0 and 6 moved](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/status.png)

Move a folder, rename a file, drop new ones in. stowe works out what actually
happened instead of reporting a wall of deletes and adds, and marks only the
characters that changed.

## Commit and back it up

![stowe commit followed by stowe push drive, ending in a report reading plus 2 new, 0 changed, 6 moved, 0 removed](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/push.png)

```bash
stowe add -A                   # stage the moves and the new files
stowe commit -m "reorganise"   # record them
stowe push drive               # rename on the drive, copy only what is new
```

`+2 new, ~0 changed, ⇄6 moved` is the point: the six relocated files were
renamed in place on the drive. Only the two genuinely new files crossed the
wire. Reorganising a terabyte archive stays cheap.

## The backup is just your files

![ls of the mirror showing the Datasets, Music, Photos, Renders and Video folders beside a hidden .stowe folder, then the two songs in Music under their own names](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/mirror.png)

A mirror is your real tree at real paths. Plug the drive into anything, open the
folders, play or edit what is inside. The history lives in `.stowe/` beside it,
so one drive is both a working copy and a time machine.

## Read the history, file by file

![stowe ilog opening the reorganisation, its moves marked where the names changed, then one photo's versions across the move](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/history.gif)

Legend: `+` new · `~` changed · `⇄` moved · `-` removed · `↑` on no remote yet

```bash
stowe ilog     # commits, their changes, each file's versions; ↵ restores one
stowe log      # history, newest first
```

![stowe log listing four commits](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/history-plain.png)

A file is followed across every move, so its versions read as one list
whatever it was called when each was committed.

## Find a file by any name it had

![stowe iscan finding a song under its old and new name, then opening the commit that renamed it](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/scan.gif)

```bash
stowe iscan            # type part of a name; ↵ opens the commit that touched it
stowe search <PATTERN> # every commit that added, changed, moved or removed a match
```

![stowe search harbor listing the commit that renamed the song and the one that added it](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/scan-plain.png)

## Know what each remote is missing

![stowe iremote showing the drive one commit behind with the two files a push would send, pushing them, then the offsite store that was never pushed to](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/remote.gif)

```bash
stowe iremote  # every remote and what a push would send; ↵ pushes, a adapts, m mounts
stowe remote   # each remote's address and format
```

The plan is worked out from what each remote was last sent, so it shows
without plugging the drive in.

## Every archive at once

![stowe irepos listing two archives with their changes and remotes, then opening one's history](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/repos.gif)

```bash
stowe irepos [path]   # every archive under a folder; ↵ opens its history
stowe repos [path]    # the same list, printed
```

![stowe repos listing two archives, their changes and where each remote stands](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/repos-plain.png)

## Two shapes of remote

![two stowe remote add commands, one for a local drive and one for an s3 bucket with --format backup, then stowe remote listing the drive as mirror and the offsite bucket as backup](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/remote.png)

```bash
stowe remote add drive local:~/drive                                 # a mirror
stowe remote add offsite s3://example-archive/stowe --format backup  # a backup
stowe convert drive                                                  # flip one in place
```

```
mirror   local:, real browsable folders on a drive or phone
backup   s3:// or --format backup, deduped blobs, compact but not browsable
```

Push to as many as you like, each tracking its own progress, and `stowe convert`
flips a remote between the two **in place**, with no re-upload.

## It will not overwrite what it did not put there

![a file copied onto the mirror by hand, then stowe push halting with a report that the mirror was changed outside stowe and an error telling you to reconcile or use --force, then stowe adapt taking that file into the working tree](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/drift.png)

```bash
stowe adapt drive        # take what changed on the drive into the working tree
stowe push drive --force # or put the drive back the way this commit has it
```

Nothing is written until you decide. `adapt` brings the drive's changes home
and stops at any file that changed on both sides; `--force` overwrites them,
and removes what was dropped on the drive by hand.

## Commands

```bash
stowe unstage [paths]          # drop what is staged, files untouched
stowe pull drive               # rebuild the working tree from a remote
stowe restore <paths>          # bring back committed files from a remote
stowe restore -A --from <C>    # ...or a whole snapshot, as of commit C
stowe remote remove <NAME>     # forget a remote here, never touching it
```

A command that takes a remote uses `origin` when none is named,
`stowe <command> --help` has every flag, and `?` in any screen lists every key.

Every command prints for people rather than for a pipe, but for `status -s`,
whose one line keeps a fixed form for a prompt or a script. A failure names
itself on stderr and exits non-zero.

## Where it keeps things

```
.stowe/            the repo: history, what is staged and your remotes
.stoweignore       the paths every scan skips
<remote>/.stowe/   the remote's history, and every replaced or deleted version
```

## What it ignores

Media folders fill up with things nobody wants versioned. Put a `.stoweignore`
at the repo root, one pattern per line:

```
# comments and blank lines are skipped
# a bare name matches that file or folder anywhere
.DS_Store
# `*` matches any run, `?` exactly one, within a segment
*.tmp
# a trailing slash matches directories only
.thumbnails/
# a pattern with a slash is anchored at the repo root
Renders/proxies/
```

The rules apply to every scan, the working tree **and** your remotes. That
second part matters: a phone's gallery recreates `.thumbnails/` every time it
indexes the folder, and without this it would read as drift and demand `--force`
on every single push.

## Notes

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

## Compatibility

Linux. Windows is deliberately unsupported until it can actually be tested
there.

## License

AGPL-3.0-only
