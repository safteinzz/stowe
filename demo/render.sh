#!/usr/bin/env bash
# Render the README assets in a container, so a machine needs podman or docker
# and nothing else: no vhs, no ttyd, no font, and the same frames on every
# machine that runs it.
#
#   ./render.sh              every tape
#   ./render.sh history      one tape
#
# Each tape stages the world it needs (`./stage.sh up`, `mid` or `story`) in a
# container of its own, which tears the stage down once the tape is recorded.
# They run one at a time, since every tape stages into the same demo/home.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

ENGINE="$(command -v podman || command -v docker || true)"
[ -n "$ENGINE" ] || { echo "render.sh needs podman or docker" >&2; exit 1; }
IMAGE=localhost/stowe-render

TAPES=("$@")
[ ${#TAPES[@]} -gt 0 ] || TAPES=(shots shots-short plain history status remote scan repos)
TAPES=("${TAPES[@]%.tape}")
# vhs exits 0 on a tape that is not there, so a typo would render nothing and
# still say done.
for t in "${TAPES[@]}"; do
  [ -f "$t.tape" ] || { echo "no tape \`$t.tape\` in demo/: pick one of $(ls *.tape | sed 's/\.tape$//' | paste -sd ' ')" >&2; exit 2; }
done

(cd .. && cargo build --release)
# The Dockerfile is the whole build context: nothing in this folder is copied in.
"$ENGINE" build -q -t "$IMAGE" - < Dockerfile > /dev/null

# Run as this user, not root: the images it writes stay yours, and the staged
# shell's prompt ends in `$` as it does on a machine rendering without a
# container, rather than root's `#`.
if [ "$(basename "$ENGINE")" = docker ]; then
  USER_ARGS=(--user "$(id -u):$(id -g)" -e HOME=/tmp)
else
  USER_ARGS=(--userns=keep-id -e HOME=/tmp)
fi

# No network: every drive is a folder inside the stage and the s3 remote is
# listed but never pushed to, so nothing in a take has a reason to leave the
# machine.
for t in "${TAPES[@]}"; do
  echo "── $t.tape"
  "$ENGINE" run --rm --network none "${USER_ARGS[@]}" \
    -v "$(cd .. && pwd):/work/stowe:Z" -w /work/stowe/demo \
    --entrypoint bash "$IMAGE" \
    -c "vhs $t.tape > /dev/null; s=\$?; ./stage.sh down > /dev/null; exit \$s"
done
echo "done: see ../readme-assets/"
