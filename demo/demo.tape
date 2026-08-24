# The README's opening GIF: one pass through stowe in the order you would meet
# it, rendered by VHS. Nothing is recorded off a screen - VHS replays these
# keystrokes in a headless terminal and draws the frames itself, so it comes out
# the same on every release.
#
#   cd demo && vhs demo.tape
#
# It starts from `./stage.sh up`: the archive exists, nothing is versioned yet,
# which is where a new reader starts too. One beat per thing stowe does, and
# each beat is finished before the next one starts.

Output "../readme-assets/demo.gif"

Set Shell bash
# The prompt is drawn with nerd-font glyphs; without this they render as boxes.
Set FontFamily "JetBrainsMono NF"
Set Theme "Catppuccin Mocha"
Set FontSize 15
# Same frame as the stills: 130 columns by 32 rows.
Set Width 1360
Set Height 700
Set Padding 20
Set Framerate 24
Set TypingSpeed 45ms

# ---- setup, off camera ----------------------------------------------------
Hide
Type "./stage.sh up >/dev/null 2>&1"
Enter
Sleep 3s
Type "./stage.sh shell"
Enter
Sleep 2s
Show
Sleep 800ms

# ===========================================================================
# SCENE 1 - a media archive, and the junk that lives in one
# ===========================================================================
Type "find . -type f | sort"
Enter
Sleep 3.5s

Type "cat .stoweignore"
Enter
Sleep 2.5s

# ===========================================================================
# SCENE 2 - version it
# ===========================================================================
Type "stowe init"
Enter
Sleep 1.5s

# 12 files on disk, 10 staged: the thumbnails and the .tmp were ignored.
Type "stowe add -A"
Enter
Sleep 4s

Type `stowe commit -m "import the archive"`
Enter
Sleep 3s

# ===========================================================================
# SCENE 3 - back it up to something you can still play
# ===========================================================================
Type "stowe remote add drive local:~/drive"
Enter
Sleep 2s

Type "stowe push drive"
Enter
Sleep 2.5s

Type "ls ~/drive && ls ~/drive/Music"
Enter
Sleep 3.5s

# ===========================================================================
# SCENE 4 - reorganise it the way anyone reorganises one
# ===========================================================================
Type `mv "Photos/Iceland 2019" Photos/2019-iceland`
Enter
Sleep 600ms
Type "mv Video/Raw Video/source"
Enter
Sleep 600ms
Type `mv "Music/Nova Kestrel - Paper Harbor (Official Video).mp3" "Music/Nova Kestrel - Paper Harbor.mp3"`
Enter
Sleep 800ms

Type "clear"
Enter
Sleep 500ms

Type "stowe status"
Enter
Sleep 5s

# ===========================================================================
# SCENE 5 - and the reorganisation costs nothing
# ===========================================================================
Type "stowe add -A"
Enter
Sleep 3s
Type `stowe commit -m "reorganise: date-prefix the shoots, Raw -> source"`
Enter
Sleep 2s
Type "stowe push drive"
Enter
Sleep 4s

# ===========================================================================
# SCENE 6 - someone edits the drive directly
# ===========================================================================
Type "clear"
Enter
Sleep 400ms
Type "cp ~/Downloads/promo-cut-v2.MP4 ~/drive/Renders/"
Enter
Sleep 1.2s

Type "stowe push drive"
Enter
Sleep 4s

Type "stowe adapt drive"
Enter
Sleep 4s
