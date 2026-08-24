# The short stills: three commands whose whole answer is a handful of lines.
# They get their own tape because VHS fixes the frame size for a whole tape, and
# a nine-line answer in the tall frame of shots.tape is a mostly empty picture.
#
#   cd demo && vhs shots-short.tape
#
# It rebuilds the stage from scratch and replays the commit and push off camera,
# so it can be run on its own and in any order relative to shots.tape.

Output "/tmp/vhs-stowe-shots-short.gif"

Set Shell bash
Set FontFamily "JetBrainsMono NF"
Set Theme "Catppuccin Mocha"
Set FontSize 15
# 1360x310 is about 130 columns by 13 rows, and 130 is what keeps the longest
# error stowe can print on one line.
Set Width 1360
Set Height 310
Set Padding 20
Set TypingSpeed 50ms

Hide
Type "./stage.sh mid >/dev/null 2>&1"
Enter
Sleep 3s
Type "./stage.sh shell"
Enter
Sleep 2s
Type "stowe add -A >/dev/null && stowe commit -m 'reorganise' >/dev/null && stowe push drive >/dev/null"
Enter
Sleep 4s
Type "clear"
Enter
Sleep 500ms
Show
Sleep 500ms

# ---- the mirror is just your files --------------------------------------
Type "ls -la ~/drive"
Enter
Sleep 1.5s
Screenshot ../readme-assets/mirror.png
Sleep 700ms

# ---- both shapes of remote ----------------------------------------------
# Both remotes already exist by now; adding them again is how `remote add`
# updates one, and it puts the syntax and the listing in the same picture.
Hide
Type "clear"
Enter
Sleep 300ms
Show
Sleep 400ms
Type "stowe remote add drive local:~/drive"
Enter
Sleep 1s
Type "stowe remote add offsite s3://example-archive/stowe --format backup"
Enter
Sleep 1s
Type "stowe remote"
Enter
Sleep 1.5s
Screenshot ../readme-assets/remote.png
Sleep 700ms

# ---- drift: someone dropped a file on the drive -------------------------
Hide
Type "clear"
Enter
Sleep 300ms
Show
Sleep 400ms
Type "cp ~/Downloads/promo-cut-v2.MP4 ~/drive/Renders/"
Enter
Sleep 800ms
Type "stowe push drive"
Enter
Sleep 1.5s
Type "stowe adapt drive"
Enter
Sleep 2s
Screenshot ../readme-assets/drift.png
Sleep 1s
