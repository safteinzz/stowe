//! `stowe iscan`: every commit that touched a file by a name you type, in a TUI. See `app/` for the screen.

use anyhow::{Result, bail};
use std::io::{self, IsTerminal};

use crate::app::App;
use crate::repo::Repo;

pub fn run() -> Result<()> {
    if !io::stdout().is_terminal() {
        bail!(
            "`stowe iscan` needs an interactive terminal; `stowe search <PATTERN>` prints the same for a pipe"
        );
    }
    let repo = Repo::find()?;
    App::at_scan(repo.root).run("iscan");
    Ok(())
}
