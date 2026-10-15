//! Key predicates and the hint labels that describe them.
//!
//! Every view reads its keys through these, so `j`/`↓` mean the same thing
//! everywhere and a pane title never hand-writes a key name.

use ratatui::crossterm::event::KeyCode;
use std::time::{Duration, Instant};

/// The motions as the help panel spells them, defined once so every screen's
/// help reads the same: the vim keys, a space, the arrows. The `CTRL_` forms
/// drive the bottom pane; Ctrl-arrows are the terminal-safe way to send them,
/// since some terminals can't send a distinct Ctrl-letter.
pub const Y_MOVE: &str = "j/k ↑↓";
pub const CTRL_Y_MOVE: &str = "ctrl-j/k ↑↓";
pub const X_MOVE: &str = "h/l ←→";
pub const CTRL_X_MOVE: &str = "ctrl-h/l ←→";

/// Ctrl-C: what Esc does under a box, in a filter or in a form, and quit on a
/// view.
pub fn is_ctrl_c(code: KeyCode, ctrl: bool) -> bool {
    ctrl && code == KeyCode::Char('c')
}

/// `?`: the help panel, on every screen, a query being typed included.
pub fn is_help(code: KeyCode) -> bool {
    code == KeyCode::Char('?')
}

// ── shared key predicates (arrow keys mirror j/k everywhere) ────────────────

/// Down = j or ↓.
pub fn is_down(c: KeyCode) -> bool {
    matches!(c, KeyCode::Char('j') | KeyCode::Down)
}
/// Up = k or ↑.
pub fn is_up(c: KeyCode) -> bool {
    matches!(c, KeyCode::Char('k') | KeyCode::Up)
}
/// Open/drill-in = Enter only (so l/→ are free for horizontal diff scroll).
pub fn is_open(c: KeyCode) -> bool {
    matches!(c, KeyCode::Enter)
}
/// Back/step-out = Esc (Ctrl-[ sends Esc too), so h/← are free to scroll.
pub fn is_back(c: KeyCode) -> bool {
    matches!(c, KeyCode::Esc)
}
/// Pan left = h or ←.
pub fn is_left(c: KeyCode) -> bool {
    matches!(c, KeyCode::Char('h') | KeyCode::Left)
}
/// Pan right = l or →.
pub fn is_right(c: KeyCode) -> bool {
    matches!(c, KeyCode::Char('l') | KeyCode::Right)
}

/// A gap shorter than this between two presses of one key is the terminal's own
/// key repeat, not a person tapping: repeats arrive 25-40 times a second, and
/// nobody taps faster than about eight.
const REPEAT: Duration = Duration::from_millis(100);
/// How long a key has to be held for each doubling of the step.
const RAMP: Duration = Duration::from_millis(500);
/// The most a single repeat may be multiplied by, so a held key still lands
/// near where it was let go.
const MAX_DOUBLINGS: u32 = 3;

/// How far one press of a movement key goes. A tap is one step, so reading a
/// list or a diff line by line is unchanged; a held key doubles its step for
/// every `RAMP` it has been held, up to eight, and letting go starts over.
/// That is what stands in for paging keys, which many keyboards do not have.
#[derive(Default)]
pub struct Accel {
    key: Option<(KeyCode, bool)>,
    last: Option<Instant>,
    /// When the terminal started repeating the key, None until it has.
    held_since: Option<Instant>,
}

impl Accel {
    /// Steps to move for this press. Every key event goes through here, so any
    /// other key in between ends the hold.
    pub fn steps(&mut self, code: KeyCode, ctrl: bool) -> usize {
        let now = Instant::now();
        let repeat = self.key == Some((code, ctrl))
            && self.last.is_some_and(|t| now.duration_since(t) < REPEAT);
        self.key = Some((code, ctrl));
        self.last = Some(now);
        if !repeat {
            self.held_since = None;
            return 1;
        }
        let since = *self.held_since.get_or_insert(now);
        let ramps = (now.duration_since(since).as_millis() / RAMP.as_millis()) as u32;
        1 << ramps.min(MAX_DOUBLINGS)
    }
}

/// `cur` moved `rows` towards either end of a list `len` long, stopping there.
pub fn stepped(cur: usize, len: usize, down: bool, rows: usize) -> usize {
    if down {
        (cur + rows).min(len.saturating_sub(1))
    } else {
        cur.saturating_sub(rows)
    }
}

/// Fold Ctrl+[ back into Esc. Terminals send Ctrl+[ as the raw ESC byte, but the
/// kitty protocol we push (DISAMBIGUATE_ESCAPE_CODES) turns it into a distinct
/// Ctrl+[ event, so map it back, since Ctrl+[ is Esc in vim muscle memory. Call
/// it once per key event before matching.
pub fn norm_esc(code: KeyCode, ctrl: bool) -> KeyCode {
    if ctrl && matches!(code, KeyCode::Char('[')) {
        KeyCode::Esc
    } else {
        code
    }
}

/// Byte offset of the `idx`-th character, for editing a query by caret position
/// rather than by byte, which a multi-byte character would land in the middle
/// of.
pub fn char_to_byte(s: &str, idx: usize) -> usize {
    s.char_indices().nth(idx).map(|(b, _)| b).unwrap_or(s.len())
}
