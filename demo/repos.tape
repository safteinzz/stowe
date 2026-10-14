# Every archive under a folder, and a way into each one's history.
#
#   cd demo && ./render.sh repos

Output "../readme-assets/repos.gif"

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
Type "cd ~ && clear"
Enter
Sleep 500ms
Show
Sleep 300ms
Type "stowe irepos"
Enter
Sleep 2s
Type "j"
Sleep 1.5s
Type "k"
Sleep 800ms
Enter
Sleep 1.8s
Enter
Sleep 2s
Escape
Sleep 600ms
Escape
Sleep 800ms
Type "q"
Sleep 500ms
