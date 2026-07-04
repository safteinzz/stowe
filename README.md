# stowe

**git for the files git chokes on:** music, photos, video, datasets.
Versioned and deduped, pushed to backups you can still *play*.

```sh
cargo install stowe

stowe init
stowe add -A
stowe commit -m "import"
stowe remote add origin local:/mnt/drive
stowe push                     # back it up
```

## Remotes come in two shapes

- **mirror** (`local:`): real, playable folders on a drive or phone. Open it and
  your music is right there; stowe's history hides in a `.stowe/` beside it.
- **backup** (`s3://`, or `--format backup`): deduped content-addressed blobs.
  Compact, not browsable.

Push to as many as you like. Each syncs on its own, whenever you want, and
`stowe convert` flips a remote between the two **in place**, no re-upload.

## Handy

- `stowe restore <file> [--from <commit>]`: undo a change, or time-travel an old version
- `stowe pull`: rebuild the whole library on a new machine
- `stowe adapt <remote>`: pull in songs you dropped onto the drive or phone by hand
- Renames and re-tagged songs are tracked as **moves** (the audio is
  fingerprinted), not re-uploaded

Run `stowe --help` for the rest.

## License

AGPL-3.0-only
