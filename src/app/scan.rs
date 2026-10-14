//! The scan level: a name typed into the bar, every commit that added,
//! changed, moved or removed a file it matches, and that commit's changes below.

use super::{App, Level};
use crate::history::Change;

/// One file a commit touched whose name matched: `at` is the commit's place in
/// the log.
pub struct Hit {
    pub at: usize,
    pub change: Change,
}

impl App {
    /// `↵` in the bar. Runs at once when the history is in, and once it is
    /// otherwise.
    pub(super) fn run_scan(&mut self) {
        self.searched = self.bar.text.trim().to_string();
        self.typing = self.searched.is_empty();
        if !self.cfeed.loading {
            self.scan();
        }
    }

    pub(super) fn scan_waiting(&mut self) {
        if !self.searched.is_empty() {
            self.scan();
        }
    }

    fn scan(&mut self) {
        let query = self.searched.clone();
        self.hits = self
            .log
            .iter()
            .enumerate()
            .flat_map(|(at, c)| {
                c.changes
                    .iter()
                    .filter(|ch| ch.matches(&query))
                    .map(move |ch| Hit {
                        at,
                        change: ch.clone(),
                    })
            })
            .collect();
        self.xsel.show((0..self.hits.len()).collect());
        self.pending = true;
    }

    pub(super) fn hit(&self) -> Option<&Hit> {
        self.xsel.idx().map(|i| &self.hits[i])
    }

    /// The pane below a hit: its commit's changes, the cursor on the hit.
    pub(super) fn enter_hit(&mut self) {
        let Some((at, path)) = self.hit().map(|h| (h.at, h.change.path.clone())) else {
            self.files.clear();
            self.fsel.show(Vec::new());
            self.files_for.clear();
            return;
        };
        let hash = self.log[at].hash.clone();
        self.show_changes(at, &hash);
        if let Some(cur) = self
            .fsel
            .visible
            .iter()
            .position(|&i| self.files[i].path == path)
        {
            self.fsel.cur = cur;
            self.fsel.state.select(Some(cur));
        }
    }

    /// `↵` on a hit: its commit in the commits level, as `ilog` shows it.
    pub(super) fn open_hit(&mut self) {
        let Some(at) = self.hit().map(|h| h.at) else {
            return;
        };
        self.csel.query.clear();
        self.csel.scope = 0;
        self.rescope_log();
        if let Some(cur) = self.csel.visible.iter().position(|&i| i == at) {
            self.csel.cur = cur;
            self.csel.state.select(Some(cur));
        }
        self.level = Level::Commits;
    }
}
