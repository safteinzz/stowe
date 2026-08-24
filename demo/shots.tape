# The two transcript stills, rendered by VHS instead of taken by hand. No tmux,
# no window sizing, no cropping: the size below is the size of both shots, and a
# rerun after an output change replaces them identically.
#
#   cd demo && vhs shots.tape
#
# The short stills live in shots-short.tape, in a frame their own height - VHS
# fixes the frame size for a whole tape, so one tape per frame is the only way
# to keep a three-line command out of a half-empty picture.

# VHS always records a video; with no frames worth keeping there is nothing to
# encode and it stops with "no frames", so this one is a throwaway.
Output "/tmp/vhs-stowe-shots.gif"

Set Shell bash
# The prompt is drawn with nerd-font glyphs; without this they render as boxes.
Set FontFamily "JetBrainsMono NF"
Set Theme "Catppuccin Mocha"
Set FontSize 15
# 1360x700 is about 130 columns by 32 rows: wide enough that no path wraps,
# tall enough for a commit's whole report.
Set Width 1360
Set Height 700
Set Padding 20
Set TypingSpeed 50ms

Hide
Type "./stage.sh mid >/dev/null 2>&1"
Enter
Sleep 3s
Type "./stage.sh shell"
Enter
Sleep 2s
Show
Sleep 500ms

# ---- status: the reorganisation, read as moves --------------------------
Type "stowe status"
Enter
Sleep 2s
Screenshot ../readme-assets/status.png
Sleep 700ms

# ---- commit and push: the moves cost nothing ----------------------------
# Staging happens off camera: `add` prints the same report `commit` is about to,
# and two copies of one diff is not what the picture is for.
Hide
Type "stowe add -A >/dev/null"
Enter
Sleep 3s
Type "clear"
Enter
Sleep 500ms
Show
Sleep 500ms
Type `stowe commit -m "reorganise: date-prefix the shoots, Raw -> source"`
Enter
Sleep 1.5s
Type "stowe push drive"
Enter
Sleep 2s
Screenshot ../readme-assets/push.png
Sleep 1s
