//! The pieces every interactive screen is built from: terminal setup, key
//! predicates and the widgets. Nothing in here knows about any one screen;
//! `app/` builds the drill on top of it.

/// How long a frame waits for a key while something is still arriving. With
/// nothing in flight the loop blocks on the key instead, so an idle TUI costs
/// nothing.
pub const FRAME: std::time::Duration = std::time::Duration::from_millis(33);

pub mod difffeed;
pub mod difftool;
pub mod highlight;
pub mod input;
pub mod widgets;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::supports_keyboard_enhancement;
use ratatui::crossterm::{ExecutableCommand, execute};
use std::io::stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

// ── keyboard enhancement (kitty protocol) ───────────────────────────────────

/// Request DISAMBIGUATE_ESCAPE_CODES if the terminal supports the kitty
/// keyboard protocol. Returns true if pushed (caller must call pop).
/// This makes Ctrl+J distinct from Enter in terminals like VS Code.
pub fn push_keyboard_enhancement() -> bool {
    if supports_keyboard_enhancement().unwrap_or(false) {
        let _ = execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
        true
    } else {
        false
    }
}

pub fn pop_keyboard_enhancement() {
    let _ = stdout().execute(PopKeyboardEnhancementFlags);
}

/// Hand the terminal to `run` (a mount command that may ask for a password),
/// then take it back. Ctrl-C meanwhile reaches the child and not the app,
/// whose screen would otherwise vanish with whatever it was holding.
pub fn suspended<T>(terminal: &mut DefaultTerminal, enhanced: bool, run: impl FnOnce() -> T) -> T {
    if enhanced {
        pop_keyboard_enhancement();
    }
    ratatui::restore();
    let ours = interrupts_end_us();
    ours.store(false, Ordering::SeqCst);
    let out = run();
    ours.store(true, Ordering::SeqCst);
    *terminal = ratatui::init();
    if enhanced {
        push_keyboard_enhancement();
    }
    out
}

/// Whether SIGINT and SIGQUIT end the app the way they would with no handler,
/// which `suspended` turns off while a child has the terminal. The handler is
/// installed once and never removed, because removing one through
/// signal-hook leaves the signal ignored for the rest of the process. A caught
/// signal is reset to the default for the child it execs, where an ignored one
/// would leave it unable to be cancelled.
fn interrupts_end_us() -> &'static Arc<AtomicBool> {
    use signal_hook::consts::{SIGINT, SIGQUIT};
    static OURS: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    OURS.get_or_init(|| {
        let ours = Arc::new(AtomicBool::new(true));
        for signal in [SIGINT, SIGQUIT] {
            let _ = signal_hook::flag::register_conditional_default(signal, ours.clone());
        }
        ours
    })
}

// ── terminal geometry ───────────────────────────────────────────────────────

/// Clamp a scroll offset so the last line can't scroll above the viewport,
/// which is scrolling off the bottom into empty space.
pub fn clamp_scroll(scroll: u16, total_lines: usize, viewport: u16) -> u16 {
    let max = (total_lines.min(u16::MAX as usize) as u16).saturating_sub(viewport);
    scroll.min(max)
}

/// Clamp horizontal scroll so you can't pan past the longest line. `cell_w` is
/// roughly one side's visible width.
pub fn clamp_hscroll(hscroll: u16, max_line: usize, cell_w: u16) -> u16 {
    let max = (max_line.min(u16::MAX as usize) as u16).saturating_sub(cell_w);
    hscroll.min(max)
}
