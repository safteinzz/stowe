//! Loading and highlighting a diff off the UI thread.
//!
//! `git diff` on a big file plus the syntect pass is long enough to stall a held
//! `j`, and both the four-level browser and `istatus` move a cursor over files.
//! They differ only in which git command produces the raw text, so that is the
//! one thing a caller passes in.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::tui::highlight::{DiffContext, RenderedDiff, prepare_diff};

/// How long that may take before a pane admits it is loading. A wait nobody
/// notices says nothing, or the word blinks on every keypress in a small repo.
const SLOW: Duration = Duration::from_millis(120);

pub struct DiffFeed {
    tx: Sender<(u64, RenderedDiff)>,
    rx: Receiver<(u64, RenderedDiff)>,
    /// The newest request. An answer carrying anything else belongs to a row the
    /// cursor has already left.
    seq: u64,
    loading: bool,
    since: Option<Instant>,
}

impl Default for DiffFeed {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        DiffFeed {
            tx,
            rx,
            seq: 0,
            loading: false,
            since: None,
        }
    }
}

impl DiffFeed {
    /// Ask for a diff. `raw` runs on the worker and returns what git said,
    /// paired with where to read the file's two sides when a hunk starts too far
    /// down the file to highlight on its own.
    pub fn request(&mut self, raw: impl FnOnce() -> (String, DiffContext) + Send + 'static) {
        self.seq += 1;
        self.loading = true;
        self.since = Some(Instant::now());
        let (seq, tx) = (self.seq, self.tx.clone());
        std::thread::spawn(move || {
            let (text, ctx) = raw();
            let _ = tx.send((seq, prepare_diff(&text, ctx)));
        });
    }

    /// Nothing is coming: the cursor is on a row with no diff to show.
    pub fn idle(&mut self) {
        self.seq += 1;
        self.loading = false;
        self.since = None;
    }

    /// The newest prepared diff, if one has arrived since the last call.
    pub fn take(&mut self) -> Option<RenderedDiff> {
        let mut newest = None;
        while let Ok((seq, prepared)) = self.rx.try_recv() {
            if seq == self.seq {
                newest = Some(prepared);
                self.loading = false;
            }
        }
        newest
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn slow(&self) -> bool {
        self.loading && self.since.is_some_and(|t| t.elapsed() >= SLOW)
    }
}
