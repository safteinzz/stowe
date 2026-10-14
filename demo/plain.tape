# The plain twins of the clips, as stills: what each prints is read, not
# watched. Its own recording is a throwaway.
#
#   cd demo && ./render.sh plain

Output "/tmp/vhs-stowe-plain.gif"

Set Shell bash
Set FontFamily "JetBrainsMono NF"
Set Theme "Catppuccin Mocha"
Set FontSize 15
Set Width 1056
Set Height 664
Set Padding 20
Set TypingSpeed 50ms

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

# ---- the log --------------------------------------------------------------
Type "stowe log"
Enter
Sleep 1.5s
Screenshot ../readme-assets/history-plain.png
Sleep 500ms

# ---- a name across every commit -------------------------------------------
Hide
Type "clear"
Enter
Sleep 300ms
Show
Type "stowe search harbor"
Enter
Sleep 1.5s
Screenshot ../readme-assets/scan-plain.png
Sleep 500ms

# ---- every archive under the home -----------------------------------------
Hide
Type "cd ~ && clear"
Enter
Sleep 300ms
Show
Type "stowe repos"
Enter
Sleep 1.5s
Screenshot ../readme-assets/repos-plain.png
Sleep 500ms
