//! `stowe irepos`: every stowe repo under a folder, and drilling into one's
//! history, in a TUI. See `app/` for the screen.

use anyhow::{Result, bail};
use std::io::{self, IsTerminal};
use std::path::Path;

use crate::app::{App, repo_scope};

pub fn run(base: &Path, depth: usize, changed: bool) -> Result<()> {
    if !io::stdout().is_terminal() {
        bail!(
            "`stowe irepos` needs an interactive terminal; `stowe repos` prints the same for a pipe"
        );
    }
    match App::at_repos(base, depth, repo_scope(changed)) {
        Some(app) => {
            app.run("irepos");
            Ok(())
        }
        None => bail!(
            "no stowe repos in or under `{}` (looked {depth} levels down); run it inside one, \
             or point it at a folder that holds some",
            crate::paths::short(base)
        ),
    }
}
