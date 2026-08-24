#!/usr/bin/env bash
# A staged home for the README screenshots and the demo GIF: an invented media
# archive and an invented drive to back it up to. Nothing here touches your real
# files - HOME and every XDG variable are redirected into ./home, and the only
# stowe that runs is this repo's release build.
#
#   ./stage.sh up      build the archive (no repo yet, this is the GIF's start)
#   ./stage.sh mid     up, then init/commit/push and reorganise (the stills)
#   ./stage.sh shell   a shell in the archive where `stowe` is this build
#   ./stage.sh down    unmount anything under the stage, then delete it
#
# Every file is generated here: photos and video are random bytes with plausible
# names, the music is tones rendered by ffmpeg, so the audio fingerprint is
# fingerprinting real audio and nothing on screen came off a real library. The
# s3 remote is an example.com-grade invention (RFC 2606) that is listed and
# never pushed to, so no credentials are needed to render anything.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STAGE="$HERE/home"
ARCHIVE="$STAGE/Archive"
DRIVE="$STAGE/drive"
# The `stowe` symlink lives inside the stage, so `down` takes it with one
# guarded delete and nothing is left in demo/ to gitignore.
BIN="$STAGE/.bin"
STOWE="$HERE/../target/release/stowe"

# Written by `up`, required by `down`. See the guard further down.
MARKER=".stowe-demo-stage"

# Used with `env -i`, so this list is not "the real environment plus overrides"
# but everything there is: HOME alone would send anything stowe caches back into
# your real state dir, and overriding leaves whatever variable nobody thought of
# still pointing at the real thing. The ceiling stops git discovery at the stage:
# the stage sits inside this repository's working tree, so without it a prompt
# with a git segment would report *stowe's* branch from inside a media archive.
env_for_stage() {
  echo "HOME=$STAGE" \
       "XDG_CONFIG_HOME=$STAGE/.config" \
       "XDG_DATA_HOME=$STAGE/.local/share" \
       "XDG_STATE_HOME=$STAGE/.local/state" \
       "XDG_CACHE_HOME=$STAGE/.cache" \
       "GIT_CEILING_DIRECTORIES=$STAGE" \
       "PATH=$BIN:/usr/local/bin:/usr/bin:/bin" \
       "TERM=${TERM:-xterm-256color}" \
       "COLORTERM=truecolor" \
       "LANG=C.UTF-8"
}

in_archive() { (cd "$ARCHIVE" && env -i $(env_for_stage) "$STOWE" "$@"); }

# A file of `$2` KiB of random bytes at `$1`. Random, not zeros, so every
# fixture has its own content hash and dedup never collapses two of them.
blob() {
  mkdir -p "$(dirname "$1")"
  head -c "$(( $2 * 1024 ))" /dev/urandom > "$1"
}

# A real, decodable track: a tone at its own frequency, so every song has its
# own fingerprint. Tags are written in, which is what makes the re-tag scene
# honest - the bytes change and the decoded audio does not.
track() {
  local path="$1" freq="$2" title="$3" artist="$4"
  mkdir -p "$(dirname "$path")"
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "sine=frequency=$freq:duration=12" \
    -metadata title="$title" -metadata artist="$artist" \
    -c:a libmp3lame -b:a 96k "$path"
}

build_archive() {
  mkdir -p "$ARCHIVE"

  blob "$ARCHIVE/Photos/Iceland 2019/DSC_0431.NEF" 320
  blob "$ARCHIVE/Photos/Iceland 2019/DSC_0478.NEF" 296
  blob "$ARCHIVE/Photos/Iceland 2019/DSC_0563.NEF" 344
  blob "$ARCHIVE/Video/Raw/drone-coastline.MP4"    768
  blob "$ARCHIVE/Video/Raw/interview-take-3.MP4"   640
  blob "$ARCHIVE/Renders/kitchen-final-4k.exr"     512
  blob "$ARCHIVE/Datasets/census-2021/households.parquet" 208

  track "$ARCHIVE/Music/Nova Kestrel - Paper Harbor (Official Video).mp3" 392 \
        "Paper Harbor" "Nova Kestrel"
  track "$ARCHIVE/Music/Halcyon Drift - Tidewater.mp3" 466 \
        "Tidewater" "Halcyon Drift"

  # The junk a camera and a phone leave behind, and the .stoweignore that keeps
  # it out of every scan - local and remote.
  blob "$ARCHIVE/Photos/Iceland 2019/.thumbnails/DSC_0431.jpg" 12
  blob "$ARCHIVE/Renders/kitchen-final-4k.exr.tmp" 4
  cat > "$ARCHIVE/.stoweignore" <<'IGNORE'
.thumbnails/
*.tmp
IGNORE

  # What the reorganisation scene copies in, so the new files arrive from
  # somewhere a viewer recognises instead of appearing out of nowhere.
  blob "$STAGE/Downloads/DSC_0587.NEF" 288
  blob "$STAGE/Downloads/migration.parquet" 176
  # And what someone drops straight onto the drive in the drift scene.
  blob "$STAGE/Downloads/promo-cut-v2.MP4" 224
}

# The moves the stills open on: two folders renamed, one file renamed, two new
# files. Pure renames, so `status` recognises them on content alone.
reorganise() {
  mv "$ARCHIVE/Photos/Iceland 2019" "$ARCHIVE/Photos/2019-iceland"
  mv "$ARCHIVE/Video/Raw" "$ARCHIVE/Video/source"
  mv "$ARCHIVE/Music/Nova Kestrel - Paper Harbor (Official Video).mp3" \
     "$ARCHIVE/Music/Nova Kestrel - Paper Harbor.mp3"
  cp "$STAGE/Downloads/DSC_0587.NEF" "$ARCHIVE/Photos/2019-iceland/DSC_0587.NEF"
  cp "$STAGE/Downloads/migration.parquet" "$ARCHIVE/Datasets/census-2021/migration.parquet"
}

up() {
  down_quiet
  mkdir -p "$STAGE"
  # Stamp it before anything else, so a later `down` can prove this tree is ours.
  : > "$STAGE/$MARKER"
  build_archive
  echo "staged in $STAGE"
  echo
  echo "  ./stage.sh mid    init, commit, push, then reorganise (the stills)"
  echo "  ./stage.sh shell  a shell in the archive where stowe is this build"
  echo "  ./stage.sh down   tear it all down"
}

# Where the stills start: a library already imported and already on the drive,
# then reorganised the way anyone reorganises one.
mid() {
  up > /dev/null
  mkdir -p "$DRIVE"
  in_archive init > /dev/null
  in_archive add -A > /dev/null
  in_archive commit -m "import the archive" > /dev/null
  in_archive remote add drive "local:~/drive" > /dev/null
  # An offsite blob store, listed and never pushed to: the remote list is the
  # one picture that has to show both shapes of remote at once.
  in_archive remote add offsite "s3://example-archive/stowe" --format backup > /dev/null
  in_archive push drive > /dev/null
  reorganise
  echo "staged in $STAGE, committed, pushed to the drive and reorganised"
}

# ---------------------------------------------------------------------------
# the teardown guard - identical in every crate's rig
# ---------------------------------------------------------------------------
# A rig is a convenience script with a recursive delete in it, run half
# attentively while thinking about something else, against a path some scenario
# may have mounted a remote filesystem onto. Both halves of that have already
# happened in this workflow: a stage path that pointed somewhere real and was
# deleted because the script trusted its own variable, and an sshfs mount inside
# a staged home torn down with `rm -rf`, which walked through the mountpoint and
# deleted the dotfiles on the machine at the far end. So the delete is proved
# rather than trusted.
refuse() { echo "REFUSING to delete $STAGE: $1" >&2; exit 1; }

assert_safe_to_delete() {
  case "$STAGE" in
    /*) ;;
    *) refuse "the stage path must be absolute" ;;
  esac
  # Resolve symlinks first: a link pointing the stage at something real must not
  # let a delete through on the strength of a harmless-looking path.
  local real
  real="$(cd "$STAGE" && pwd -P)" || refuse "cannot resolve the path"
  case "$real" in
    / | /home | /root | /usr | /etc | /var | /opt | /srv | /boot | /tmp)
      refuse "that is a system directory" ;;
  esac
  [ "$real" = "$HOME" ] && refuse "that is your home directory"
  case "$HOME/" in
    "$real"/*) refuse "your home directory is inside it" ;;
  esac
  # The real gate: only ever delete a tree this script built and stamped.
  [ -f "$real/$MARKER" ] || refuse "no \`$MARKER\` in it, so this script did not build it"
  # Unmount anything under it, longest path first, then check again: a recursive
  # delete walks straight through a mountpoint and removes the far side.
  local mp
  while read -r mp; do
    [ -n "$mp" ] || continue
    echo "unmounting $mp"
    fusermount -u "$mp" 2> /dev/null || umount "$mp" 2> /dev/null || true
  done < <(awk -v s="$real/" '$2 ~ "^"s {print length($2), $2}' /proc/mounts |
             sort -rn | cut -d' ' -f2-)
  if awk -v s="$real/" '$2 ~ "^"s {found=1} END {exit !found}' /proc/mounts; then
    refuse "something is still mounted under it; unmount it by hand and rerun"
  fi
}

down_quiet() {
  [ -d "$STAGE" ] || return 0
  assert_safe_to_delete
  # --one-file-system as a second net, in case the mount check was wrong.
  rm -rf --one-file-system "$STAGE"
}

# ---------------------------------------------------------------------------
# the shell in frame - identical in every crate's rig
# ---------------------------------------------------------------------------
# The prompt is invented, and deliberately not the renderer's own. Sourcing a
# real ~/.bashrc paints a different picture on every machine that regenerates
# the assets, which defeats the point of keeping the rig in the repo: these
# images are a build output, and a build output that depends on whose machine
# ran it is not reproducible. A username is not a leak, but `user@host` is the
# same for everyone, and it is the same string in all six rigs so the frames
# match. No tape sets a theme either, so every frame is VHS's default black.
write_demorc() {
  cat > "$STAGE/.demorc" <<'EOF'
PS1='\[\e[38;5;114m\]user@host\[\e[0m\]:\[\e[38;5;110m\]\w\[\e[0m\]\$ '
unset PROMPT_COMMAND
HISTFILE=
clear
EOF
}

# A shell in the archive where `stowe` is this build, so the frame shows the
# command you would type rather than a path into target/release.
open_shell() {
  mkdir -p "$BIN"
  ln -sf "$(cd "$(dirname "$STOWE")" && pwd)/stowe" "$BIN/stowe"
  write_demorc
  (cd "$ARCHIVE" && env -i $(env_for_stage) \
    bash --noprofile --rcfile "$STAGE/.demorc" -i)
}

case "${1:-up}" in
  up)     up ;;
  mid)    mid ;;
  reorg)  reorganise ;;
  shell)  open_shell ;;
  down)   down_quiet; echo "torn down" ;;
  *)      echo "usage: $0 [up|mid|reorg|shell|down]" >&2; exit 2 ;;
esac
