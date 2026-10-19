//! Reading history as changes: what each commit did to which path, a walk over
//! every commit doing that, and the name search `search` and `iscan` share.

use anyhow::Result;

use crate::model::Commit;
use crate::repo::Repo;
use crate::scan::{self, Diff};

/// What a commit did to one path, in the order the changes pane groups them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Kind {
    Added,
    Modified,
    Moved,
    Removed,
}

impl Kind {
    /// The mark `stowe log`'s summary line uses for it.
    pub fn mark(self) -> char {
        match self {
            Kind::Added => '+',
            Kind::Modified => '~',
            Kind::Moved => '⇄',
            Kind::Removed => '-',
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Kind::Added => "added",
            Kind::Modified => "changed",
            Kind::Moved => "moved",
            Kind::Removed => "removed",
        }
    }
}

/// One path a commit touched. `from` is where a move came from.
#[derive(Clone, Debug)]
pub struct Change {
    pub kind: Kind,
    pub path: String,
    pub from: Option<String>,
}

impl Change {
    /// The path as one line: `{old => new}` around the part a move changed.
    pub fn shown(&self) -> String {
        match &self.from {
            Some(from) => moved(from, &self.path),
            None => self.path.clone(),
        }
    }

    /// Whether every term of `query` is in this change's path, or in the path
    /// a move came from, ignoring case.
    pub fn matches(&self, query: &str) -> bool {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let hit = |p: &str| {
            let p = p.to_lowercase();
            terms.iter().all(|t| p.contains(t.as_str()))
        };
        !terms.is_empty() && (hit(&self.path) || self.from.as_deref().is_some_and(hit))
    }
}

/// `{old => new}` around what differs between two paths, sharing the
/// folders and the extension they have in common, the way git prints a rename.
pub fn moved(from: &str, to: &str) -> String {
    let (f, t): (Vec<char>, Vec<char>) = (from.chars().collect(), to.chars().collect());
    // Cut at a `/` so a folder is shared whole or not at all.
    let mut head = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    while head > 0 && f[head - 1] != '/' {
        head -= 1;
    }
    let most = (f.len() - head).min(t.len() - head);
    let mut tail = (0..most)
        .take_while(|&k| f[f.len() - 1 - k] == t[t.len() - 1 - k])
        .count();
    while tail > 0 && f[f.len() - tail] != '/' && f[f.len() - tail] != '.' {
        tail -= 1;
    }
    let s = |c: &[char]| c.iter().collect::<String>();
    if head == 0 && tail == 0 {
        return format!("{from} => {to}");
    }
    format!(
        "{}{{{} => {}}}{}",
        s(&f[..head]),
        s(&f[head..f.len() - tail]),
        s(&t[head..t.len() - tail]),
        s(&f[f.len() - tail..])
    )
}

/// A diff as one list, in the order `status` prints it: removals, changes,
/// moves, additions, each sorted by path.
pub fn changes(d: &Diff) -> Vec<Change> {
    let one = |kind, p: &String| Change {
        kind,
        path: p.clone(),
        from: None,
    };
    let mut out: Vec<Change> = d.removed.iter().map(|p| one(Kind::Removed, p)).collect();
    out.extend(d.modified.iter().map(|p| one(Kind::Modified, p)));
    out.extend(d.moved.iter().map(|(from, to)| Change {
        kind: Kind::Moved,
        path: to.clone(),
        from: Some(from.clone()),
    }));
    out.extend(d.added.iter().map(|p| one(Kind::Added, p)));
    out
}

/// Walk history from HEAD back to the first commit, handing `visit` each
/// commit with what it changed, until `visit` returns false. Each commit is
/// read once, its manifest kept only until its parent has been diffed.
pub fn walk(repo: &Repo, mut visit: impl FnMut(&str, &Commit, Vec<Change>) -> bool) -> Result<()> {
    let Some(mut hash) = repo.head()? else {
        return Ok(());
    };
    let mut commit = repo.read_commit(&hash)?;
    loop {
        let parent = match &commit.parent {
            Some(p) => Some((p.clone(), repo.read_commit(p)?)),
            None => None,
        };
        let empty = Vec::new();
        let before = parent.as_ref().map_or(&empty, |(_, c)| &c.files);
        let changed = changes(&scan::diff(before, &commit.files));
        if !visit(&hash, &commit, changed) {
            return Ok(());
        }
        match parent {
            Some((h, c)) => {
                hash = h;
                commit = c;
            }
            None => return Ok(()),
        }
    }
}

/// The paths `path` has had: itself, and every path a move anywhere in history
/// took it to or from. Moves come in `(from, to)` pairs, as `walk` hands them.
pub fn aliases(moves: &[(String, String)], path: &str) -> Vec<String> {
    let mut names = vec![path.to_string()];
    let mut grew = true;
    while grew {
        grew = false;
        for (from, to) in moves {
            for (have, add) in [(from, to), (to, from)] {
                if names.contains(have) && !names.contains(add) {
                    names.push(add.clone());
                    grew = true;
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::{Change, Kind, aliases, moved};

    #[test]
    fn a_move_shows_only_the_part_that_changed() {
        assert_eq!(
            moved("Music/old song.flac", "Music/new song.flac"),
            "Music/{old song => new song}.flac"
        );
        assert_eq!(moved("a/x.mp3", "b/x.mp3"), "{a => b}/x.mp3");
        assert_eq!(moved("one", "two"), "one => two");
    }

    #[test]
    fn a_search_finds_a_file_by_the_name_it_moved_away_from() {
        // `iscan` is for "where did that photo go": the old name is the one
        // a person remembers.
        let c = Change {
            kind: Kind::Moved,
            path: "Photos/2024 Lisbon/beach.jpg".into(),
            from: Some("Photos/2024/IMG_0042.jpg".into()),
        };
        assert!(c.matches("img_0042"), "the old name is searched too");
        assert!(
            c.matches("lisbon beach"),
            "every term has to be in the path"
        );
        assert!(
            !c.matches("lisbon img"),
            "terms are not split across the two names"
        );
        assert!(!c.matches("   "), "a blank query matches nothing");
    }

    #[test]
    fn a_file_keeps_its_history_across_every_move() {
        let moves = vec![
            ("a.flac".to_string(), "b.flac".to_string()),
            ("x.flac".to_string(), "y.flac".to_string()),
            ("b.flac".to_string(), "Album/c.flac".to_string()),
        ];
        let mut names = aliases(&moves, "Album/c.flac");
        names.sort();
        assert_eq!(names, ["Album/c.flac", "a.flac", "b.flac"]);
    }
}
