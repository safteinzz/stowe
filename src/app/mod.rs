//! The drill: repos → commits → files, with the remotes, the name search and
//! the working tree as places it can start.
//!
//! One app with one level stack, entered at whichever level the command asked
//! for. `stowe irepos` starts at Repos, `stowe ilog` at Commits, `stowe iscan`
//! at Scan, `stowe istatus` at Status and `stowe iremote` at Remotes; Esc walks
//! back up and quits at the level it was entered on.
//!
//! Every screen is two panes: the current level's list on top, what the row
//! under the cursor holds underneath. Plain keys drive the top pane, Ctrl drives
//! the bottom one. Nothing that reads a tree or a history runs on the input
//! loop: it streams in from `load` and the pane fills as it arrives.

mod commits;
mod inspect;
mod jobs;
mod keys;
mod load;
mod patch;
mod remotes;
mod repos;
mod scan;
mod status;
mod ui;

use crate::history::Change;
use crate::scan::progress_to_screen;
use crate::tui::difffeed::DiffFeed;
use crate::tui::highlight::RenderedDiff;
use crate::tui::input::{Accel, stepped};
use crate::tui::widgets::{CommandLine, Form, Modal, NOTE, Query};
use crate::tui::{FRAME, pop_keyboard_enhancement, push_keyboard_enhancement};
use load::{Batch, Logged, StatusRow};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::widgets::ListState;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a pane may be empty before it says it is still loading. A load
/// quick enough not to be noticed says nothing at all.
const SLOW: Duration = Duration::from_millis(120);

/// Which list the top pane is showing. The order is the drill's: Esc steps back
/// only while the level is deeper than the one the app was entered at.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Repos,
    Remotes,
    Scan,
    Status,
    Commits,
    Files,
}

impl Level {
    /// The level Esc steps back to from here, in an app entered at `start`, or
    /// None at the top of the stack.
    fn back(self, start: Level) -> Option<Level> {
        match self {
            Level::Repos | Level::Remotes | Level::Scan | Level::Status => None,
            Level::Commits => match start {
                Level::Repos | Level::Scan => Some(start),
                _ => None,
            },
            Level::Files => Some(Level::Commits),
        }
    }
}

/// A selection over one list: which of its entries the current scope keeps,
/// where the cursor sits in that subset, and the widget state that scrolls it.
#[derive(Default)]
pub struct Sel {
    /// Indices into the owning Vec, in display order.
    pub visible: Vec<usize>,
    pub cur: usize,
    pub state: ListState,
    /// Index into the level's scope slider, moved with h/l.
    pub scope: usize,
    /// What the cursor was on when the list was read again, named rather than
    /// numbered, since that row may come back at another position or not at all.
    pub restore: Option<String>,
    /// What `/` (or `ctrl-f` on the pane below) narrowed this list to.
    pub query: Query,
}

impl Sel {
    /// Show these entries, cursor back at the top.
    fn show(&mut self, visible: Vec<usize>) {
        self.visible = visible;
        self.cur = 0;
        self.state.select((!self.visible.is_empty()).then_some(0));
    }

    pub fn len(&self) -> usize {
        self.visible.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// Index into the owning Vec of whatever the cursor is on.
    pub fn idx(&self) -> Option<usize> {
        self.visible.get(self.cur).copied()
    }

    fn restored_at(&mut self, at: usize) {
        self.cur = at;
        self.state.select(Some(at));
        self.restore = None;
    }

    /// Add entries a background load produced. Rows landing under the cursor
    /// never move it.
    fn append(&mut self, more: Vec<usize>) {
        let was_empty = self.visible.is_empty();
        self.visible.extend(more);
        if was_empty && !self.visible.is_empty() {
            self.cur = 0;
            self.state.select(Some(0));
        }
    }

    /// Put the cursor on the visible row `name` gives `want` for, if it is
    /// there now, which is how a list read again keeps its place.
    fn settle_on(&mut self, name: impl Fn(usize) -> String) {
        if let Some(want) = self.restore.clone()
            && let Some(at) = self.visible.iter().position(|&i| name(i) == want)
        {
            self.restored_at(at);
        }
    }

    /// Move the cursor `rows` at a time, stopping at either end. Returns
    /// whether it actually moved.
    fn step(&mut self, down: bool, rows: usize) -> bool {
        if self.visible.is_empty() {
            return false;
        }
        let to = stepped(self.cur, self.visible.len(), down, rows);
        if to == self.cur {
            return false;
        }
        self.cur = to;
        self.state.select(Some(to));
        true
    }

    /// Slide the scope one notch within `len` stops, returning whether it moved.
    fn slide(&mut self, right: bool, len: usize) -> bool {
        if right && self.scope + 1 < len {
            self.scope += 1;
            true
        } else if !right && self.scope > 0 {
            self.scope -= 1;
            true
        } else {
            false
        }
    }
}

/// One pane's background load: the request in flight, shared with the worker
/// so a superseded one stops itself, and whether rows are still coming.
#[derive(Default)]
struct Feed {
    latest: Arc<AtomicU64>,
    shown: u64,
    loading: bool,
    since: Option<Instant>,
}

impl Feed {
    /// Claim the next request number, superseding whatever was in flight.
    fn issue(&mut self) -> u64 {
        let seq = self.latest.fetch_add(1, Ordering::Relaxed) + 1;
        self.shown = seq;
        self.loading = true;
        self.since = Some(Instant::now());
        seq
    }

    /// Has this load been going long enough to be worth mentioning?
    fn slow(&self) -> bool {
        self.loading && self.since.is_some_and(|t| t.elapsed() >= SLOW)
    }

    fn accepts(&self, seq: u64) -> bool {
        seq == self.shown
    }
}

/// Which of a level's two lists a filter key opens: `/` the one plain keys
/// drive, `ctrl-f` the pane below it.
#[derive(Clone, Copy, PartialEq)]
pub enum Pane {
    Top,
    Bottom,
}

impl Pane {
    pub fn sigil(self) -> &'static str {
        match self {
            Pane::Top => "/",
            Pane::Bottom => "ctrl-f",
        }
    }
}

/// A form that is up, and what its submit does.
pub(super) enum FormFor {
    NewRemote,
    EditRemote(String),
    Commit,
}

pub struct App {
    level: Level,
    start: Level,
    enhanced: bool,
    note: Option<(bool, String)>,
    note_at: Instant,
    modal: Option<Modal>,
    confirm: Option<jobs::Confirm>,
    form: Option<(Form, FormFor)>,
    command: Option<CommandLine>,
    /// The pane below the cursor is out of date; it is asked for once the keys
    /// stop coming.
    pending: bool,
    editing: Option<Pane>,
    accel: Accel,
    tx: Sender<Batch>,
    rx: Receiver<Batch>,
    /// The job running, which the footer names.
    job: Option<jobs::Job>,
    /// A job a key asked for, started once the key is handled, since starting
    /// one may hand the terminal to a mount command.
    pending_job: Option<jobs::Job>,
    /// What the running job last said about how far along it is.
    progress: Arc<Mutex<String>>,

    /// The repo every level below Repos reads.
    repo: PathBuf,

    // ── repos level ─────────────────────────────────────────────────────────
    base: PathBuf,
    depth: usize,
    repos: Vec<repos::RepoRow>,
    rsel: Sel,
    rfeed: Feed,

    // ── remotes level ───────────────────────────────────────────────────────
    remotes: Vec<remotes::RemoteRow>,
    msel: Sel,

    // ── scan level ──────────────────────────────────────────────────────────
    bar: Query,
    /// The bar has the keys.
    typing: bool,
    /// What the hits were found for, empty before the first run.
    searched: String,
    hits: Vec<scan::Hit>,
    xsel: Sel,

    // ── status level ────────────────────────────────────────────────────────
    status: Vec<StatusRow>,
    ssel: Sel,
    sfeed: Feed,
    status_failed: Option<String>,
    /// The selected file's diff, highlighted, and how far it is scrolled and panned.
    prepared: RenderedDiff,
    dscroll: u16,
    dhscroll: u16,
    /// Columns a commit's changes are panned right by.
    fpan: usize,
    dfeed: DiffFeed,

    // ── commits: the repo's history, read once and shared by every level ────
    log: Vec<Logged>,
    /// Commits above this place in the log are on no remote.
    pushed_from: usize,
    /// Commits above this place in the log are missing from some remote.
    synced_from: usize,
    csel: Sel,
    cfeed: Feed,
    log_failed: Option<String>,

    // ── files: a commit's changes, or what a push would do ──────────────────
    files: Vec<Change>,
    fsel: Sel,
    pfeed: Feed,
    /// The newest look at the remotes, so one that began before the config
    /// changed is dropped.
    probe_seq: u64,
    /// What `files` holds: a commit's hash, or `push:<remote>`.
    files_for: String,
    plan_failed: Option<String>,
    /// How many of `files` lead a push's plan as what changed on the mirror
    /// outside stowe.
    plan_drift: usize,

    // ── history: one file's versions ────────────────────────────────────────
    hist: Vec<commits::Version>,
    hsel: Sel,
    /// The path `hist` was worked out for, and whether the log was whole then.
    hist_for: (String, bool),
}

impl App {
    fn new(start: Level, repo: PathBuf) -> App {
        let (tx, rx) = mpsc::channel();
        App {
            level: start,
            start,
            enhanced: false,
            note: None,
            note_at: Instant::now(),
            modal: None,
            confirm: None,
            form: None,
            command: None,
            pending: false,
            editing: None,
            accel: Accel::default(),
            tx,
            rx,
            job: None,
            pending_job: None,
            progress: progress_to_screen(),
            repo,
            base: PathBuf::from("."),
            depth: 0,
            repos: Vec::new(),
            rsel: Sel::default(),
            rfeed: Feed::default(),
            remotes: Vec::new(),
            msel: Sel::default(),
            bar: Query::default(),
            typing: false,
            searched: String::new(),
            hits: Vec::new(),
            xsel: Sel::default(),
            status: Vec::new(),
            ssel: Sel::default(),
            sfeed: Feed::default(),
            status_failed: None,
            prepared: RenderedDiff::default(),
            dscroll: 0,
            dhscroll: 0,
            fpan: 0,
            dfeed: DiffFeed::default(),
            log: Vec::new(),
            pushed_from: usize::MAX,
            synced_from: usize::MAX,
            csel: Sel::default(),
            cfeed: Feed::default(),
            log_failed: None,
            files: Vec::new(),
            fsel: Sel::default(),
            pfeed: Feed::default(),
            probe_seq: 0,
            files_for: String::new(),
            plan_failed: None,
            plan_drift: 0,
            hist: Vec::new(),
            hsel: Sel::default(),
            hist_for: (String::new(), false),
        }
    }

    /// `stowe irepos`: every repo under `base`. None when there are none.
    pub fn at_repos(base: &Path, depth: usize, scope: usize) -> Option<App> {
        let found = crate::repos::find(base, depth);
        let first = found.first()?.clone();
        let mut app = App::new(Level::Repos, first);
        app.base = base.to_path_buf();
        app.depth = depth;
        app.rsel.scope = scope;
        app.load_repos(found);
        app.enter_repo();
        Some(app)
    }

    /// `stowe ilog`: the history of the repo at `repo`.
    pub fn at_commits(repo: PathBuf) -> App {
        let mut app = App::new(Level::Commits, repo);
        app.load_remotes();
        app.request_log();
        app
    }

    /// `stowe iscan`: the name search, its bar up and empty.
    pub fn at_scan(repo: PathBuf) -> App {
        let mut app = App::new(Level::Scan, repo);
        app.typing = true;
        app.load_remotes();
        app.request_log();
        app
    }

    /// `stowe istatus`: the working tree against the last commit.
    pub fn at_status(repo: PathBuf) -> App {
        let mut app = App::new(Level::Status, repo);
        app.request_status();
        app
    }

    /// `stowe iremote`: the remotes, each with what a push to it would do.
    pub fn at_remotes(repo: PathBuf) -> App {
        let mut app = App::new(Level::Remotes, repo);
        app.reread_remotes();
        app.request_log();
        app
    }

    /// Set up the terminal, run until the user quits, then put it back exactly
    /// as it was. `cmd` only names the command in an error line.
    pub fn run(mut self, cmd: &str) {
        let mut terminal = ratatui::init();
        self.enhanced = push_keyboard_enhancement();
        let result = self.event_loop(&mut terminal);
        if self.enhanced {
            pop_keyboard_enhancement();
        }
        ratatui::restore();
        if let Err(e) = result {
            eprintln!("stowe {cmd}: {}", crate::errors::io_words(&e));
            std::process::exit(1);
        }
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        loop {
            self.drain();
            if self.pending && !event::poll(Duration::ZERO)? {
                self.settle();
            }
            terminal.draw(|frame| ui::draw(frame, self))?;

            // While rows are arriving or a job runs, come back on a frame timer
            // to show them, and while a note is up, when it is due to go. With
            // neither, block on the key and leave the CPU alone.
            let wait = match (self.filling(), self.note_left()) {
                (true, Some(left)) => Some(left.min(FRAME)),
                (true, None) => Some(FRAME),
                (false, left) => left,
            };
            if let Some(wait) = wait
                && !event::poll(wait)?
            {
                if self.note_left() == Some(Duration::ZERO) {
                    self.note = None;
                }
                continue;
            }
            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press && keys::on_key(self, key, terminal) =>
                {
                    break;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Control characters in `text` (a file name can hold any) are shown as
    /// marks, never handed to the terminal, here and in `set_failed`.
    fn set_status(&mut self, text: impl Into<String>) {
        self.note = Some((true, crate::names::display(&text.into())));
        self.note_at = Instant::now();
    }

    fn set_failed(&mut self, text: impl Into<String>) {
        self.note = Some((false, crate::names::display(&text.into())));
        self.note_at = Instant::now();
    }

    fn note_left(&self) -> Option<Duration> {
        self.note
            .as_ref()
            .map(|_| NOTE.saturating_sub(self.note_at.elapsed()))
    }

    fn filling(&self) -> bool {
        self.job.is_some()
            || self.cfeed.loading
            || self.sfeed.loading
            || self.dfeed.loading()
            || self.pfeed.loading
            || self.rfeed.loading
            || self
                .remotes
                .iter()
                .any(|r| r.reach == load::Reach::Checking)
    }

    /// Take everything the background reads have produced since the last frame.
    fn drain(&mut self) {
        if let Some(prepared) = self.dfeed.take() {
            self.prepared = prepared;
        }
        while let Ok(batch) = self.rx.try_recv() {
            match batch {
                Batch::Log {
                    seq,
                    rows,
                    done,
                    failed,
                } => {
                    if !self.cfeed.accepts(seq) {
                        continue;
                    }
                    let from = self.log.len();
                    self.log.extend(rows);
                    self.extend_log(from);
                    if done {
                        self.cfeed.loading = false;
                        self.log_failed = failed;
                        self.log_done();
                    }
                }
                Batch::Status { seq, rows } => {
                    if self.sfeed.accepts(seq) {
                        self.sfeed.loading = false;
                        self.status_in(rows);
                    }
                }
                Batch::Plan { seq, rows } => {
                    if self.pfeed.accepts(seq) {
                        self.pfeed.loading = false;
                        self.plan_in(rows);
                    }
                }
                Batch::Reach {
                    seq,
                    name,
                    reach,
                    theirs,
                } => {
                    if seq == self.probe_seq {
                        self.reach_in(&name, reach, theirs);
                    }
                }
                Batch::Repo { seq, path, state } => {
                    if self.rfeed.accepts(seq) {
                        self.repo_in(path, state);
                    }
                }
                Batch::Job(outcome) => self.finished(outcome),
            }
        }
    }

    /// Run the load a cursor move deferred: the pane below the cursor.
    fn settle(&mut self) {
        self.pending = false;
        match self.level {
            Level::Repos => self.enter_repo(),
            Level::Remotes => self.enter_remote(),
            Level::Scan => self.enter_hit(),
            Level::Status => self.enter_status(),
            Level::Commits => self.enter_commit(),
            Level::Files => self.enter_file(),
        }
    }

    /// The `Sel` a filter key acts on at this level. Scan's top pane is its
    /// bar's, which `/` goes back to instead.
    fn pane_sel(&mut self, pane: Pane) -> Option<&mut Sel> {
        Some(match (self.level, pane) {
            (Level::Repos, Pane::Top) => &mut self.rsel,
            (Level::Repos, Pane::Bottom) | (Level::Commits, Pane::Top) => &mut self.csel,
            (Level::Remotes, Pane::Top) => &mut self.msel,
            (Level::Scan, Pane::Top) => return None,
            (Level::Remotes | Level::Scan | Level::Commits, Pane::Bottom)
            | (Level::Files, Pane::Top) => &mut self.fsel,
            (Level::Status, Pane::Top) => &mut self.ssel,
            (Level::Status, Pane::Bottom) => return None,
            (Level::Files, Pane::Bottom) => &mut self.hsel,
        })
    }

    /// [`App::pane_sel`], to read.
    fn pane_sel_ref(&self, pane: Pane) -> Option<&Sel> {
        Some(match (self.level, pane) {
            (Level::Repos, Pane::Top) => &self.rsel,
            (Level::Repos, Pane::Bottom) | (Level::Commits, Pane::Top) => &self.csel,
            (Level::Remotes, Pane::Top) => &self.msel,
            (Level::Scan, Pane::Top) => return None,
            (Level::Remotes | Level::Scan | Level::Commits, Pane::Bottom)
            | (Level::Files, Pane::Top) => &self.fsel,
            (Level::Status, Pane::Top) => &self.ssel,
            (Level::Status, Pane::Bottom) => return None,
            (Level::Files, Pane::Bottom) => &self.hsel,
        })
    }

    /// The filter an Enter kept on this level, top pane first: what `esc`
    /// drops before it steps back, and what the footer shows.
    pub(super) fn kept_query(&self) -> Option<(Pane, &str)> {
        [Pane::Top, Pane::Bottom].into_iter().find_map(|pane| {
            let text = self.pane_sel_ref(pane)?.query.text.as_str();
            (!text.is_empty()).then_some((pane, text))
        })
    }

    /// `esc` on a kept filter: the pane shows its whole scope again.
    fn drop_query(&mut self, pane: Pane) {
        if let Some(sel) = self.pane_sel(pane) {
            sel.query.open();
        }
        self.refilter(pane);
    }

    fn open_query(&mut self, pane: Pane) {
        let Some(sel) = self.pane_sel(pane) else {
            return;
        };
        let dropped = sel.query.open();
        self.editing = Some(pane);
        if dropped {
            self.refilter(pane);
        }
    }

    /// Re-apply a pane's scope and filter to what is already loaded.
    fn refilter(&mut self, pane: Pane) {
        match (self.level, pane) {
            (Level::Repos, Pane::Top) => self.rescope_repos(),
            (Level::Repos, Pane::Bottom) | (Level::Commits, Pane::Top) => self.rescope_log(),
            (Level::Remotes, Pane::Top) => self.rescope_remotes(),
            (Level::Scan, Pane::Top) => {}
            (Level::Remotes | Level::Scan | Level::Commits, Pane::Bottom)
            | (Level::Files, Pane::Top) => self.rescope_files(),
            (Level::Status, Pane::Top) => self.rescope_status(),
            (Level::Status, Pane::Bottom) => {}
            (Level::Files, Pane::Bottom) => self.rescope_hist(),
        }
        if pane == Pane::Top {
            self.pending = true;
        }
    }

    /// `r`: read this level again, the cursor kept on what it was on by name.
    fn refresh(&mut self) {
        match self.level {
            Level::Repos => {
                self.rsel.restore = self.rsel.idx().map(|i| self.repos[i].name.clone());
                let found = crate::repos::find(&self.base, self.depth);
                self.load_repos(found);
                self.pending = true;
            }
            Level::Remotes => {
                self.reread_remotes();
                self.request_log();
            }
            Level::Scan | Level::Commits | Level::Files => {
                self.load_remotes();
                self.request_log();
            }
            Level::Status => self.request_status(),
        }
    }

    /// Esc: step back one level, or quit if this is where we came in.
    fn back(&mut self) -> bool {
        match self.level.back(self.start) {
            Some(prev) if self.level > self.start => {
                self.level = prev;
                false
            }
            _ => true,
        }
    }
}

/// Starting stop on the repo scope slider for `stowe irepos --changed`.
pub fn repo_scope(changed: bool) -> usize {
    usize::from(changed)
}
