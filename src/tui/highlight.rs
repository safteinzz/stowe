//! Side-by-side diff rendering, syntax-highlighted with syntect (pure Rust, no
//! oniguruma).
//!
//! Highlighting is the expensive part, so it happens once in `prepare_diff`;
//! `render_prepared` then re-lays-out the already-highlighted spans on every
//! scroll or pan for free.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SynStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// Tint for changed cells (left = removed, right = added).
const REMOVED_BG: Color = Color::Rgb(55, 24, 24);
const ADDED_BG: Color = Color::Rgb(24, 46, 24);

/// Longest content line in a raw diff (minus its +/-/space prefix), for
/// clamping horizontal scroll.
fn max_line_width(raw: &str) -> usize {
    raw.lines()
        .map(|l| l.chars().count().saturating_sub(1))
        .max()
        .unwrap_or(0)
}

/// Columns between tab stops once a tab is expanded.
const TAB_WIDTH: usize = 4;

/// `raw` with tabs expanded and every other control character in caret notation
/// (`^M`), a trailing CR dropped. ratatui hands them to the terminal as they
/// are, where a tab or a lone CR moves the cursor and leaves cells behind that
/// the next frame believes are blank.
fn printable(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for line in raw.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // tab stops count from after the diff's own marker column
        let mut col = 0usize;
        for (i, c) in line.chars().enumerate() {
            let before = out.len();
            match c {
                '\t' => {
                    let pad = if i == 0 {
                        1
                    } else {
                        TAB_WIDTH - col % TAB_WIDTH
                    };
                    out.extend(std::iter::repeat_n(' ', pad));
                }
                c if c.is_ascii_control() => {
                    out.push('^');
                    out.push(char::from(c as u8 ^ 0x40));
                }
                c if c.is_control() => out.push(char::REPLACEMENT_CHARACTER),
                c => out.push(c),
            }
            if i > 0 {
                col += out[before..].chars().count();
            }
        }
        out.push('\n');
    }
    out
}

// ── side-by-side diff rendering ─────────────────────────────────────────────

/// The whole text of one side of the file a diff is about, read only when a
/// hunk turns out to need it.
pub type Blob = Box<dyn FnOnce() -> Option<String> + Send>;

/// Where to read that text, per side. A hunk starting at line 375 hands syntect
/// a closing `"""` it never saw opened, which it reads as an opening one and
/// stains every line below; the cure is the lines above the hunk, and only the
/// caller knows which revisions they live in. It describes one file, so it is
/// spent on the first `diff --git` of the text - the only shape the interactive
/// views ever show.
#[derive(Default)]
pub struct DiffContext {
    pub old: Option<Blob>,
    pub new: Option<Blob>,
}

/// Past this many bytes a file is not primed: parsing a megabyte to find the
/// line that opened a docstring costs more than the stain does.
const MAX_PRIME: usize = 1 << 20;

/// One side's highlighter, plus the file text that catches it up when a hunk
/// starts past the line it has reached.
struct Side {
    hl: HighlightLines<'static>,
    blob: Option<Blob>,
    lines: Option<Vec<String>>,
    /// File lines this highlighter has consumed, primed or rendered.
    fed: usize,
}

impl Side {
    fn new(syntax: &'static SyntaxReference, blob: Option<Blob>) -> Self {
        Side {
            hl: HighlightLines::new(syntax, theme()),
            blob,
            lines: None,
            fed: 0,
        }
    }

    fn line(&mut self, ps: &SyntaxSet, code: &str) -> Vec<Span<'static>> {
        self.fed += 1;
        hl_spans(&mut self.hl, ps, code)
    }

    /// Push the file's own lines through the highlighter up to `start`, throwing
    /// the spans away: what a hunk wants from the lines above it is syntect's
    /// state, not their colours. One fetch per side per file, and none at all
    /// while the hunks follow on from each other.
    fn prime(&mut self, ps: &SyntaxSet, start: usize) {
        if start <= self.fed + 1 {
            return;
        }
        if self.lines.is_none() {
            let Some(blob) = self.blob.take() else {
                return;
            };
            let Some(text) = blob().filter(|t| t.len() <= MAX_PRIME) else {
                return;
            };
            self.lines = Some(text.lines().map(str::to_string).collect());
        }
        let Side { hl, lines, fed, .. } = self;
        let Some(lines) = lines.as_ref() else {
            return;
        };
        let upto = (start - 1).min(lines.len());
        if upto <= *fed {
            return;
        }
        for line in &lines[*fed..upto] {
            let _ = hl.highlight_line(&format!("{line}\n"), ps);
        }
        *fed = upto;
    }
}

/// Two highlighters per file - one for the old side, one for the new - so a
/// removed line's syntax state never corrupts the added side and vice versa.
struct FileHl {
    old: Side,
    new: Side,
}

/// One accumulated changed line: its line number and highlighted spans.
type NumLine = (usize, Vec<Span<'static>>);

/// One row of a parsed diff: either a full-width header line, or a side-by-side
/// pair whose cells are already syntax-highlighted (so re-laying out for a
/// scroll is cheap - no re-highlighting).
enum DiffRow {
    Header(Line<'static>),
    Pair {
        lnum: Option<usize>,
        lspans: Vec<Span<'static>>,
        lbg: Option<Color>,
        rnum: Option<usize>,
        rspans: Vec<Span<'static>>,
        rbg: Option<Color>,
    },
}

/// A diff parsed and highlighted once. Cheap to re-render at any width/hscroll.
#[derive(Default)]
pub struct RenderedDiff {
    rows: Vec<DiffRow>,
    gutter_w: usize,
    max_line: usize,
}

impl RenderedDiff {
    /// Longest content line, for clamping horizontal scroll.
    pub fn max_line(&self) -> usize {
        self.max_line
    }

    /// Visible content columns per side at total pane `width`. Mirrors the layout
    /// math in `render_prepared` (gutters + " │ " separator eat into each side),
    /// so horizontal-scroll clamping matches what's actually drawn - otherwise the
    /// tail of a medium-length line stays hidden because panning stops short.
    pub fn cell_width(&self, width: u16) -> u16 {
        let avail = (width as usize).saturating_sub(2 * self.gutter_w + 5);
        (avail / 2).max(1) as u16
    }
}

/// Parse `git show` output and syntax-highlight every line **once**. This is
/// the expensive step (syntect); call it when a file is opened, then re-render
/// with `render_prepared` for free on every scroll.
pub fn prepare_diff(raw: &str, mut ctx: DiffContext) -> RenderedDiff {
    let ps = syntaxes();
    let raw = &printable(raw);

    let mut rows: Vec<DiffRow> = Vec::new();
    let mut hl: Option<FileHl> = None;
    let mut in_patch = false;
    let mut old_ln = 0usize;
    let mut new_ln = 0usize;
    let mut rem: Vec<NumLine> = Vec::new();
    let mut add: Vec<NumLine> = Vec::new();

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush_pairs(&mut rows, &mut rem, &mut add);
            in_patch = true;
            let syntax = syntax_for(ps, rest.rsplit(" b/").next().unwrap_or(""));
            // Plain text has no state to get wrong, so it never pays for a blob.
            let (old, new) = if std::ptr::eq(syntax, ps.find_syntax_plain_text()) {
                (None, None)
            } else {
                (ctx.old.take(), ctx.new.take())
            };
            hl = Some(FileHl {
                old: Side::new(syntax, old),
                new: Side::new(syntax, new),
            });
            rows.push(DiffRow::Header(plain(
                line,
                dim().add_modifier(Modifier::BOLD),
            )));
            continue;
        }

        if !in_patch {
            if line.is_empty() {
                continue;
            }
            rows.push(DiffRow::Header(plain(line, Style::default())));
            continue;
        }

        if line.starts_with("@@") {
            flush_pairs(&mut rows, &mut rem, &mut add);
            if let Some((a, _, c, _)) = parse_hunk(line) {
                old_ln = a;
                new_ln = c;
                if let Some(h) = hl.as_mut() {
                    h.old.prime(ps, a);
                    h.new.prime(ps, c);
                }
            }
            rows.push(DiffRow::Header(plain(
                line,
                Style::default().fg(Color::Cyan),
            )));
            continue;
        }
        if is_meta(line) {
            flush_pairs(&mut rows, &mut rem, &mut add);
            rows.push(DiffRow::Header(plain(line, dim())));
            continue;
        }

        if let Some(code) = line.strip_prefix('-') {
            let spans = hl_old(&mut hl, ps, code);
            rem.push((old_ln, spans));
            old_ln += 1;
        } else if let Some(code) = line.strip_prefix('+') {
            let spans = hl_new(&mut hl, ps, code);
            add.push((new_ln, spans));
            new_ln += 1;
        } else if let Some(code) = line.strip_prefix(' ') {
            flush_pairs(&mut rows, &mut rem, &mut add);
            let l = hl_old(&mut hl, ps, code);
            let r = hl_new(&mut hl, ps, code);
            rows.push(DiffRow::Pair {
                lnum: Some(old_ln),
                lspans: l,
                lbg: None,
                rnum: Some(new_ln),
                rspans: r,
                rbg: None,
            });
            old_ln += 1;
            new_ln += 1;
        } else {
            flush_pairs(&mut rows, &mut rem, &mut add);
            rows.push(DiffRow::Header(plain(line, dim())));
        }
    }
    flush_pairs(&mut rows, &mut rem, &mut add);

    RenderedDiff {
        rows,
        gutter_w: gutter_width(raw),
        max_line: max_line_width(raw),
    }
}

/// Pair up accumulated removed/added lines into side-by-side `Pair` rows.
fn flush_pairs(rows: &mut Vec<DiffRow>, rem: &mut Vec<NumLine>, add: &mut Vec<NumLine>) {
    let n = rem.len().max(add.len());
    for i in 0..n {
        let (lnum, lspans, lbg) = match rem.get(i) {
            Some((num, s)) => (Some(*num), s.clone(), Some(REMOVED_BG)),
            None => (None, Vec::new(), None),
        };
        let (rnum, rspans, rbg) = match add.get(i) {
            Some((num, s)) => (Some(*num), s.clone(), Some(ADDED_BG)),
            None => (None, Vec::new(), None),
        };
        rows.push(DiffRow::Pair {
            lnum,
            lspans,
            lbg,
            rnum,
            rspans,
            rbg,
        });
    }
    rem.clear();
    add.clear();
}

/// Lay out a prepared diff at `width`, panned right by `hscroll` columns. Cheap:
/// just slices the already-highlighted spans into columns - no highlighting.
pub fn render_prepared(d: &RenderedDiff, width: u16, hscroll: u16) -> Text<'static> {
    let g = d.gutter_w;
    let inner = width as usize;
    // layout: [num g][ ][left lw] " │ " [num g][ ][right rw]
    let avail = inner.saturating_sub(2 * g + 5);
    let lw = (avail / 2).max(1);
    let rw = avail.saturating_sub(lw).max(1);
    let hs = hscroll as usize;

    let lines: Vec<Line<'static>> = d
        .rows
        .iter()
        .map(|row| match row {
            DiffRow::Header(l) => l.clone(),
            DiffRow::Pair {
                lnum,
                lspans,
                lbg,
                rnum,
                rspans,
                rbg,
            } => {
                let mut spans = gutter(*lnum, g, *lbg);
                spans.extend(fit_cell(lspans.clone(), lw, *lbg, hs));
                spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
                spans.extend(gutter(*rnum, g, *rbg));
                spans.extend(fit_cell(rspans.clone(), rw, *rbg, hs));
                Line::from(spans)
            }
        })
        .collect();

    Text::from(lines)
}

/// A right-aligned line-number gutter `g` wide plus a trailing space.
fn gutter(num: Option<usize>, g: usize, bg: Option<Color>) -> Vec<Span<'static>> {
    let text = match num {
        Some(n) => format!("{n:>g$} "),
        None => " ".repeat(g + 1),
    };
    let mut style = Style::default().fg(Color::DarkGray);
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    vec![Span::styled(text, style)]
}

/// Column width for the line-number gutter: digits of the largest line number
/// any hunk reaches, at least 3.
fn gutter_width(raw: &str) -> usize {
    let mut max = 1usize;
    for line in raw.lines() {
        if line.starts_with("@@")
            && let Some((a, b, c, d)) = parse_hunk(line)
        {
            max = max.max(a + b).max(c + d);
        }
    }
    digits(max).max(3)
}

fn digits(mut n: usize) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// Parse a hunk header `@@ -a,b +c,d @@` into `(a, b, c, d)`. The counts `b`/`d`
/// default to 1 when omitted (`@@ -a +c @@`).
fn parse_hunk(line: &str) -> Option<(usize, usize, usize, usize)> {
    let body = line.strip_prefix("@@ ")?.split(" @@").next()?; // "-a,b +c,d"
    let mut parts = body.split(' ');
    let (a, b) = parse_pair(parts.next()?.strip_prefix('-')?)?;
    let (c, d) = parse_pair(parts.next()?.strip_prefix('+')?)?;
    Some((a, b, c, d))
}

fn parse_pair(s: &str) -> Option<(usize, usize)> {
    let mut it = s.split(',');
    let start = it.next()?.parse().ok()?;
    let count = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
    Some((start, count))
}

/// Slide a cell left by `skip` display columns (horizontal scroll), then
/// truncate/pad to exactly `width` columns, applying an optional background to
/// the whole cell (including the padding).
fn fit_cell(
    spans: Vec<Span<'static>>,
    width: usize,
    bg: Option<Color>,
    skip: usize,
) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut skipped = 0usize; // columns dropped off the left so far
    let mut used = 0usize; // columns emitted into the cell
    for sp in spans {
        if used >= width {
            break;
        }
        let chars: Vec<char> = sp.content.chars().collect();
        let mut start = 0usize;
        if skipped < skip {
            let drop = (skip - skipped).min(chars.len());
            start = drop;
            skipped += drop;
            if start >= chars.len() {
                continue; // whole span scrolled off the left edge
            }
        }
        let take = (chars.len() - start).min(width - used);
        let text: String = chars[start..start + take].iter().collect();
        let mut style = sp.style;
        if let Some(bg) = bg {
            style = style.bg(bg);
        }
        out.push(Span::styled(text, style));
        used += take;
    }
    if used < width {
        let mut style = Style::default();
        if let Some(bg) = bg {
            style = style.bg(bg);
        }
        out.push(Span::styled(" ".repeat(width - used), style));
    }
    out
}

fn hl_old(hl: &mut Option<FileHl>, ps: &SyntaxSet, code: &str) -> Vec<Span<'static>> {
    match hl {
        Some(h) => h.old.line(ps, code),
        None => vec![Span::raw(code.to_string())],
    }
}

fn hl_new(hl: &mut Option<FileHl>, ps: &SyntaxSet, code: &str) -> Vec<Span<'static>> {
    match hl {
        Some(h) => h.new.line(ps, code),
        None => vec![Span::raw(code.to_string())],
    }
}

fn hl_spans(h: &mut HighlightLines, ps: &SyntaxSet, code: &str) -> Vec<Span<'static>> {
    let with_nl = format!("{code}\n");
    match h.highlight_line(&with_nl, ps) {
        Ok(ranges) => ranges
            .into_iter()
            .filter_map(|(sty, text)| {
                let text = text.trim_end_matches('\n');
                (!text.is_empty()).then(|| Span::styled(text.to_string(), syn_to_ratatui(sty)))
            })
            .collect(),
        Err(_) => vec![Span::raw(code.to_string())],
    }
}

fn is_meta(line: &str) -> bool {
    line.starts_with("index ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("new file")
        || line.starts_with("deleted file")
        || line.starts_with("similarity ")
        || line.starts_with("rename ")
        || line.starts_with("\\ No newline")
}

fn syntax_for<'a>(ps: &'a SyntaxSet, path: &str) -> &'a SyntaxReference {
    path.rsplit('.')
        .next()
        .filter(|ext| *ext != path)
        .and_then(|ext| ps.find_syntax_by_extension(ext))
        .unwrap_or_else(|| ps.find_syntax_plain_text())
}

fn plain(text: &str, style: Style) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), style))
}

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn syn_to_ratatui(s: SynStyle) -> Style {
    let fg = s.foreground;
    let mut style = Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b));
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let mut ts = ThemeSet::load_defaults();
        ts.themes
            .remove("base16-eighties.dark")
            .or_else(|| ts.themes.remove("base16-ocean.dark"))
            .unwrap_or_else(|| ts.themes.values().next().cloned().unwrap_or_default())
    })
}
