//! `stowe commit`: record the staged snapshot as the new head.

use anyhow::{Result, anyhow, bail};

use crate::model::Commit;
use crate::model::{Manifest, short};
use crate::repo::Repo;
use crate::scan;
use crate::time::now;

pub fn run(message: Option<&str>, all: bool) -> Result<()> {
    let repo = Repo::find()?;
    if all {
        match crate::commands::add::stage_all(&repo)? {
            Some((_, d)) => crate::names::warn_unportable(&d),
            None => bail!("nothing to commit: the working tree matches the last commit"),
        }
    }
    let message = match message {
        Some(m) => m.to_string(),
        None => edit_message(&repo, &staged(&repo)?.1)?,
    };
    let (hash, d) = commit(&repo, &message)?;
    outln!("committed {} \"{message}\"", short(&hash));
    scan::print_diff(&d);
    outln!("\n(remember: `stowe push` to back the file contents up to a remote)");
    Ok(())
}

/// Record what is staged as the new head. Returns its hash and what changed.
pub fn commit(repo: &Repo, message: &str) -> Result<(String, scan::Diff)> {
    let (staged, d) = staged(repo)?;
    let commit = Commit {
        parent: repo.head()?,
        message: message.to_string(),
        time: now(),
        files: staged,
    };
    let hash = repo.write_commit(&commit)?;
    repo.set_head(&hash)?;
    repo.clear_index()?;
    Ok((hash, d))
}

/// What is staged and how it differs from HEAD, failing when a commit of it
/// would record nothing.
fn staged(repo: &Repo) -> Result<(Manifest, scan::Diff)> {
    let staged = repo
        .read_index()?
        .ok_or_else(|| anyhow!("nothing staged: run `stowe add -A` first, or commit with `-a`"))?;
    let d = scan::diff(&repo.head_manifest()?, &staged);
    if d.is_empty() {
        bail!("staged snapshot is identical to the last commit; nothing to commit");
    }
    Ok((staged, d))
}

/// The message written in an editor, as git asks for one: `$VISUAL`, else
/// `$EDITOR`, else `vi`, run through `sh` on `.stowe/COMMIT_EDITMSG` listing
/// `d`, the name editors give git's commit highlighting to.
/// Lines starting with `#` are dropped, and an empty message, an editor that
/// fails or no terminal to edit in commits nothing.
fn edit_message(repo: &Repo, d: &scan::Diff) -> Result<String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!(
            "no commit message: pass `-m <MESSAGE>`, or run it in a terminal to write one in \
             your editor"
        );
    }
    let mut text = String::from(
        "\n# What this snapshot is. Lines starting with '#' are dropped, and an empty\n\
         # message commits nothing.\n#\n# To be committed:\n",
    );
    let mut list =
        |mark: char, p: &str| text += &format!("#   {mark} {}\n", crate::names::display(p));
    d.added.iter().for_each(|p| list('+', p));
    d.modified.iter().for_each(|p| list('~', p));
    d.moved
        .iter()
        .for_each(|(from, to)| list('⇄', &format!("{from} -> {to}")));
    d.removed.iter().for_each(|p| list('-', p));
    let path = repo.dir.join("COMMIT_EDITMSG");
    fs_err::write(&path, text)?;

    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "vi".to_string());
    // `"$@"` hands the path over as one word, so an editor line with flags still works.
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(&editor)
        .arg(&path)
        .status();
    let status = match status {
        Ok(s) => s,
        Err(e) => bail!(
            "could not start `{editor}` for the message: {}",
            crate::errors::io_words(&e)
        ),
    };
    if !status.success() {
        let how = status
            .code()
            .map_or("on a signal".to_string(), |c| format!("with code {c}"));
        bail!("`{editor}` exited {how}, so nothing was committed");
    }
    let written = fs_err::read_to_string(&path)?;
    let _ = fs_err::remove_file(&path);
    let message = written
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if message.is_empty() {
        bail!("nothing committed: the message was empty");
    }
    Ok(message)
}
