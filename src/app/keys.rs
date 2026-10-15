//! Key dispatch, one arm per level.
//!
//! The shape is the same everywhere: plain keys drive the top pane, Ctrl drives
//! the pane below it, `h`/`l` slide that level's tabs, Enter does the level's
//! main thing and Esc steps back out. `/` filters the top pane and `ctrl-f` the
//! one below.

use super::{App, FormFor, Level, Pane, commits, repos, status};
use crate::tui::input::{
    is_back, is_ctrl_c, is_down, is_help, is_left, is_open, is_right, is_up, norm_esc,
};
use crate::tui::widgets::{Command, CommandLine, FormKey, Typed};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Handle one key press. Returns true when the app should quit, which it never
/// does while a job runs, since quitting kills the job's thread mid-write.
pub(super) fn on_key(app: &mut App, key: KeyEvent, terminal: &mut DefaultTerminal) -> bool {
    let quit = handle(app, key, terminal);
    if let Some(job) = app.pending_job.take() {
        app.start(job, terminal);
    }
    if quit && let Some(job) = &app.job {
        let doing = job.doing();
        app.set_failed(format!("still {doing}: quit once it is done"));
        return false;
    }
    quit
}

/// Whether a job is running, said on the status line. A key that writes the
/// config or the index checks this first, since the job writes them as it ends
/// and would put back what the key changed.
fn busy(app: &mut App) -> bool {
    let Some(job) = &app.job else {
        return false;
    };
    let doing = job.doing();
    app.set_failed(format!("still {doing}: try again once it is done"));
    true
}

fn handle(app: &mut App, key: KeyEvent, terminal: &mut DefaultTerminal) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let mut code = norm_esc(key.code, ctrl);
    let in_bar = app.level == Level::Scan && app.typing;

    // Ctrl-C is Esc under a box, in a form, in the `:` line, in a filter or in
    // the bar, and quits from the view itself.
    if is_ctrl_c(code, ctrl) {
        let open = app.modal.is_some()
            || app.form.is_some()
            || app.confirm.is_some()
            || app.command.is_some()
            || app.editing.is_some()
            || in_bar;
        if !open {
            return true;
        }
        code = KeyCode::Esc;
    }
    if let Some(modal) = &mut app.modal {
        if modal.on_key(code, ctrl) {
            app.modal = None;
        }
        return false;
    }
    if let Some((form, _)) = &mut app.form {
        match form.on_key(code, key) {
            FormKey::Open => {}
            FormKey::Cancel => app.form = None,
            FormKey::Submit => submit(app),
        }
        return false;
    }
    // A typed gate goes ahead only once the name is typed exactly.
    if let Some(confirm) = &mut app.confirm
        && confirm.target.typed()
    {
        match code {
            KeyCode::Enter if confirm.typed.text == confirm.target.name() => {
                if let Some(confirm) = app.confirm.take() {
                    app.confirmed(confirm.target, terminal);
                }
            }
            KeyCode::Esc => app.confirm = None,
            _ => {
                confirm.typed.on_key(key);
            }
        }
        return false;
    }
    // A gate or an offer: Yes on the left, opening on No for a gate and on Yes
    // for an offer.
    if let Some(confirm) = &mut app.confirm {
        if is_left(code) {
            confirm.yes = true;
        } else if is_right(code) {
            confirm.yes = false;
        } else if code == KeyCode::Char('y') || (is_open(code) && confirm.yes) {
            if let Some(confirm) = app.confirm.take() {
                app.confirmed(confirm.target, terminal);
            }
        } else if is_open(code) || is_back(code) || matches!(code, KeyCode::Char('q' | 'n')) {
            app.confirm = None;
        }
        return false;
    }
    if let Some(cmd) = &mut app.command {
        match cmd.on_key(code) {
            Typed::Open => {}
            Typed::Cancel => app.command = None,
            Typed::Run(run) => {
                app.command = None;
                match run {
                    Command::Help => app.help(),
                    Command::Quit => return true,
                    Command::Unknown(what) => {
                        app.set_failed(format!("unknown command `{what}` · ? lists every key"))
                    }
                }
            }
        }
        return false;
    }
    if let Some(pane) = app.editing {
        if is_help(code) {
            app.help();
        } else {
            query_key(app, pane, code, key);
        }
        return false;
    }
    if in_bar {
        match code {
            KeyCode::Enter => app.run_scan(),
            KeyCode::Char('?') => app.help(),
            // Esc leaves the bar only when there are hits to go back to.
            KeyCode::Esc if !app.searched.is_empty() => app.typing = false,
            KeyCode::Esc => return true,
            _ => {
                app.bar.on_key(key);
            }
        }
        return false;
    }

    if code == KeyCode::Char('q') {
        return true;
    }
    if is_help(code) {
        app.help();
        return false;
    }
    if code == KeyCode::Char(':') {
        app.command = Some(CommandLine::default());
        return false;
    }
    if !ctrl && code == KeyCode::Char('/') {
        if app.level == Level::Scan {
            app.bar.open();
            app.typing = true;
        } else {
            app.open_query(Pane::Top);
        }
        return false;
    }
    if ctrl && code == KeyCode::Char('f') {
        app.open_query(Pane::Bottom);
        return false;
    }
    if code == KeyCode::Char('r') {
        app.refresh();
        return false;
    }

    // Only another move of the top pane can leave a load pending: anything
    // else acts on the pane that load fills.
    let moving = !ctrl && (is_up(code) || is_down(code) || is_left(code) || is_right(code));
    if app.pending && !moving {
        app.settle();
    }
    if !ctrl && code == KeyCode::Char('K') {
        app.inspect();
        return false;
    }

    let steps = app.accel.steps(code, ctrl);
    match app.level {
        Level::Repos => repos_key(app, code, ctrl, steps),
        Level::Remotes => remotes_key(app, code, ctrl, steps, terminal),
        Level::Scan => scan_key(app, code, ctrl, steps),
        Level::Status => status_key(app, code, ctrl, steps, terminal),
        Level::Commits => commits_key(app, code, ctrl, steps),
        Level::Files => files_key(app, code, ctrl, steps),
    }
}

/// A form's Enter on its last field. A refusal leaves it up, under the alert.
fn submit(app: &mut App) {
    let Some((form, kind)) = app.form.take() else {
        return;
    };
    let done = match &kind {
        FormFor::NewRemote => app.submit_remote(&form, None),
        FormFor::EditRemote(was) => app.submit_remote(&form, Some(was)),
        FormFor::Commit => app.submit_commit(&form),
    };
    if !done {
        app.form = Some((form, kind));
    }
}

/// The filter has focus: Enter keeps what it found and Esc clears it.
fn query_key(app: &mut App, pane: Pane, code: KeyCode, key: KeyEvent) {
    if code == KeyCode::Enter {
        app.editing = None;
        return;
    }
    if code == KeyCode::Esc {
        if let Some(sel) = app.pane_sel(pane) {
            sel.query.clear();
        }
        app.editing = None;
        app.refilter(pane);
        return;
    }
    let Some(sel) = app.pane_sel(pane) else {
        app.editing = None;
        return;
    };
    if sel.query.on_key(key) {
        app.refilter(pane);
    }
}

/// Repos on top, the selected repo's commits below.
fn repos_key(app: &mut App, code: KeyCode, ctrl: bool, steps: usize) -> bool {
    if ctrl && (is_down(code) || is_up(code)) {
        app.csel.step(is_down(code), steps);
    } else if is_down(code) || is_up(code) {
        if app.rsel.step(is_down(code), steps) {
            app.pending = true;
        }
    } else if !ctrl && (is_left(code) || is_right(code)) {
        if app.rsel.slide(is_right(code), repos::SCOPES.len()) {
            app.rescope_repos();
            app.pending = true;
        }
    } else if is_open(code) && !app.csel.is_empty() {
        app.level = Level::Commits;
        app.enter_commit();
    } else if is_back(code) {
        return app.back();
    }
    false
}

/// Remotes on top, what a push to the selected one would do below.
fn remotes_key(
    app: &mut App,
    code: KeyCode,
    ctrl: bool,
    steps: usize,
    terminal: &mut DefaultTerminal,
) -> bool {
    let name = app.remote().map(|r| r.name.clone());
    if !ctrl && matches!(code, KeyCode::Char('c' | 'e' | 'd')) && busy(app) {
        return false;
    }
    if !ctrl {
        match (code, &name) {
            (KeyCode::Char('c'), _) => app.create_remote(),
            (KeyCode::Enter, Some(name)) => app.ask_push(name, terminal),
            (KeyCode::Char('a'), Some(_)) => app.ask_adapt(),
            (KeyCode::Char('m'), Some(name)) => app.mount(name, terminal),
            (KeyCode::Char('v'), Some(_)) => app.ask_convert(),
            (KeyCode::Char('e'), Some(_)) => app.edit_remote(),
            (KeyCode::Char('d'), Some(_)) => app.ask_forget(),
            _ => {}
        }
        if matches!(
            code,
            KeyCode::Enter | KeyCode::Char('c' | 'a' | 'm' | 'v' | 'e' | 'd')
        ) {
            return false;
        }
    }
    if ctrl && (is_down(code) || is_up(code)) {
        app.fsel.step(is_down(code), steps);
    } else if is_down(code) || is_up(code) {
        if app.msel.step(is_down(code), steps) {
            app.pending = true;
        }
    } else if is_back(code) {
        return app.back();
    }
    false
}

/// Hits on top, the hit's commit's changes below.
fn scan_key(app: &mut App, code: KeyCode, ctrl: bool, steps: usize) -> bool {
    if ctrl && (is_down(code) || is_up(code)) {
        app.fsel.step(is_down(code), steps);
    } else if is_down(code) || is_up(code) {
        if app.xsel.step(is_down(code), steps) {
            app.pending = true;
        }
    } else if is_open(code) {
        app.open_hit();
    } else if is_back(code) {
        return app.back();
    }
    false
}

/// Rows a ctrl-j/k moves the diff and columns a ctrl-h/l pans it, before a held
/// key multiplies them.
const SCROLL_STEP: u16 = 3;
const PAN_STEP: u16 = 8;

/// The working tree's changes on top, the selected file's diff below.
fn status_key(
    app: &mut App,
    code: KeyCode,
    ctrl: bool,
    steps: usize,
    terminal: &mut DefaultTerminal,
) -> bool {
    if !ctrl && matches!(code, KeyCode::Char(' ' | 's' | 'u' | 'S' | 'U' | 'c')) && busy(app) {
        return false;
    }
    if !ctrl {
        let staged = app
            .status_row()
            .map(|r| r.side == super::load::Side::Staged);
        match code {
            KeyCode::Enter => app.difftool(terminal),
            KeyCode::Char(' ') => app.toggle(),
            KeyCode::Char('s') if staged == Some(false) => app.toggle(),
            KeyCode::Char('u') if staged == Some(true) => app.toggle(),
            KeyCode::Char('S') => app.stage_all(),
            KeyCode::Char('U') => app.unstage_all(),
            KeyCode::Char('c') => app.create_commit(),
            _ => {}
        }
        if matches!(
            code,
            KeyCode::Enter | KeyCode::Char(' ' | 's' | 'u' | 'S' | 'U' | 'c')
        ) {
            return false;
        }
    }
    let by = |step: u16| step.saturating_mul(u16::try_from(steps).unwrap_or(u16::MAX));
    if ctrl && is_down(code) {
        app.dscroll = app.dscroll.saturating_add(by(SCROLL_STEP));
    } else if ctrl && is_up(code) {
        app.dscroll = app.dscroll.saturating_sub(by(SCROLL_STEP));
    } else if ctrl && is_right(code) {
        app.dhscroll = app.dhscroll.saturating_add(by(PAN_STEP));
    } else if ctrl && is_left(code) {
        app.dhscroll = app.dhscroll.saturating_sub(by(PAN_STEP));
    } else if is_down(code) || is_up(code) {
        if app.ssel.step(is_down(code), steps) {
            app.pending = true;
        }
    } else if !ctrl && (is_left(code) || is_right(code)) {
        if app.ssel.slide(is_right(code), status::SCOPES.len()) {
            app.rescope_status();
            app.pending = true;
        }
    } else if is_back(code) {
        return app.back();
    }
    false
}

/// Commits on top, the selected commit's changes below.
fn commits_key(app: &mut App, code: KeyCode, ctrl: bool, steps: usize) -> bool {
    if ctrl && (is_down(code) || is_up(code)) {
        app.fsel.step(is_down(code), steps);
    } else if is_down(code) || is_up(code) {
        if app.csel.step(is_down(code), steps) {
            app.pending = true;
        }
    } else if !ctrl && (is_left(code) || is_right(code)) {
        if app.csel.slide(is_right(code), commits::SCOPES.len()) {
            app.rescope_log();
            app.pending = true;
        }
    } else if is_open(code) {
        app.open_commit();
    } else if is_back(code) {
        return app.back();
    }
    false
}

/// A commit's changes on top, the selected file's versions below.
fn files_key(app: &mut App, code: KeyCode, ctrl: bool, steps: usize) -> bool {
    if ctrl && (is_down(code) || is_up(code)) {
        app.hsel.step(is_down(code), steps);
    } else if is_down(code) || is_up(code) {
        if app.fsel.step(is_down(code), steps) {
            app.pending = true;
        }
    } else if is_open(code) {
        app.ask_restore();
    } else if is_back(code) {
        return app.back();
    }
    false
}
