//! Many repos at once: finding every stowe repo under a folder, and the state
//! `repos` and `irepos` show for each.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::repo::Repo;
use crate::scan;

/// Every stowe repo under `base`, at most `depth` folders down, sorted by path.
/// A repo's own tree is not searched further, and neither is a mirror's: both
/// carry a `.stowe/`, but only a repo has a `HEAD` in it. When there are none
/// and `base` sits inside a repo, that repo is the answer.
pub fn find(base: &Path, depth: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    look(base, depth, &mut found);
    if found.is_empty()
        && let Ok(repo) = Repo::find_from(base)
    {
        found.push(repo.root);
    }
    found.sort();
    found
}

fn look(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if dir.join(".stowe").join("HEAD").is_file() {
        found.push(dir.to_path_buf());
        return;
    }
    if depth == 0 || dir.join(".stowe").is_dir() {
        return;
    }
    let Ok(rd) = fs_err::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        if !hidden && entry.file_type().is_ok_and(|t| t.is_dir()) {
            look(&entry.path(), depth - 1, found);
        }
    }
}

/// How far a remote is behind this repo's HEAD.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Behind {
    /// Nothing here says what it holds.
    Never,
    /// This many commits are not on it yet.
    By(usize),
    /// It is at a commit this repo does not have.
    Elsewhere,
}

impl Behind {
    /// How a remote's row says it, `in sync` for none behind.
    pub fn words(self) -> String {
        match self {
            Behind::Never => "never pushed".to_string(),
            Behind::By(0) => "in sync".to_string(),
            Behind::By(n) => format!("{n} behind"),
            Behind::Elsewhere => "diverged".to_string(),
        }
    }
}

/// Where remote `name` stands against `history` (newest first), from the record
/// of the commit it was last known to be at.
pub fn behind(repo: &Repo, name: &str, history: &[String]) -> Behind {
    match repo.remote_head(name).ok().flatten() {
        None => Behind::Never,
        Some(h) => history
            .iter()
            .position(|c| *c == h)
            .map_or(Behind::Elsewhere, Behind::By),
    }
}

/// Hashes from HEAD back to the first commit, without reading any manifest
/// twice: a commit's parent is the only field this needs.
pub fn hashes(repo: &Repo) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = repo.head()?;
    while let Some(h) = cur {
        cur = repo.read_commit(&h)?.parent;
        out.push(h);
    }
    Ok(out)
}

/// One repo as `repos` reports it.
pub struct State {
    /// Where it sits under the folder that was searched.
    pub name: String,
    pub commits: usize,
    /// Paths that differ from the last commit, staged or not.
    pub changed: usize,
    pub remotes: Vec<(String, Behind)>,
}

impl State {
    /// The commits `↑` marks, see [`unpushed`].
    pub fn unpushed(&self) -> usize {
        unpushed(&self.remotes, self.commits)
    }

    /// The commits `⇡` marks, see [`elsewhere`].
    pub fn elsewhere(&self) -> usize {
        elsewhere(&self.remotes, self.commits)
    }
}

/// How many of `commits` a remote lacks: all of them when nothing says what
/// it holds or it is at a commit this repo does not have.
fn lacks(b: Behind, commits: usize) -> usize {
    match b {
        Behind::By(n) => n,
        Behind::Never | Behind::Elsewhere => commits,
    }
}

/// What `↑` marks: the commits `origin` lacks, which a bare `stowe push` sends,
/// or with no `origin` the commits on no remote.
pub fn unpushed(remotes: &[(String, Behind)], commits: usize) -> usize {
    match remotes.iter().find(|(name, _)| name == "origin") {
        Some((_, b)) => lacks(*b, commits),
        None => remotes
            .iter()
            .map(|(_, b)| lacks(*b, commits))
            .min()
            .unwrap_or(commits),
    }
}

/// What `⇡` marks: the most commits any remote but `origin` lacks.
pub fn elsewhere(remotes: &[(String, Behind)], commits: usize) -> usize {
    remotes
        .iter()
        .filter(|(name, _)| name != "origin")
        .map(|(_, b)| lacks(*b, commits))
        .max()
        .unwrap_or(0)
}

/// Read `path`'s state. This walks its working tree, hashing only what changed
/// since the last commit, so a tree of repos is read one per thread.
pub fn state(path: &Path, base: &Path) -> Result<State> {
    let repo = Repo::at(path.to_path_buf());
    let history = hashes(&repo)?;
    let head = repo.head_manifest()?;
    let working = scan::scan(&repo, &head, false)?;
    let d = scan::diff(&head, &working);
    let changed = d.added.len() + d.removed.len() + d.modified.len() + d.moved.len();
    let remotes = repo
        .config()?
        .remotes
        .keys()
        .map(|name| (name.clone(), behind(&repo, name, &history)))
        .collect();
    Ok(State {
        name: relative_name(path, base),
        commits: history.len(),
        changed,
        remotes,
    })
}

/// A repo's path under the folder searched (`photos/2024`), or its folder's own
/// name when it is that folder or sits above it.
pub fn relative_name(repo: &Path, base: &Path) -> String {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let (repo, base) = (canon(repo), canon(base));
    repo.strip_prefix(&base)
        .ok()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            repo.file_name()
                .unwrap_or(repo.as_os_str())
                .to_string_lossy()
                .into_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::find;

    #[test]
    fn a_mirror_on_disk_is_never_listed_as_a_repo() {
        // A mirror carries a `.stowe/` too, and listing a drive as a repo
        // would offer to commit into a backup.
        let tmp = tempfile::tempdir().expect("temp dir");
        let base = tmp.path();
        let mk = |p: &str| fs_err::create_dir_all(base.join(p)).expect("fixture dir");
        mk("music/.stowe/commits");
        fs_err::write(base.join("music/.stowe/HEAD"), "").expect("HEAD");
        mk("drive/.stowe/refs");
        fs_err::write(base.join("drive/.stowe/refs/main"), "abc").expect("ref");
        mk("music/inner/.stowe");
        fs_err::write(base.join("music/inner/.stowe/HEAD"), "").expect("HEAD");

        let found = find(base, 3);
        assert_eq!(
            found,
            [base.join("music")],
            "only the repo, not the mirror nor a repo inside a repo's tree"
        );
    }
}
