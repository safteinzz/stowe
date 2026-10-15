//! Pane furniture, the footer, the boxes and the form: what every view draws
//! with, none of it knowing which view is drawing.

use crate::tui::clamp_scroll;
use crate::tui::input::{char_to_byte, is_back, is_down, is_up};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Wrap,
};
use std::time::Duration;

/// A bordered block whose border is bright when the pane is focused.
pub fn pane_block(title: impl Into<Line<'static>>, active: bool) -> Block<'static> {
    let color = if active { Color::Cyan } else { Color::DarkGray };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title(title)
}

/// How long a note sits in a view's footer before the key hints come back.
pub const NOTE: Duration = Duration::from_secs(3);

// The words every footer and key row is built from, so the same key reads the
// same on every screen and a hand-typed legend stands out.
pub const CREATE: &str = "c create";
pub const EDIT: &str = "e edit";
pub const DEL: &str = "d del";
pub const FIND: &str = "/ find";
pub const REFRESH: &str = "r refresh";
pub const QUIT: &str = "q quit";
pub const HELP: &str = "? help";
pub const KEEP: &str = "↵ keep";
pub const SELECT: &str = "↵ select";
pub const BACK: &str = "esc back";
pub const CANCEL: &str = "esc cancel";
pub const CLOSE: &str = "esc close";
pub const SEP: &str = " · ";

/// The key rows of the box kinds, each written by that kind's drawing function.
pub const GATE_KEYS: &[&str] = &[SELECT, CANCEL];
pub const READER_KEYS: &[&str] = &[CLOSE];

/// The footer of a pane whose filter is being typed into: every letter goes
/// into the query, so only keys that are not letters are offered.
pub const FILTER_KEYS: &[&str] = &[KEEP, BACK];

/// What a `:` line asked for.
pub enum Command {
    Help,
    Quit,
    Unknown(String),
}

/// What a key did to an open `:` line.
pub enum Typed {
    Open,
    Cancel,
    Run(Command),
}

/// The `:` line in a view's footer: what has been typed after the colon. It
/// opens showing `help` as a placeholder, and Enter on an empty line runs
/// exactly that, so the one command worth knowing needs nothing typed.
#[derive(Default)]
pub struct CommandLine {
    text: String,
}

impl CommandLine {
    pub fn on_key(&mut self, code: KeyCode) -> Typed {
        match code {
            KeyCode::Esc => Typed::Cancel,
            KeyCode::Enter => Typed::Run(match self.text.trim() {
                "" | "help" | "h" | "keys" => Command::Help,
                "q" | "quit" => Command::Quit,
                other => Command::Unknown(other.to_string()),
            }),
            KeyCode::Backspace if self.text.is_empty() => Typed::Cancel,
            KeyCode::Backspace => {
                self.text.pop();
                Typed::Open
            }
            KeyCode::Char(c) => {
                self.text.push(c);
                Typed::Open
            }
            _ => Typed::Open,
        }
    }
}

/// A pane's live filter: what has been typed into it, and where the caret sits
/// in that text. Empty means the pane shows everything its scope keeps.
#[derive(Default)]
pub struct Query {
    pub text: String,
    pub caret: usize,
}

impl Query {
    /// Does a row survive this filter? Terms are whitespace-separated and every
    /// one of them has to appear somewhere in the row, so `pablo fix` narrows
    /// to what both words are in rather than to either.
    pub fn keeps(&self, row: &str) -> bool {
        if self.text.trim().is_empty() {
            return true;
        }
        let row = row.to_lowercase();
        self.text
            .split_whitespace()
            .all(|term| row.contains(&term.to_lowercase()))
    }

    /// Start a new query: `/` replaces whatever a previous Enter kept, as it
    /// does in every app. True when that dropped a kept query, so the caller
    /// filters again.
    pub fn open(&mut self) -> bool {
        let had = !self.text.is_empty();
        self.text.clear();
        self.caret = 0;
        had
    }

    /// A key typed into the open filter, edited like a shell line: `←→`
    /// `ctrl-b/f` a character, `alt-b/f` a word, `home`/`end` `ctrl-a/e` either
    /// end, `backspace` `ctrl-h` `delete` `ctrl-d` a character, `alt-backspace`
    /// `alt-d` a word, `ctrl-w` back to a space, `ctrl-u` everything before the
    /// caret. Returns whether the text changed, so the caller re-filters only
    /// then. Not `ctrl-k`, which a shell kills to the end with: it moves the
    /// pane below everywhere else.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let mut chars: Vec<char> = self.text.chars().collect();
        let len = chars.len();
        let mut at = self.caret.min(len);
        match key.code {
            KeyCode::Left => at = at.saturating_sub(1),
            KeyCode::Right => at = (at + 1).min(len),
            KeyCode::Home => at = 0,
            KeyCode::End => at = len,
            KeyCode::Backspace if alt || ctrl => {
                let from = word_start(&chars, at, char::is_alphanumeric);
                chars.drain(from..at);
                at = from;
            }
            KeyCode::Backspace if at > 0 => {
                chars.remove(at - 1);
                at -= 1;
            }
            KeyCode::Delete if at < len => {
                chars.remove(at);
            }
            KeyCode::Char(c) if ctrl => match c {
                'a' => at = 0,
                'e' => at = len,
                'b' => at = at.saturating_sub(1),
                'f' => at = (at + 1).min(len),
                'h' if at > 0 => {
                    chars.remove(at - 1);
                    at -= 1;
                }
                'd' if at < len => {
                    chars.remove(at);
                }
                // back to a space, so a whole path goes at once
                'w' => {
                    let from = word_start(&chars, at, |c| !c.is_whitespace());
                    chars.drain(from..at);
                    at = from;
                }
                'u' => {
                    chars.drain(..at);
                    at = 0;
                }
                _ => {}
            },
            KeyCode::Char(c) if alt => match c {
                'b' => at = word_start(&chars, at, char::is_alphanumeric),
                'f' => at = word_end(&chars, at),
                'd' => {
                    let to = word_end(&chars, at);
                    chars.drain(at..to);
                }
                _ => {}
            },
            KeyCode::Char(c) => {
                chars.insert(at, c);
                at += 1;
            }
            _ => {}
        }
        self.caret = at;
        let text: String = chars.into_iter().collect();
        let changed = text != self.text;
        self.text = text;
        changed
    }

    /// The text split at the caret, for drawing it with the caret between.
    pub fn split(&self) -> (&str, &str) {
        self.text.split_at(char_to_byte(&self.text, self.caret))
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }

    /// The filter as its pane's title shows it, led by `key`, the key that
    /// opened it, so a query can never look like it went to the other pane. A
    /// `filterable` pane with nothing typed offers that key instead, dimmed.
    pub fn title_span(&self, key: &str, editing: bool, filterable: bool) -> Option<Span<'static>> {
        // `/fix`, but `ctrl-f fix`: a key that is a word needs the space
        let lead = match key.chars().count() {
            1 => key.to_string(),
            _ => format!("{key} "),
        };
        if editing {
            let (before, after) = self.split();
            Some(Span::raw(format!("   {lead}{before}▏{after}")))
        } else if !self.text.is_empty() {
            Some(Span::raw(format!("   {lead}{}", self.text)))
        } else if filterable {
            Some(Span::styled(
                format!("   {key} find"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ))
        } else {
            None
        }
    }
}

/// Where the word ending at `at` starts: skip what is not part of a word, then
/// what is.
fn word_start(chars: &[char], mut at: usize, in_word: fn(char) -> bool) -> usize {
    while at > 0 && !in_word(chars[at - 1]) {
        at -= 1;
    }
    while at > 0 && in_word(chars[at - 1]) {
        at -= 1;
    }
    at
}

fn word_end(chars: &[char], mut at: usize) -> usize {
    while at < chars.len() && !chars[at].is_alphanumeric() {
        at += 1;
    }
    while at < chars.len() && chars[at].is_alphanumeric() {
        at += 1;
    }
    at
}

/// The row under a view: the keys it answers to, or for `NOTE` what the last
/// action came to, green when it worked and yellow when it did not, or the `:`
/// line while one is open. Keys live here rather than on pane borders, which
/// only have room for what a pane is. `help` (`HELP`)
/// is kept at the right edge however narrow the window, since it is the
/// way to every key that falls off.
pub fn key_footer(
    actions: &[&str],
    note: Option<&(bool, String)>,
    command: Option<&CommandLine>,
    help: Option<&str>,
    width: u16,
) -> Paragraph<'static> {
    let dim = Style::default().fg(Color::DarkGray);
    let line = match (command, note, help) {
        (Some(cmd), _, _) => {
            let mut spans = vec![Span::raw(format!(" :{}▏", cmd.text))];
            if cmd.text.is_empty() {
                spans.push(Span::styled("help", dim.add_modifier(Modifier::DIM)));
            }
            Line::from(spans)
        }
        (None, Some((true, text)), _) => Line::from(Span::styled(
            format!(" ✓ {text}"),
            Style::default().fg(Color::Green),
        )),
        (None, Some((false, text)), _) => Line::from(Span::styled(
            format!(" ✗ {text}"),
            Style::default().fg(Color::Yellow),
        )),
        (None, None, None) => Line::from(Span::styled(fit(actions, width), dim)),
        (None, None, Some(help)) => {
            let pin = format!("{help} ");
            let pin_w = pin.chars().count();
            // a gap before the pin as wide as a separator, so it never reads
            // as part of the last key
            let room = (width as usize).saturating_sub(pin_w + SEP.chars().count());
            let left = fit(actions, room as u16);
            let pad = (width as usize).saturating_sub(left.chars().count() + pin_w);
            Line::from(vec![
                Span::styled(left, dim),
                Span::raw(" ".repeat(pad)),
                Span::styled(pin, dim),
            ])
        }
    };
    Paragraph::new(line)
}

/// As many whole keys as fit in `width`, in order: a key cut off mid-word says
/// less than one left off.
fn fit(keys: &[&str], width: u16) -> String {
    let mut line = String::new();
    for key in keys {
        let next = if line.is_empty() {
            format!(" {key}")
        } else {
            format!("{line}{SEP}{key}")
        };
        if next.chars().count() > width as usize {
            break;
        }
        line = next;
    }
    line
}

/// The most tabs a slider shows at once. More than this and a border runs out
/// of room, so a search with a dozen terms shows the ones around where you are.
const MAX_TABS: usize = 5;

/// A scope slider as tabs: `│` between them, and the picked one filled with
/// the border's cyan, the way a picked button is. On a pane border, whose own
/// text is already cyan, a cyan-and-bold stop the way the tab bars in the other
/// crates mark it did not stand out from the rest. Past `MAX_TABS` it shows a
/// window that slides with the picked one, and `‹`/`›` say more are hidden.
pub fn scope_tabs(labels: &[&str], picked: usize) -> Vec<Span<'static>> {
    let first = picked
        .saturating_sub(MAX_TABS / 2)
        .min(labels.len().saturating_sub(MAX_TABS));
    let last = (first + MAX_TABS).min(labels.len());
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans = Vec::new();
    if first > 0 {
        spans.push(Span::styled("‹", dim));
    }
    for (i, label) in labels.iter().enumerate().take(last).skip(first) {
        if i > first {
            spans.push(Span::styled("│", dim));
        }
        let style = if i == picked {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Reset)
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }
    if last < labels.len() {
        spans.push(Span::styled("›", dim));
    }
    spans
}

/// A scrollbar on `area`'s right border for `total` rows, `view` of them on
/// screen from `top`, where `area` is the bordered rect it sits on. Nothing is
/// drawn when every row fits.
pub fn vscrollbar(frame: &mut Frame, area: Rect, total: usize, top: usize, view: usize) {
    if total <= view {
        return;
    }
    let mut state = ScrollbarState::new(total - view).position(top);
    let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None);
    frame.render_stateful_widget(bar, area.inner(Margin::new(0, 1)), &mut state);
}

/// A scrollbar on `area`'s bottom border for content `total` columns wide,
/// `view` of them on screen from `left`, kept one column off each corner.
/// Nothing is drawn when every column fits.
pub fn hscrollbar(frame: &mut Frame, area: Rect, total: usize, left: usize, view: usize) {
    if total <= view {
        return;
    }
    let mut state = ScrollbarState::new(total - view).position(left);
    // `■` renders vertically centered and medium-weight - between the too-thin,
    // low-sitting `▬` and the full-cell block `█`.
    let bar = Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
        .begin_symbol(None)
        .end_symbol(None)
        .thumb_symbol("■");
    frame.render_stateful_widget(bar, area.inner(Margin::new(1, 0)), &mut state);
}

/// Scrollbar for a diff pane: thumb tracks the scroll line.
pub fn diff_scrollbar(frame: &mut Frame, area: Rect, total_lines: usize, scroll: u16) {
    let view = area.height.saturating_sub(2) as usize;
    vscrollbar(frame, area, total_lines, scroll as usize, view);
}

/// Scrollbar for a list pane: thumb tracks the visible window (the list's
/// `offset`). Call it right after rendering the list so the offset is current.
pub fn list_scrollbar(frame: &mut Frame, area: Rect, total: usize, offset: usize) {
    let view = area.height.saturating_sub(2) as usize;
    vscrollbar(frame, area, total, offset, view);
}

/// Horizontal scrollbar along a diff pane's bottom border. `max_line` is the
/// widest content line, `cell_w` the visible columns per side, `hscroll` the pan
/// offset. Drawn only when the content is wider than one cell (else there's
/// nothing to pan).
pub fn diff_hscrollbar(frame: &mut Frame, area: Rect, max_line: usize, cell_w: u16, hscroll: u16) {
    hscrollbar(frame, area, max_line, hscroll as usize, cell_w as usize);
}

// ── modal ───────────────────────────────────────────────────────────────────

/// Columns between tab stops once a tab is expanded.
const TAB_WIDTH: usize = 4;

/// `text` with tabs expanded and every other control character but the line
/// breaks in caret notation (`^M`). A box shows another program's words, and
/// ratatui hands a tab to the terminal as it is, which jumps the cursor and
/// leaves every cell after it one frame out of step.
fn printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut col = 0usize;
    for c in text.chars() {
        match c {
            '\n' => {
                out.push('\n');
                col = 0;
            }
            '\t' => {
                let pad = TAB_WIDTH - col % TAB_WIDTH;
                out.extend(std::iter::repeat_n(' ', pad));
                col += pad;
            }
            c if c.is_ascii_control() => {
                out.push('^');
                out.push(char::from(c as u8 ^ 0x40));
                col += 2;
            }
            c if c.is_control() => {
                out.push(char::REPLACEMENT_CHARACTER);
                col += 1;
            }
            c => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

/// A box over the panes with something the user has to read: a title, a body,
/// and no way past it but dismissing it.
///
/// It exists because the alternative is a line in a pane title, which is where
/// a failed `git difftool` used to be reported and where nobody looked: the
/// screen came back unchanged and the run looked like a no-op. A view holds an
/// `Option<Modal>` and hands it the keys first, the way the drill gates on its
/// confirm popup.
pub struct Modal {
    title: String,
    body: String,
    /// The help panel's styled rows, drawn in place of `body` when present.
    help: Option<Vec<Line<'static>>>,
    scroll: u16,
    /// Yellow for an alert, cyan for a reader: the one thing that differs.
    colour: Color,
}

/// One group of a help panel: a heading, then `(keys, what they do)` rows,
/// where a row with no keys is a note about the group.
pub type HelpSection = (&'static str, &'static [(&'static str, &'static str)]);

/// The width of a help panel's key column, so every description starts in one
/// place.
const HELP_KEYS: usize = 16;

fn help_lines(sections: &[HelpSection]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (section, entries) in sections {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            *section,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        for (keys, what) in *entries {
            if keys.is_empty() {
                lines.push(Line::styled(
                    format!("  {what}"),
                    Style::default().add_modifier(Modifier::DIM),
                ));
                continue;
            }
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {keys:<HELP_KEYS$}"),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(*what),
            ]));
        }
    }
    lines
}

impl Modal {
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Modal {
        Modal {
            title: title.into(),
            body: printable(&body.into()),
            help: None,
            scroll: 0,
            colour: Color::Yellow,
        }
    }

    /// The help panel: a reader of `sections`, titled `help`, which also pages
    /// (`ctrl-d ctrl-u`), jumps to either end (`g G`) and closes on `?`.
    pub fn help(sections: &[HelpSection]) -> Modal {
        Modal {
            help: Some(help_lines(sections)),
            colour: Color::Cyan,
            ..Modal::new("help", "")
        }
    }

    /// A reader: the same box in cyan, for something read by choice rather
    /// than something that went wrong. `rows` is a label and what goes with
    /// it, one per line, the labels in one column. An empty label continues
    /// the row above.
    pub fn reader(title: impl Into<String>, rows: &[(String, String)]) -> Modal {
        let width = rows
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0)
            + 2;
        let body = rows
            .iter()
            .map(|(key, does)| format!("{key:<width$}{does}"))
            .collect::<Vec<_>>()
            .join("\n");
        Modal {
            title: title.into(),
            body: printable(&body),
            help: None,
            scroll: 0,
            colour: Color::Cyan,
        }
    }

    /// Free text under a reader's rows, a blank line apart: a message, which
    /// wraps on its own rather than hanging under the rows' second column.
    pub fn with_text(mut self, text: &str) -> Modal {
        if !text.is_empty() {
            self.body = format!("{}\n\n{}", self.body, printable(text));
        }
        self
    }

    /// Handle one key while the modal is up. Returns true when it was
    /// dismissed, so the caller drops it. Movement keys scroll a body too long
    /// for the box; anything else is swallowed, so a stray keypress can't
    /// close a message before it is read.
    pub fn on_key(&mut self, code: KeyCode, ctrl: bool) -> bool {
        if self.help.is_some() {
            return self.help_key(code, ctrl);
        }
        if is_down(code) {
            self.scroll = self.scroll.saturating_add(1);
        } else if is_up(code) {
            self.scroll = self.scroll.saturating_sub(1);
        } else if is_back(code) || matches!(code, KeyCode::Enter | KeyCode::Char('q' | ' ')) {
            return true;
        }
        false
    }

    /// `on_key` for the help panel. `draw` clamps the scroll, so `G` can ask
    /// for more than there is.
    fn help_key(&mut self, code: KeyCode, ctrl: bool) -> bool {
        let half = 10;
        match code {
            KeyCode::Char('d') if ctrl => self.scroll = self.scroll.saturating_add(half),
            KeyCode::Char('u') if ctrl => self.scroll = self.scroll.saturating_sub(half),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(half),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(half),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = u16::MAX,
            _ if is_down(code) => self.scroll = self.scroll.saturating_add(1),
            _ if is_up(code) => self.scroll = self.scroll.saturating_sub(1),
            _ if is_back(code) => return true,
            KeyCode::Enter | KeyCode::Char('q' | ' ' | '?') => return true,
            _ => {}
        }
        false
    }

    /// Draw it centered in `area`, the panes above the footer. A body taller
    /// than the box scrolls under a key row that stays put, so the way out is
    /// on screen however far it is scrolled.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(lines) = &self.help {
            let lines = lines.clone();
            return self.draw_help(frame, area, lines);
        }
        let width = box_width(area.width);
        // Measured from the wrapped text, never its line count: counting
        // unwrapped lines is what clips the bottom off a long message.
        let body_h = wrapped_height(&self.body, box_inner_width(width)) as u16;
        // The body, a blank, the keys.
        let rect = popup_area(area, width, box_height(body_h + 2, area.height));

        let block = box_block(self.colour, &self.title);
        let inner = block.inner(rect);
        let [text_area, _, keys_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        self.scroll = clamp_scroll(self.scroll, body_h as usize, text_area.height);

        frame.render_widget(Clear, rect); // wipe whatever's underneath
        frame.render_widget(block, rect);
        let lines: Vec<Line> = self
            .body
            .lines()
            .map(|l| Line::raw(l.to_string()))
            .collect();
        let body = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        frame.render_widget(body, text_area);
        frame.render_widget(Paragraph::new(box_hint(READER_KEYS)), keys_area);
        vscrollbar(
            frame,
            rect,
            body_h as usize,
            self.scroll as usize,
            text_area.height as usize,
        );
    }

    /// The help panel's `draw`: its rows are never wrapped, and a scrollbar
    /// runs down the right border once they are taller than the box.
    fn draw_help(&mut self, frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
        let width = box_width(area.width);
        // The body, a blank, the keys.
        let rect = popup_area(area, width, box_height(lines.len() as u16 + 2, area.height));
        let block = box_block(self.colour, &self.title);
        let inner = block.inner(rect);
        let [text_area, _, keys_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        self.scroll = clamp_scroll(self.scroll, lines.len(), text_area.height);

        frame.render_widget(Clear, rect);
        frame.render_widget(block, rect);
        let shown = text_area.height as usize;
        let total = lines.len();
        frame.render_widget(Paragraph::new(lines).scroll((self.scroll, 0)), text_area);
        frame.render_widget(Paragraph::new(box_hint(READER_KEYS)), keys_area);
        vscrollbar(frame, rect, total, self.scroll as usize, shown);
    }
}

// ── the house box ───────────────────────────────────────────────────────────
// Every overlay is built from these, so only its colour and its buttons carry
// meaning: gate red, alert yellow, offer/picker/form/reader cyan.

/// Narrowest a box may be, so a two-word message still reads as a box.
pub const BOX_MIN_W: u16 = 24;
/// Widest, so one long line does not stretch a box across a 200-column screen.
pub const BOX_MAX_W: u16 = 88;
/// Rows the chrome costs: two borders plus the single top padding row.
pub const BOX_CHROME_H: u16 = 3;
/// Columns the chrome costs: two borders plus two columns of padding a side.
pub const BOX_CHROME_W: u16 = 6;

/// How wide a box is on a screen this wide.
pub fn box_width(screen_w: u16) -> u16 {
    screen_w.saturating_sub(4).clamp(BOX_MIN_W, BOX_MAX_W)
}

/// The columns a body actually gets, which is what it must be wrapped to.
pub fn box_inner_width(width: u16) -> usize {
    width.saturating_sub(BOX_CHROME_W).max(1) as usize
}

/// How tall a box holding `body_rows` *wrapped* rows is, capped at the screen.
/// Pass the wrapped count, never the line count: measuring the unwrapped text
/// is what clips a modal's last row off and makes it look unanswerable.
///
/// The cap is the whole screen and not some fraction of it. A box is drawn over
/// a `Clear`, so it owns the screen while it is up anyway, and a fraction only
/// decides in advance that a long one gets cut off.
pub fn box_height(body_rows: u16, screen_h: u16) -> u16 {
    let floor = BOX_CHROME_H + 1;
    body_rows
        .saturating_add(BOX_CHROME_H)
        .clamp(floor, screen_h.max(floor))
}

/// The bordered block every box wears: a spaced title on the top border, and
/// otherwise an unbroken frame in the colour that says what kind it is.
///
/// Nothing else is written on the border. Keys go in the body, through
/// `box_hint`: a frame with a sentence along the bottom of it stops reading as
/// a frame, and the title then has to compete with it.
pub fn box_block(colour: Color, title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(colour))
        // One blank row above the content and none below it: the key line is
        // the last thing in the box, and a blank under it is a wasted row that
        // makes the frame look loose.
        .padding(Padding::new(2, 2, 1, 0))
        .title(format!(" {} ", title.trim()))
}

/// The line of keys a box ends with, as the last row of its body. Every kind
/// puts it in the same place, so it is where the eye already is.
///
/// Quieter than anything else in the box, deliberately: an unfocused field
/// label is the default foreground dimmed, so this goes a step below that with
/// `DarkGray` dimmed again. Separation is the blank row above it and its fixed
/// place at the bottom, not brightness. Colouring it only made a guideline look
/// like something worth reading.
pub fn box_hint(keys: &[&str]) -> Line<'static> {
    Line::from(Span::styled(
        keys.join(SEP),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    ))
}

/// The Yes/No row a gate and an offer share. The labels carry their keys, so
/// the hint line does not have to teach them twice, and the picked one is
/// filled with the border colour rather than merely reversed: a reversed
/// button reads as "selected", a filled one reads as "this is what Enter does".
pub fn box_buttons(colour: Color, yes: bool) -> Line<'static> {
    let button = |label: &str, picked: bool| {
        let style = if picked {
            Style::default()
                .fg(Color::Black)
                .bg(colour)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        };
        Span::styled(format!(" {label} "), style)
    };
    Line::from(vec![
        button("Yes (y)", yes),
        Span::raw("  "),
        button("No (n)", !yes),
    ])
}

/// A Yes/No box: a gate (red, opening on No, since a reflex Enter must never be
/// the key that fires an irreversible thing) or an offer (cyan, opening on
/// Yes, with nothing at stake). Which it is comes from `colour` and the `yes`
/// the caller opens it with. `note` is what else to know before answering.
pub fn confirm_popup(
    frame: &mut Frame,
    area: Rect,
    colour: Color,
    title: &str,
    name: &str,
    note: Option<&str>,
    yes: bool,
) {
    let width = box_width(area.width);
    let mut lines = vec![Line::from(Span::styled(
        name.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))];
    for note in note.into_iter().flat_map(str::lines) {
        lines.push(Line::from(Span::styled(
            note.to_string(),
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    lines.push(Line::from(""));
    lines.push(box_buttons(colour, yes));
    lines.push(Line::from(""));
    lines.push(box_hint(GATE_KEYS));
    draw_box(frame, area, width, lines, colour, title);
}

/// Draw `lines` in a box `width` wide, centered in `area`, as tall as they are
/// once wrapped: a long name or note wraps, and a box of fixed height would put
/// the key row past its own bottom border.
fn draw_box(
    frame: &mut Frame,
    area: Rect,
    width: u16,
    lines: Vec<Line<'static>>,
    colour: Color,
    title: &str,
) {
    let inner = box_inner_width(width);
    let rows: usize = lines
        .iter()
        .map(|l| {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            wrapped_rows(&text, inner)
        })
        .sum();
    let rect = popup_area(area, width, box_height(rows as u16, area.height));
    frame.render_widget(Clear, rect);
    let body = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(box_block(colour, title));
    frame.render_widget(body, rect);
}

/// The typed gate: red, no Yes/No, and nothing happens on Enter until `typed`
/// is exactly `name`, which the box shows so it is copied rather than guessed.
/// `verb` is what Enter does once the name matches (`del`, `drop`).
pub fn typed_popup(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    note: Option<&str>,
    name: &str,
    typed: &Query,
    verb: &str,
) {
    let width = box_width(area.width);
    let mut lines: Vec<Line<'static>> = note
        .into_iter()
        .flat_map(str::lines)
        .map(|l| Line::from(l.to_string()))
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("type "),
        Span::styled(
            name.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" to confirm:"),
    ]));
    // Armed or not is shown on the thing Enter does, not only on the text: plain
    // text and a dim `↵ <verb>` until the name matches, then bold green text
    // and `↵ <verb>` filled red, the way a picked gate button is.
    let armed = typed.text == name;
    let field = if armed {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let (before, after) = typed.split();
    lines.push(Line::from(Span::styled(format!("{before}▏{after}"), field)));
    lines.push(Line::from(""));
    let dim = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    let enter = Span::styled(
        format!("↵ {verb}"),
        if armed {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD)
        } else {
            dim
        },
    );
    lines.push(Line::from(vec![
        enter,
        Span::styled(format!("{SEP}{CANCEL}"), dim),
    ]));
    draw_box(frame, area, width, lines, Color::Red, title);
}

// ── form ────────────────────────────────────────────────────────────────────

/// One typed field of a form.
pub struct Field {
    /// A short noun, which the form follows with its colon.
    pub label: &'static str,
    pub value: Query,
    /// Submitting refuses a blank, and the label wears a red `*` for it.
    pub required: bool,
    /// What the field takes, dim in the value column until something is typed.
    pub placeholder: &'static str,
    /// For a pick rather than a typed value, what it picks from: `←→` or
    /// `space` move through them, and the value is the one showing.
    pub choices: &'static [&'static str],
}

impl Field {
    pub fn new(label: &'static str, placeholder: &'static str, required: bool) -> Field {
        Field {
            label,
            value: Query::default(),
            required,
            placeholder,
            choices: &[],
        }
    }

    /// A pick from `choices`, on the first until moved.
    pub fn pick(label: &'static str, choices: &'static [&'static str]) -> Field {
        Field {
            choices,
            ..Field::new(label, "", false).with(choices.first().copied().unwrap_or(""))
        }
    }

    /// Show the next or the previous choice.
    fn cycle(&mut self, forward: bool) {
        let n = self.choices.len();
        if n == 0 {
            return;
        }
        let at = self
            .choices
            .iter()
            .position(|c| *c == self.value.text)
            .unwrap_or(0);
        let to = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        };
        self.value.text = self.choices[to].to_string();
    }

    /// The same field holding `text`, caret at its end.
    pub fn with(mut self, text: &str) -> Field {
        self.value.text = text.to_string();
        self.value.caret = text.chars().count();
        self
    }
}

/// What a key did to an open form.
pub enum FormKey {
    Open,
    Cancel,
    Submit,
}

/// A cyan box of typed fields: labels in one column, values in the next. Enter
/// moves to the next field and submits on the last, `tab`/`ctrl-j` and
/// `shift-tab`/`ctrl-k` move between fields, and every field edits like a shell
/// line through `Query::on_key`.
pub struct Form {
    pub title: String,
    pub fields: Vec<Field>,
    pub at: usize,
}

/// Where a form's values start, and the furthest a long label may push them.
const FORM_VALUES: usize = 14;
const FORM_VALUES_MAX: usize = 22;

impl Form {
    pub fn new(title: impl Into<String>, fields: Vec<Field>) -> Form {
        Form {
            title: title.into(),
            fields,
            at: 0,
        }
    }

    /// A field's value with the spaces around it dropped.
    pub fn value(&self, i: usize) -> &str {
        self.fields.get(i).map_or("", |f| f.value.text.trim())
    }

    /// `code` is the key after the caller's own folding (Ctrl-C and `ctrl-[`
    /// as Esc); `key` is what a field is typed with.
    pub fn on_key(&mut self, code: KeyCode, key: KeyEvent) -> FormKey {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = self.fields.len().saturating_sub(1);
        match code {
            KeyCode::Esc => return FormKey::Cancel,
            KeyCode::Enter => {
                // A blank the submit would refuse is where the cursor goes,
                // rather than past it.
                let blank = self
                    .fields
                    .iter()
                    .position(|f| f.required && f.value.text.trim().is_empty());
                match blank {
                    Some(i) if i <= self.at || self.at == last => self.at = i,
                    _ if self.at < last => self.at += 1,
                    _ => return FormKey::Submit,
                }
            }
            KeyCode::Tab => self.at = if self.at == last { 0 } else { self.at + 1 },
            KeyCode::BackTab => self.at = if self.at == 0 { last } else { self.at - 1 },
            KeyCode::Down => self.at = (self.at + 1).min(last),
            KeyCode::Up => self.at = self.at.saturating_sub(1),
            KeyCode::Char('j') if ctrl => self.at = (self.at + 1).min(last),
            KeyCode::Char('k') if ctrl => self.at = self.at.saturating_sub(1),
            _ => {
                if let Some(field) = self.fields.get_mut(self.at) {
                    if field.choices.is_empty() {
                        field.value.on_key(key);
                    } else if matches!(code, KeyCode::Left | KeyCode::Char('h')) {
                        field.cycle(false);
                    } else if matches!(code, KeyCode::Right | KeyCode::Char('l' | ' ')) {
                        field.cycle(true);
                    }
                }
            }
        }
        FormKey::Open
    }

    /// Draw it centered in `area`, as tall as its fields once wrapped.
    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let width = box_width(area.width);
        let longest = self
            .fields
            .iter()
            .map(|f| f.label.chars().count() + 3)
            .max()
            .unwrap_or(0);
        let col = longest.clamp(FORM_VALUES, FORM_VALUES_MAX);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut lines = Vec::new();
        for (i, f) in self.fields.iter().enumerate() {
            let focused = i == self.at;
            let label = if focused { Style::default() } else { dim };
            let mut spans = vec![Span::styled(f.label.to_string(), label)];
            let mut used = f.label.chars().count() + 1;
            if f.required {
                spans.push(Span::styled("*", Style::default().fg(Color::Red)));
                used += 1;
            }
            spans.push(Span::styled(":", label));
            spans.push(Span::raw(" ".repeat(col.saturating_sub(used).max(1))));
            let placeholder = Span::styled(
                f.placeholder.to_string(),
                Style::default().fg(Color::DarkGray),
            );
            if !f.choices.is_empty() {
                let marks = if focused { Style::default() } else { dim };
                spans.extend([
                    Span::styled("‹ ", marks.fg(Color::DarkGray)),
                    Span::styled(f.value.text.clone(), label),
                    Span::styled(" ›", marks.fg(Color::DarkGray)),
                ]);
                if !f.placeholder.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", f.placeholder),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                lines.push(Line::from(spans));
                continue;
            }
            match (focused, f.value.text.is_empty()) {
                (true, true) => spans.extend([Span::raw("▏"), placeholder]),
                (true, false) => {
                    let (before, after) = f.value.split();
                    spans.push(Span::raw(format!("{before}▏{after}")));
                }
                (false, true) => spans.push(placeholder),
                (false, false) => spans.push(Span::styled(f.value.text.clone(), dim)),
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(""));
        let enter = if self.at + 1 == self.fields.len() {
            "↵ submit"
        } else {
            "↵ next"
        };
        let mut keys = vec![enter, CANCEL];
        if self.fields.iter().any(|f| f.required) {
            keys.push("* required");
        }
        lines.push(box_hint(&keys));
        draw_box(frame, area, width, lines, Color::Cyan, &self.title);
    }
}

/// Rows `text` takes once word-wrapped to `width` columns, the way ratatui's
/// `Wrap` breaks it: each line on its own, a word that does not fit starting a
/// new row, and a word longer than the row breaking across several.
pub fn wrapped_height(text: &str, width: usize) -> usize {
    text.lines().map(|l| wrapped_rows(l, width)).sum()
}

/// `wrapped_height` for one line with no breaks in it.
fn wrapped_rows(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let mut rows = 1usize;
    let mut col = 0usize;
    // Single spaces, not runs of whitespace: an indent is real columns.
    for (i, word) in text.split(' ').enumerate() {
        let w = word.chars().count();
        let need = if i == 0 { w } else { col + 1 + w };
        if need <= width {
            col = need;
        } else {
            rows += 1;
            col = w;
        }
        while col > width {
            rows += 1;
            col -= width;
        }
    }
    rows
}

/// A box of at most `w`×`h`, centered in `area`.
pub fn popup_area(area: Rect, w: u16, h: u16) -> Rect {
    let (w, h) = (w.min(area.width), h.min(area.height));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::{Modal, Query};

    #[test]
    fn a_box_never_hands_a_tab_or_control_character_to_the_terminal() {
        // A box shows git's own words, which indent file names with a tab. A
        // tab reaches the terminal as a cursor jump ratatui does not know
        // about, and every cell after it on the row was drawn out of place.
        let said = "error: overwritten by merge:\n\tdocs/notes.md\nbell\x07 and\r return";
        let bodies = [
            Modal::new("t", said).body,
            Modal::reader("t", &[("k".to_string(), said.to_string())]).body,
            Modal::reader("t", &[]).with_text(said).body,
        ];
        for body in bodies {
            assert!(
                !body.chars().any(|c| c != '\n' && c.is_control()),
                "a control character reached the box: {body:?}"
            );
            assert!(
                body.contains("docs/notes.md"),
                "the words around it survive: {body:?}"
            );
        }
    }

    fn query(text: &str) -> Query {
        Query {
            text: text.to_string(),
            caret: 0,
        }
    }

    #[test]
    fn a_filter_keeps_only_rows_every_term_is_in() {
        // `/` and `ctrl-f` split on whitespace and require all of them, which is what
        // lets `pablo fix` mean both words rather than either.
        assert!(
            query("").keeps("anything at all"),
            "an empty filter keeps everything"
        );

        let q = query("pablo fix");
        assert!(q.keeps("a1b2c3 2026-09-03 pablo fix: the thing"));
        assert!(!q.keeps("a1b2c3 2026-09-03 pablo feat: the thing"));
        assert!(!q.keeps("a1b2c3 2026-09-03 marta fix: the thing"));
    }

    #[test]
    fn a_filter_ignores_case_on_both_sides() {
        assert!(query("FIX Pablo").keeps("pablo fix: lowercase row"));
        assert!(query("fix").keeps("PABLO FIX: UPPERCASE ROW"));
    }

    #[test]
    fn a_filter_of_only_spaces_is_no_filter() {
        // Typing a space and deleting the word must not leave a query that
        // matches nothing at all.
        assert!(query("   ").keeps("anything"));
    }
}
