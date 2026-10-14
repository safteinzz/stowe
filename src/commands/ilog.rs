//! `stowe ilog`: the history, each commit's changes and each file's versions, in a TUI. See `app/` for the screen.

use anyhow::{Result, bail};
use std::io::{self, IsTerminal};

use crate::app::App;
use crate::repo::Repo;

pub fn run() -> Result<()> {
    if !io::stdout().is_terminal() {
        bail!("`stowe ilog` needs an interactive terminal; `stowe log` prints the same for a pipe");
    }
    let repo = Repo::find()?;
    App::at_commits(repo.root).run("ilog");
    Ok(())
}
