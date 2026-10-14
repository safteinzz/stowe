//! `stowe istatus`: the working tree against the last commit, in a TUI. See `app/` for the screen.

use anyhow::{Result, bail};
use std::io::{self, IsTerminal};

use crate::app::App;
use crate::repo::Repo;

pub fn run() -> Result<()> {
    if !io::stdout().is_terminal() {
        bail!(
            "`stowe istatus` needs an interactive terminal; `stowe status` prints the same for a pipe"
        );
    }
    let repo = Repo::find()?;
    App::at_status(repo.root).run("istatus");
    Ok(())
}
