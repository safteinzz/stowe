# The history, each commit's changes and a file's versions across its moves.
#
#   cd demo && ./render.sh history

Output "../readme-assets/history.gif"

Set Shell bash
Set FontFamily "JetBrainsMono NF"
Set Theme "Catppuccin Mocha"
Set FontSize 15
# 1056x664 is 99 columns by 30 rows, the size every clip shares, so a README
# shows them at one text size.
Set Width 1056
Set Height 664
Set Padding 20
Set Framerate 20
Set TypingSpeed 55ms

Hide
Type "./stage.sh story >/dev/null 2>&1"
Enter
Sleep 4s
# `exec`, so a stage that refuses to open takes the terminal with it: without it
# the script exits, the outer shell survives, and every keystroke after this
# lands in the *real* home - which is how a README image ends up showing
# somebody's actual files.
Type "exec ./stage.sh shell"
Enter
Sleep 2s
Type "clear"
Enter
Sleep 500ms
Show
Sleep 300ms
Type "stowe ilog"
Enter
Sleep 2s
# the reorganisation: folders renamed, read as moves with only the change marked
Type "jj"
Sleep 2s
# into its changes, and the versions of one file across every move
Enter
Sleep 1.5s
Type "j"
Sleep 2.2s
Escape
Sleep 700ms
# the tabs: what is on no remote yet
Type "l"
Sleep 1.8s
Type "q"
Sleep 500ms
