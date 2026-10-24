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

## The git you know, for files git chokes on

![stowe remote add pointing a remote at a drive, then stowe push copying the ten files of a freshly committed archive onto it](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/import.png)

```bash
stowe init                            # make this folder a repo
stowe add -A                          # stage everything in it
stowe commit -m "import the archive"  # record that snapshot
stowe remote add drive local:~/drive  # somewhere to keep it
stowe push drive                      # copy it there
```

Same verbs as git, one linear `main`, no branches and no content diffs. What
follows is only what works differently.

## The backup is just your files

![ls of the mirror showing the Datasets, Music, Photos, Renders and Video folders beside a hidden .stowe folder, then the two songs in Music under their own names](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/mirror.png)

A local remote is a mirror: your real tree at real paths. Plug the drive into
anything and play what is on it. The history sits in `.stowe/` beside it.

## Moves, not deletes and adds

![stowe commit followed by stowe push drive, ending in a report reading plus 2 new, 0 changed, 6 moved, 0 removed](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/push.png)

Git guesses renames from similarity. stowe knows them, because the bytes hash
the same, so a reorganised terabyte pushes as renames on the drive and only new
files cross the wire.

## Know what each remote is missing

![stowe iremote showing the drive one commit behind with the two files a push would send, pushing them, then the offsite store that was never pushed to](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/remote.gif)

Legend: `↑` missing commits · `here` `empty` `unplugged` `mountable` `cloud` whether it is there now

```bash
stowe iremote    # every remote and what a push would send it; ↵ pushes
stowe status -s  # ↑ what origin lacks, ⇡ the most another remote lacks
```

A drive you push to on Monday and a phone you catch up on Friday each keep their
own place, and the plan shows without plugging either in.

## Read the history, file by file

![stowe ilog opening the reorganisation, its moves marked where the names changed, then one photo's versions across the move](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/history.gif)

Legend: `+` new · `~` changed · `⇄` moved · `-` removed · `↑` on no remote yet, dim when on some but not all · `(drive)` the last commit a remote has

```bash
stowe ilog     # commits, their changes grouped, each file's versions; ↵ restores one
stowe iscan    # every commit that touched a name, old or new
```

A file is followed across every move, so its versions read as one list
whatever it was called at the time.

## It will not write where it should not

![a file copied onto the mirror by hand, then stowe push halting with a report that the mirror was changed outside stowe and an error telling you to reconcile or use --force, then stowe adapt taking that file into the working tree](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/drift.png)

```bash
stowe adapt drive         # take what changed on the drive into the working tree
stowe push drive --force  # or put the drive back the way this commit has it
```

A push stops at files changed on the drive by hand, and at an empty folder it
never wrote to, which is what an unmounted drive leaves behind.

## Two shapes of remote

![two stowe remote add commands, one for a local drive and one for an s3 bucket with --format backup, then stowe remote listing the drive as mirror and the offsite bucket as backup](https://gitlab.com/safteinzz/stowe/-/raw/main/readme-assets/remote.png)

```
mirror   a local path: real folders on a drive or phone
backup   s3://, or --format backup: deduped blobs, compact but not browsable
```

`--mount CMD` runs your own script before each use of a remote, so a drive or a
phone over sshfs mounts on demand.

## Commands

```bash
stowe istatus                # see a text file's diff, stage and commit in one screen
stowe status                 # what changed since the last commit
stowe log                    # history, newest first
stowe search <PATTERN>       # every commit that touched a matching name
stowe irepos [path]          # every repo under a folder; ↵ opens its history
stowe repos [path]           # the same list, printed
stowe unstage [paths]        # drop what is staged, files untouched
stowe clone <URL> [path]     # a remote into a new folder, on a new machine
stowe pull drive             # rebuild the working tree from a remote
stowe convert drive          # flip a remote between mirror and backup in place
stowe restore <paths>        # bring back committed files from a remote
stowe restore -A --from <C>  # ...or a whole snapshot, as of commit C
stowe remote                 # each remote's address and format
stowe remote remove <NAME>   # forget a remote here, never touching it
```

A command that takes a remote uses `origin` when none is named,
`stowe <command> --help` has every flag, and `?` in any screen lists every key.

Every command prints for people rather than for a pipe, but for `status -s`,
whose one line keeps a fixed form for a prompt or a script. A failure names
itself on stderr and exits non-zero.

## Where it keeps things

```
.stowe/            the repo: history, what is staged, your remotes and copies of small text files
.stoweignore       the paths every scan skips, gitignore-style, on remotes too
<remote>/.stowe/   the remote's history, and every replaced or deleted version
```

## Notes

- Audio is fingerprinted from the decoded signal, so a song renamed and re-tagged in one go is still a move.
- Text files up to 1 MB are kept as they are staged, so `istatus` diffs them without a remote.
- Names a drive cannot store (exFAT, NTFS) are caught before the push, with a rename offered.

## Compatibility

Linux. Windows is deliberately unsupported until it can actually be tested
there.

## License

AGPL-3.0-only
