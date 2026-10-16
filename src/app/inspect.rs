//! `K`: everything about the row under the cursor that its line has no room
//! for, in a reader over the panes.

use super::load::Side;
use super::{App, Level};
use crate::history::Kind;
use crate::model::{count, short};
use crate::repos::Behind;
use crate::time::format_time;
use crate::tui::widgets::Modal;

type Rows = Vec<(String, String)>;

fn row(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

impl App {
    pub(super) fn inspect(&mut self) {
        let found = match self.level {
            Level::Repos => self.inspect_repo(),
            Level::Remotes => self.inspect_remote(),
            Level::Status => self.inspect_status(),
            Level::Files => self.inspect_file(),
            Level::Scan => self
                .hit()
                .map(|h| h.at)
                .and_then(|at| self.inspect_commit(at)),
            Level::Commits => self.csel.idx().and_then(|at| self.inspect_commit(at)),
        };
        if let Some((title, rows, text)) = found {
            self.modal = Some(Modal::reader(title, &rows).with_text(&text));
        }
    }

    fn inspect_repo(&self) -> Option<(String, Rows, String)> {
        let r = &self.repos[self.rsel.idx()?];
        let path = r.path.canonicalize().unwrap_or_else(|_| r.path.clone());
        let mut rows = vec![row("path", crate::paths::short(&path))];
        match &r.state {
            None => rows.push(row("state", "still being read")),
            Some(Err(e)) => rows.push(row("state", format!("could not be read: {e}"))),
            Some(Ok(s)) => {
                rows.push(row("commits", s.commits.to_string()));
                rows.push(row(
                    "changed",
                    match s.changed {
                        0 => "nothing since the last commit".to_string(),
                        n => format!("{} since the last commit", count(n, "file", "files")),
                    },
                ));
                if s.remotes.is_empty() {
                    rows.push(row("remotes", "none"));
                }
                for (name, b) in &s.remotes {
                    rows.push(row(name, b.words()));
                }
            }
        }
        Some((format!("repo {}", r.name), rows, String::new()))
    }

    fn inspect_remote(&self) -> Option<(String, Rows, String)> {
        let r = self.remote()?;
        let mut rows = vec![
            row("url", crate::paths::short_url(&r.url)),
            row("format", r.format.name()),
            row("now", r.reach.word()),
        ];
        if let Some(m) = &r.mount {
            rows.push(row("mount", m.as_str()));
        }
        match &r.last {
            None => rows.push(row("pushed", "never, from here")),
            Some(h) => {
                let when = self
                    .log
                    .iter()
                    .find(|c| &c.hash == h)
                    .map(|c| format!(", {}", format_time(c.time)))
                    .unwrap_or_default();
                rows.push(row("pushed", format!("{}{when}", short(h))));
            }
        }
        if let Some(b) = self.behind(r) {
            rows.push(row(
                "behind",
                match b {
                    Behind::By(0) => "nothing, it has HEAD".to_string(),
                    other => other.words(),
                },
            ));
        }
        Some((format!("remote {}", r.name), rows, String::new()))
    }

    fn inspect_commit(&self, at: usize) -> Option<(String, Rows, String)> {
        let c = self.log.get(at)?;
        let counts = format!(
            "+{} ~{} ⇄{} -{}",
            c.count(Kind::Added),
            c.count(Kind::Modified),
            c.count(Kind::Moved),
            c.count(Kind::Removed)
        );
        let rows = vec![
            row("hash", c.hash.as_str()),
            row("date", format_time(c.time)),
            row("files", c.files.to_string()),
            row("changed", counts),
            row(
                "remote",
                if self.unpushed(at) {
                    "on no remote yet"
                } else {
                    "pushed"
                },
            ),
        ];
        Some((
            format!("commit {}", short(&c.hash)),
            rows,
            c.message.clone(),
        ))
    }

    fn inspect_file(&self) -> Option<(String, Rows, String)> {
        let f = self.file()?;
        let mut rows = vec![row("path", f.path.as_str()), row("change", f.kind.word())];
        if let Some(from) = &f.from {
            rows.push(row("was", from.as_str()));
        }
        if let Some(c) = self.commit() {
            rows.push(row("commit", format!("{}  {}", short(&c.hash), c.message)));
        }
        Some((format!("file {}", leaf(&f.path)), rows, String::new()))
    }

    fn inspect_status(&self) -> Option<(String, Rows, String)> {
        let r = self.status_row()?;
        let side = match r.side {
            Side::Staged => "staged",
            Side::Unstaged => "not staged",
            Side::Untracked => "untracked",
        };
        let mut rows = vec![
            row("path", r.change.path.as_str()),
            row("change", r.change.kind.word()),
            row("state", side),
        ];
        if let Some(from) = &r.change.from {
            rows.push(row("was", from.as_str()));
        }
        Some((
            format!("file {}", leaf(&r.change.path)),
            rows,
            String::new(),
        ))
    }
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
