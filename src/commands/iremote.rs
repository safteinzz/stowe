//! `stowe iremote`: the remotes and what a push to each would do, in a TUI. See `app/` for the screen.

use anyhow::{Result, bail};
use std::io::{self, IsTerminal};

use crate::app::App;
use crate::repo::Repo;

pub fn run() -> Result<()> {
    if !io::stdout().is_terminal() {
        bail!(
            "`stowe iremote` needs an interactive terminal; `stowe remote` prints the same for a pipe"
        );
    }
    let repo = Repo::find()?;
    App::at_remotes(repo.root).run("iremote");
    Ok(())
}
