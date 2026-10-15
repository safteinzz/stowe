//! Rendering. Two panes on every level, the current list on top and what its
//! selected row holds underneath, the scan's bar above both, and the footer.

use super::load::{Reach, Side, StatusRow};
use super::patch;
use super::{App, Level, Pane, Sel, commits, repos, status};
use crate::diff::common;
use crate::history::{Change, Kind};
use crate::mirror::Format;
use crate::model::short;
use crate::names::display;
use crate::repos::Behind;
use crate::time::short_time;
use crate::tui::highlight::render_prepared;
use crate::tui::input::{CTRL_X_MOVE, CTRL_Y_MOVE, X_MOVE, Y_MOVE};
use crate::tui::widgets::{
    BACK, CANCEL, CREATE, DEL, EDIT, FILTER_KEYS, FIND, HELP, HelpSection, Modal, QUIT, REFRESH,
    SEP, confirm_popup, diff_hscrollbar, diff_scrollbar, key_footer, list_scrollbar, pane_block,
    scope_tabs, typed_popup,
};
use crate::tui::{clamp_hscroll, clamp_scroll};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{List, ListItem, Paragraph};

/// Width a repo's or a remote's name is padded to, at most.
const NAME_W: usize = 28;
/// Cap on a remote's URL column.
const URL_W: usize = 40;

pub(super) fn draw(frame: &mut Frame, app: &mut App) {
    let [panes, footer_row] =
        Layout::vertical([Constraint::Min(2), Constraint::Length(1)]).areas(frame.area());
    let lists = if app.level == Level::Scan {
        let [bar, rest] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(2)]).areas(panes);
        draw_bar(frame, bar, app);
        rest
    } else {
        panes
    };
    let [top, bottom] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(lists);
    let (edit_top, edit_bot) = (
        app.editing == Some(Pane::Top),
        app.editing == Some(Pane::Bottom),
    );

    match app.level {
        Level::Repos => {
            let labels: Vec<&str> = repos::SCOPES.iter().map(|s| s.label()).collect();
            let t = title(
                "repos",
                Some((&labels, app.rsel.scope)),
                &app.rsel,
                app.rfeed.slow(),
                Pane::Top,
                edit_top,
            );
            let items = repo_items(app);
            list(frame, top, items, t, &mut app.rsel);
            let name = app.rsel.idx().map_or("commits".to_string(), |i| {
                format!("{} commits", display(&app.repos[i].name))
            });
            draw_log(frame, bottom, app, &name, Pane::Bottom, edit_bot, false);
        }
        Level::Remotes => {
            let t = title("remotes", None, &app.msel, false, Pane::Top, edit_top);
            let items = remote_items(app);
            list(frame, top, items, t, &mut app.msel);
            let name = match app.remote() {
                Some(r) => format!("a push to {}{}", r.name, counts(&app.files)),
                None => "a push".to_string(),
            };
            let t = title(
                &name,
                None,
                &app.fsel,
                app.pfeed.slow(),
                Pane::Bottom,
                edit_bot,
            );
            let items = match &app.plan_failed {
                Some(e) => vec![dim_row(e)],
                None if app.fsel.is_empty() && !app.pfeed.loading && app.remote().is_some() => {
                    vec![dim_row("nothing to send: it has HEAD")]
                }
                None => change_items(app, true),
            };
            list(frame, bottom, items, t, &mut app.fsel);
        }
        Level::Scan => {
            let name = "hits";
            let t = title(name, None, &app.xsel, app.cfeed.slow(), Pane::Top, false);
            let items = hit_items(app);
            list(frame, top, items, t, &mut app.xsel);
            let name = app.hit().map_or("changes".to_string(), |h| {
                format!("{} changes", short(&app.log[h.at].hash))
            });
            let t = title(&name, None, &app.fsel, false, Pane::Bottom, edit_bot);
            let items = change_items(app, true);
            list(frame, bottom, items, t, &mut app.fsel);
        }
        Level::Status => {
            let labels: Vec<&str> = status::SCOPES.iter().map(|s| s.label()).collect();
            let t = title(
                "status",
                Some((&labels, app.ssel.scope)),
                &app.ssel,
                app.sfeed.slow(),
                Pane::Top,
                edit_top,
            );
            let items = status_items(app);
            list(frame, top, items, t, &mut app.ssel);
            draw_patch(frame, bottom, app);
        }
        Level::Commits => {
            draw_log(frame, top, app, "commits", Pane::Top, edit_top, true);
            let t = title("changes", None, &app.fsel, false, Pane::Bottom, edit_bot);
            let items = change_items(app, true);
            list(frame, bottom, items, t, &mut app.fsel);
        }
        Level::Files => {
            let name = app.commit().map_or("changes".to_string(), |c| {
                format!("{} changes", short(&c.hash))
            });
            let t = title(&name, None, &app.fsel, false, Pane::Top, edit_top);
            let items = change_items(app, true);
            list(frame, top, items, t, &mut app.fsel);
            draw_versions(frame, bottom, app, edit_bot);
        }
    }

    draw_footer(frame, footer_row, app);

    if let Some(c) = &app.confirm
        && c.target.typed()
    {
        typed_popup(
            frame,
            panes,
            &c.target.title(),
            Some(&c.target.note()),
            &c.target.name(),
            &c.typed,
            c.target.verb(),
        );
    } else if let Some(c) = &app.confirm {
        confirm_popup(
            frame,
            panes,
            c.target.colour(),
            &c.target.title(),
            &c.target.name(),
            Some(&c.target.note()),
            c.yes,
        );
    }
    if let Some((form, _)) = &app.form {
        form.draw(frame, panes);
    }
    if let Some(modal) = &mut app.modal {
        modal.draw(frame, panes);
    }
}

/// The scan's bar: what was typed, or what to type.
fn draw_bar(frame: &mut Frame, area: Rect, app: &App) {
    let typing = app.typing;
    let hint = if typing {
        " a file name, old or new: every word must be in it "
    } else {
        " a file name, old or new: every word must be in it   / find "
    };
    let line = if typing {
        let (before, after) = app.bar.split();
        Line::from(format!(" {before}▏{after}"))
    } else {
        Line::from(format!(" {}", app.searched))
    };
    frame.render_widget(Paragraph::new(line).block(pane_block(hint, typing)), area);
}

/// The history as a list, with its tabs when it is the pane plain keys drive.
fn draw_log(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    name: &str,
    pane: Pane,
    editing: bool,
    tabs: bool,
) {
    let labels: Vec<&str> = commits::SCOPES.iter().map(|s| s.label()).collect();
    let scope = tabs.then_some((labels.as_slice(), app.csel.scope));
    let t = title(name, scope, &app.csel, app.cfeed.slow(), pane, editing);
    let items = match &app.log_failed {
        Some(e) if app.csel.is_empty() => vec![dim_row(e)],
        _ => commit_items(app),
    };
    list(frame, area, items, t, &mut app.csel);
}

/// One file's versions, the commit the top pane is in marked.
fn draw_versions(frame: &mut Frame, area: Rect, app: &mut App, editing: bool) {
    let leaf = app.hist_for.0.rsplit('/').next().unwrap_or("").to_string();
    let name = if leaf.is_empty() {
        "versions".to_string()
    } else {
        format!("versions of {}", display(&leaf))
    };
    let t = title(
        &name,
        None,
        &app.hsel,
        app.cfeed.slow(),
        Pane::Bottom,
        editing,
    );
    let items = version_items(app);
    list(frame, area, items, t, &mut app.hsel);
}

/// The selected status row's diff, side by side, scrolled
/// and panned with Ctrl.
fn draw_patch(frame: &mut Frame, area: Rect, app: &mut App) {
    let title = match app.status_row() {
        Some(r) => format!(" {} {} ", display(&r.change.shown()), patch::tag(r.side)),
        None => " (nothing to show) ".to_string(),
    };
    let inner = area.width.saturating_sub(2);
    let cell = app.prepared.cell_width(inner);
    app.dhscroll = clamp_hscroll(app.dhscroll, app.prepared.max_line(), cell);
    let diff = render_prepared(&app.prepared, inner, app.dhscroll);
    app.dscroll = clamp_scroll(app.dscroll, diff.lines.len(), area.height.saturating_sub(2));
    // An empty diff and one still being prepared look the same, so say which.
    let body = if diff.lines.is_empty() && app.dfeed.slow() {
        Text::from(Line::from(Span::styled(
            "  loading…",
            Style::default().add_modifier(Modifier::DIM),
        )))
    } else {
        diff
    };
    let total = body.lines.len();
    let widget = Paragraph::new(body)
        .block(pane_block(title, true))
        .scroll((app.dscroll, 0));
    frame.render_widget(widget, area);
    diff_scrollbar(frame, area, total, app.dscroll);
    diff_hscrollbar(frame, area, app.prepared.max_line(), cell, app.dhscroll);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    if app.note.is_none()
        && app.command.is_none()
        && app.editing.is_none()
        && let Some(job) = &app.job
    {
        let said = app.progress.lock().map(|p| p.clone()).unwrap_or_default();
        let mut text = format!(" ⋯ {}…", display(&job.doing()));
        if !said.is_empty() {
            text.push_str(&format!("{SEP}{}", display(&said)));
        }
        // `? help` stays pinned right, as on every footer, since a job can run
        // for minutes.
        let pin = format!("{HELP} ");
        let room = (area.width as usize).saturating_sub(pin.chars().count() + SEP.chars().count());
        let text = truncate(&text, room);
        let gap = (area.width as usize).saturating_sub(text.chars().count() + pin.chars().count());
        let line = Line::from(vec![
            Span::raw(text),
            Span::raw(" ".repeat(gap)),
            Span::styled(pin, Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    let keys = if app.editing.is_some() {
        FILTER_KEYS.to_vec()
    } else if app.level == Level::Scan && app.typing {
        // Before the first run the bar is all there is, so cancelling it ends
        // the scan.
        vec![
            "↵ run",
            if app.searched.is_empty() {
                CANCEL
            } else {
                BACK
            },
        ]
    } else {
        actions(app)
    };
    frame.render_widget(
        key_footer(
            &keys,
            app.note.as_ref(),
            app.command.as_ref(),
            Some(HELP),
            area.width,
        ),
        area,
    );
}

/// What a level can do, in the house order: `↵`, its own keys, `K`, then
/// `c create · e edit · d del`, then `/ find · r refresh · q quit`.
fn actions(app: &App) -> Vec<&'static str> {
    let mut keys = Vec::new();
    match app.level {
        Level::Repos | Level::Commits | Level::Scan => keys.push("↵ open"),
        Level::Files => keys.push("↵ restore"),
        Level::Status => keys.extend(["↵ difftool", "s stage", "u unstage", "space flip"]),
        Level::Remotes => {
            if let Some(r) = app.remote() {
                keys.push("↵ push");
                if r.format == Format::Mirror {
                    keys.push("a adapt");
                }
                if r.mount.is_some() {
                    keys.push("m mount");
                }
                if crate::mirror::local_root(&r.url).is_some() {
                    keys.push("v convert");
                }
            }
        }
    }
    keys.push("K inspect");
    match app.level {
        Level::Remotes => {
            keys.push(CREATE);
            if app.remote().is_some() {
                keys.extend([EDIT, DEL]);
            }
        }
        Level::Status => keys.push(CREATE),
        _ => {}
    }
    keys.extend([FIND, REFRESH, QUIT]);
    keys
}

const MOVING: HelpSection = (
    "moving",
    &[
        (Y_MOVE, "move a row"),
        (X_MOVE, "the previous, next tab"),
        ("", "a held key speeds up"),
    ],
);
const BELOW: HelpSection = (
    "the pane below",
    &[(CTRL_Y_MOVE, "move in it"), ("ctrl-f", "find in it")],
);
const EVERYWHERE: HelpSection = (
    "every screen",
    &[
        ("esc", "back"),
        ("/", "find"),
        ("K", "inspect the row"),
        ("r", "refresh"),
        (":", "a command (:q quits)"),
        ("?", "this help"),
        ("q", "quit, once the job in the footer is done"),
        (
            "ctrl-c",
            "quit, or esc in a box, a form, a filter or the bar",
        ),
        ("", "a change: + added · ~ changed · ⇄ moved · - removed"),
    ],
);
const REPOS: HelpSection = (
    "repos",
    &[
        ("↵", "its history"),
        ("", "✚ files changed since the last commit"),
        ("", "↑ commits some remote does not have yet"),
    ],
);
const REMOTES: HelpSection = (
    "remotes",
    &[
        ("↵", "push to it, never over what changed there"),
        (
            "a",
            "adapt: bring what changed on a mirror here, asking before it deletes a file",
        ),
        ("m", "run its mount command"),
        ("v", "convert between mirror and backup"),
        ("c", "add a remote"),
        ("e", "change one"),
        ("d", "forget one, asking first; nothing on it is touched"),
        (
            "",
            "here · unplugged · mountable · cloud: whether it is there now",
        ),
        (
            "",
            "a mount command runs first, in the terminal, then the screen is back",
        ),
        ("", "below: what a push would do, from what was last pushed"),
    ],
);
const SCAN: HelpSection = (
    "the bar",
    &[
        ("type", "part of a file name"),
        ("↵", "find every commit that touched one"),
        ("esc", "back to the hits, or quit before the first run"),
        ("/", "from the hits, back to the bar"),
        ("", "↵ on a hit opens its commit as ilog does"),
    ],
);
const STATUS: HelpSection = (
    "stage",
    &[
        ("s", "stage"),
        ("u", "unstage"),
        ("space", "flip between the two"),
        ("S", "stage every row"),
        ("U", "unstage everything"),
        ("c", "create a commit from what is staged"),
        ("", "S staged · M not staged · ? untracked"),
        ("", "staging decodes audio, so a song keeps its history"),
    ],
);
const DIFF: HelpSection = (
    "diff",
    &[
        (CTRL_Y_MOVE, "scroll"),
        (CTRL_X_MOVE, "pan"),
        (
            "↵",
            "git difftool, or your editor when only one version is here",
        ),
        (
            "",
            "text up to 1 MB is kept as it is staged, so its diff needs no remote",
        ),
        ("", "anything else shows its size"),
    ],
);
const COMMITS: HelpSection = (
    "commits",
    &[
        ("↵", "its changes, each with its versions below"),
        ("", "↑ on no remote yet"),
    ],
);
const FILES: HelpSection = (
    "changes",
    &[
        (
            "↵",
            "put the file back as this commit has it, from a remote",
        ),
        ("", "● the version the commit above is"),
    ],
);
const IN_A_BOX: HelpSection = (
    "in a box",
    &[
        ("y n", "answer"),
        (X_MOVE, "pick"),
        ("↵", "select"),
        (Y_MOVE, "scroll a long one"),
        ("esc", "cancel, or close a long one"),
    ],
);
const IN_A_FORM: HelpSection = (
    "in a form",
    &[
        ("type", "fill the field"),
        ("tab shift-tab", "the next, previous field"),
        (CTRL_Y_MOVE, "the next, previous field"),
        ("←→ space", "change a pick"),
        ("↵", "the next field, or submit on the last"),
        ("esc", "cancel"),
    ],
);
const IN_A_FIND: HelpSection = (
    "in a find",
    &[
        ("type", "filter"),
        ("↵", "keep"),
        ("esc", "back"),
        ("?", "help"),
    ],
);
const IN_THIS_HELP: HelpSection = (
    "in this help",
    &[
        (Y_MOVE, "scroll"),
        ("ctrl-d ctrl-u", "half a page down, up"),
        ("g G", "the top, the bottom"),
        ("esc q ?", "close"),
    ],
);

/// Every key the app answers to, for the levels it can reach from where it
/// was entered.
fn help(app: &App) -> Vec<HelpSection> {
    let below = if app.start == Level::Status {
        DIFF
    } else {
        BELOW
    };
    let mut sections = vec![MOVING, below, EVERYWHERE];
    match app.start {
        Level::Repos => sections.push(REPOS),
        Level::Remotes => sections.push(REMOTES),
        Level::Scan => sections.push(SCAN),
        Level::Status => sections.push(STATUS),
        Level::Commits | Level::Files => {}
    }
    if matches!(app.start, Level::Repos | Level::Scan | Level::Commits) {
        sections.extend([COMMITS, FILES]);
    }
    sections.push(IN_A_BOX);
    if matches!(app.start, Level::Remotes | Level::Status) {
        sections.push(IN_A_FORM);
    }
    sections.extend([IN_A_FIND, IN_THIS_HELP]);
    sections
}

impl App {
    /// `?`: every key, in a reader over the panes.
    pub(super) fn help(&mut self) {
        self.modal = Some(Modal::help(&help(self)));
    }
}

fn list(
    frame: &mut Frame,
    area: Rect,
    items: Vec<ListItem<'static>>,
    title: Line<'static>,
    sel: &mut Sel,
) {
    let widget = List::new(items)
        .block(pane_block(title, true))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ");
    frame.render_stateful_widget(widget, area, &mut sel.state);
    list_scrollbar(frame, area, sel.len(), sel.state.offset());
}

/// `" <name>  <tabs>  <i>/<n>   <filter> "`, the title every pane wears, with a
/// trailing `…` while rows are still arriving.
fn title(
    name: &str,
    scope: Option<(&[&str], usize)>,
    sel: &Sel,
    loading: bool,
    pane: Pane,
    editing: bool,
) -> Line<'static> {
    // The scan's hits are found again from the bar, so `/` is not theirs.
    let filterable = !(name == "hits" && pane == Pane::Top);
    let mut spans = vec![Span::raw(format!(" {name} "))];
    if let Some((labels, picked)) = scope {
        spans.extend(scope_tabs(labels, picked));
    }
    spans.push(Span::raw(" "));
    let count = if sel.is_empty() {
        if loading { "…" } else { "(none)" }.to_string()
    } else {
        let dots = if loading { "…" } else { "" };
        format!("{}/{}{dots}", sel.cur + 1, sel.len())
    };
    spans.push(Span::raw(count));
    spans.extend(sel.query.title_span(pane.sigil(), editing, filterable));
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// `  +2 ~0 ⇄6 -0`, after a pane's name, or nothing for no changes.
fn counts(changes: &[Change]) -> String {
    if changes.is_empty() {
        return String::new();
    }
    let n = |k| changes.iter().filter(|c| c.kind == k).count();
    format!(
        "  +{} ~{} ⇄{} -{}",
        n(Kind::Added),
        n(Kind::Modified),
        n(Kind::Moved),
        n(Kind::Removed)
    )
}

fn dim_row(text: &str) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        format!("  {text}"),
        Style::default().add_modifier(Modifier::DIM),
    )))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

// ── row renderers ───────────────────────────────────────────────────────────

fn repo_items(app: &App) -> Vec<ListItem<'static>> {
    let name_w = app
        .rsel
        .visible
        .iter()
        .map(|&i| display(&app.repos[i].name).chars().count())
        .max()
        .unwrap_or(0)
        .clamp(1, NAME_W);
    app.rsel
        .visible
        .iter()
        .map(|&i| {
            let r = &app.repos[i];
            let mut spans = vec![Span::styled(
                format!("  {:<name_w$}  ", truncate(&display(&r.name), name_w)),
                Style::default().add_modifier(Modifier::BOLD),
            )];
            match &r.state {
                None => spans.push(Span::styled(
                    "checking…",
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Some(Err(e)) => spans.push(Span::styled(
                    format!("could not be read: {e}"),
                    Style::default().fg(Color::Yellow),
                )),
                Some(Ok(s)) => {
                    let mut marks = 0;
                    if s.changed > 0 {
                        let t = format!("✚{} ", s.changed);
                        marks += t.chars().count();
                        spans.push(Span::styled(t, Style::default().fg(Color::Yellow)));
                    }
                    if s.unpushed() > 0 {
                        let t = format!("↑{} ", s.unpushed());
                        marks += t.chars().count();
                        spans.push(Span::styled(t, Style::default().fg(Color::Green)));
                    }
                    if marks == 0 {
                        marks = 8;
                        spans.push(Span::styled("✓ clean ", Style::default().fg(Color::Green)));
                    }
                    spans.push(Span::raw(" ".repeat(10usize.saturating_sub(marks))));
                    let remotes = if s.remotes.is_empty() {
                        "no remotes".to_string()
                    } else {
                        s.remotes
                            .iter()
                            .map(|(n, b)| format!("{n} {}", b.words()))
                            .collect::<Vec<_>>()
                            .join(SEP)
                    };
                    spans.push(Span::styled(remotes, Style::default().fg(Color::DarkGray)));
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect()
}

fn remote_items(app: &App) -> Vec<ListItem<'static>> {
    if app.remotes.is_empty() {
        return vec![dim_row("no remotes yet: c adds one")];
    }
    let width = |f: &dyn Fn(usize) -> usize, cap: usize| {
        app.msel
            .visible
            .iter()
            .map(|&i| f(i))
            .max()
            .unwrap_or(0)
            .clamp(1, cap)
    };
    let name_w = width(&|i| app.remotes[i].name.chars().count(), NAME_W);
    let url_w = width(
        &|i| crate::paths::short_url(&app.remotes[i].url).chars().count(),
        URL_W,
    );
    app.msel
        .visible
        .iter()
        .map(|&i| {
            let r = &app.remotes[i];
            let behind = app.behind(r);
            let ahead = matches!(behind, Some(Behind::By(n)) if n > 0)
                || matches!(behind, Some(Behind::Never));
            let mark = if ahead {
                Span::styled(
                    "↑ ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("  ")
            };
            let reach_style = match r.reach {
                Reach::Here => Style::default().fg(Color::Green),
                Reach::Unplugged => Style::default().fg(Color::Yellow),
                Reach::Mountable => Style::default().fg(Color::Cyan),
                Reach::Cloud | Reach::Checking => Style::default().add_modifier(Modifier::DIM),
            };
            let behind_words = behind.map_or("…".to_string(), |b| b.words());
            ListItem::new(Line::from(vec![
                mark,
                Span::styled(
                    format!("{:<name_w$}", truncate(&r.name, name_w)),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!(
                        "  {:<url_w$}",
                        truncate(&crate::paths::short_url(&r.url), url_w)
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(format!("  {:<6}", r.format.name())),
                Span::styled(format!("  {:<10}", r.reach.word()), reach_style),
                Span::raw(format!("  {behind_words}")),
            ]))
        })
        .collect()
}

fn commit_items(app: &App) -> Vec<ListItem<'static>> {
    if app.csel.is_empty() && app.cfeed.slow() {
        return vec![dim_row("loading…")];
    }
    app.csel
        .visible
        .iter()
        .map(|&i| {
            let c = &app.log[i];
            let mark = if app.unpushed(i) {
                Span::styled(
                    "↑ ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("  ")
            };
            let n = |k| c.count(k);
            ListItem::new(Line::from(vec![
                mark,
                Span::styled(
                    format!("{} ", short(&c.hash)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{}  ", short_time(c.time)),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!("+{:<3}", n(Kind::Added)),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!("~{:<3}", n(Kind::Modified)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("⇄{:<3}", n(Kind::Moved)),
                    Style::default().fg(Color::Blue),
                ),
                Span::styled(
                    format!("-{:<3} ", n(Kind::Removed)),
                    Style::default().fg(Color::Red),
                ),
                Span::raw(display(&c.message)),
            ]))
        })
        .collect()
}

fn kind_style(k: Kind) -> Style {
    Style::default().fg(match k {
        Kind::Added => Color::Green,
        Kind::Modified => Color::Yellow,
        Kind::Moved => Color::Blue,
        Kind::Removed => Color::Red,
    })
}

/// A path with the characters a move changed marked: taken away in red on the
/// old name, added in green on the new.
fn marked(text: &str, kept: &[bool], added: bool) -> Vec<Span<'static>> {
    let changed = if added {
        Style::default().fg(Color::Black).bg(Color::Green)
    } else {
        Style::default().fg(Color::Black).bg(Color::Red)
    };
    let same = if added {
        Style::default().fg(Color::Blue)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_kept = None;
    for (i, ch) in text.chars().enumerate() {
        let k = kept.get(i).copied().unwrap_or(false);
        if run_kept.is_some_and(|r| r != k) {
            let style = if run_kept == Some(true) {
                same
            } else {
                changed
            };
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        run_kept = Some(k);
        run.push_str(&display(ch.encode_utf8(&mut [0; 4])));
    }
    if !run.is_empty() {
        let style = if run_kept == Some(true) {
            same
        } else {
            changed
        };
        spans.push(Span::styled(run, style));
    }
    spans
}

/// One change: its mark, then its path, a move's two names with what changed
/// marked when `marks`, else as `{old => new}`.
fn change_line(c: &Change, marks: bool) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        format!("{} ", c.kind.mark()),
        kind_style(c.kind),
    )];
    match (&c.from, marks) {
        (Some(from), true) => {
            let (kept_from, kept_to) = common(from, &c.path);
            spans.extend(marked(from, &kept_from, false));
            spans.push(Span::styled(" → ", Style::default().fg(Color::Blue)));
            spans.extend(marked(&c.path, &kept_to, true));
        }
        _ => spans.push(Span::styled(display(&c.shown()), kind_style(c.kind))),
    }
    spans
}

fn change_items(app: &App, marks: bool) -> Vec<ListItem<'static>> {
    if app.fsel.is_empty() && app.pfeed.slow() {
        return vec![dim_row("loading…")];
    }
    app.fsel
        .visible
        .iter()
        .map(|&i| ListItem::new(Line::from(change_line(&app.files[i], marks))))
        .collect()
}

fn status_items(app: &App) -> Vec<ListItem<'static>> {
    if let Some(e) = &app.status_failed {
        return vec![dim_row(e)];
    }
    if app.ssel.is_empty() && app.sfeed.slow() {
        return vec![dim_row("reading the working tree…")];
    }
    if app.status.is_empty() && !app.sfeed.loading {
        return vec![dim_row("nothing to commit, working tree clean")];
    }
    app.ssel
        .visible
        .iter()
        .map(|&i| status_item(&app.status[i]))
        .collect()
}

fn status_item(r: &StatusRow) -> ListItem<'static> {
    let (side, style) = match r.side {
        Side::Staged => ("S", Style::default().fg(Color::Green)),
        Side::Unstaged => ("M", Style::default().fg(Color::Yellow)),
        Side::Untracked => ("?", Style::default().fg(Color::Red)),
    };
    let mut spans = vec![Span::styled(format!("{side} "), style)];
    spans.extend(change_line(&r.change, false));
    ListItem::new(Line::from(spans))
}

fn hit_items(app: &App) -> Vec<ListItem<'static>> {
    if app.searched.is_empty() {
        return vec![dim_row("type part of a file name above, then ↵")];
    }
    if app.hits.is_empty() {
        let text = if app.cfeed.loading {
            "reading the history…"
        } else {
            "no commit touched a file by that name"
        };
        return vec![dim_row(text)];
    }
    app.xsel
        .visible
        .iter()
        .map(|&i| {
            let h = &app.hits[i];
            let c = &app.log[h.at];
            let mut spans = vec![
                Span::styled(
                    format!("{} ", short(&c.hash)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{}  ", short_time(c.time)),
                    Style::default().fg(Color::Green),
                ),
            ];
            spans.extend(change_line(&h.change, false));
            spans.push(Span::styled(
                format!("  {}", display(&c.message)),
                Style::default().add_modifier(Modifier::DIM),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect()
}

fn version_items(app: &App) -> Vec<ListItem<'static>> {
    if app.hsel.is_empty() && app.cfeed.slow() {
        return vec![dim_row("reading the history…")];
    }
    app.hsel
        .visible
        .iter()
        .map(|&i| {
            let v = &app.hist[i];
            let c = &app.log[v.at];
            let here = if c.hash == app.files_for {
                "● "
            } else {
                "  "
            };
            let mut spans = vec![
                Span::styled(here, Style::default().fg(Color::Cyan)),
                Span::styled(
                    format!("{} ", short(&c.hash)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{}  ", short_time(c.time)),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!("{:<8}", v.change.kind.word()),
                    kind_style(v.change.kind),
                ),
                Span::raw(display(&v.change.shown())),
            ];
            spans.push(Span::styled(
                format!("  {}", display(&c.message)),
                Style::default().add_modifier(Modifier::DIM),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect()
}
