//! stowe - git for files, any remote.
//!
//! A git-shaped CLI for versioning large/binary files. Linear history (one
//! "main", no branches), content-addressed dedup, and a pluggable remote that
//! is just a dumb file store. See the module docs for the on-disk layout.

/// `print!` for a command's output, ending the process quietly once the
/// reader of stdout has gone (`stowe log | head`) where `print!` panics.
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::emit(format_args!($($arg)*))
    };
}

/// `println!` the way `out!` is `print!`.
macro_rules! outln {
    () => {
        $crate::emit(format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::emit(format_args!("{}\n", format_args!($($arg)*)))
    };
}

mod app;
mod audio;
mod commands;
mod diff;
mod errors;
mod history;
mod ignore;
mod kept;
mod mirror;
mod model;
mod names;
mod paths;
mod prompt;
mod remote;
mod repo;
mod repos;
mod scan;
mod selfcmd;
mod time;
mod tui;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use commands::remote::RemoteCmd;

/// Shown at the bottom of `stowe --help`: the distinctions the command list
/// can't convey - that a remote is either a playable mirror or a blob backup,
/// and which paths every scan skips - and then what a script can expect from
/// stdout.
const AFTER: &str = concat!(
    "Every remote is one of two shapes:
  mirror   real, playable folders - a drive or phone you can browse & play
  backup   deduped content-addressed blobs - S3, or a space-saving archive
Local remotes default to mirror, s3:// to backup; set it with `remote add --format`.

A `.stoweignore` at the repo root keeps paths out of every scan, local and
remote: bare names match anywhere, a trailing `/` means directories only, and a
pattern with a `/` is anchored at the root.

Every command prints for people rather than for a pipe, but for `status -s`,
and a failure names itself on stderr and exits non-zero; `search` finding
nothing is one. `status -s` prints one line in a fixed form for a prompt or a
script: +N added, -N removed, ~N changed, ⇄N moved since the last commit and
↑N commits a remote lacks, in that order, each left out at zero.
`repos` marks ✚N files changed since the last commit and ↑N commits a remote
lacks, then each remote as `in sync`, `N behind` (lacks N commits), `never
pushed` (nothing here says what it holds) or `diverged` (at a commit this repo
lacks: `stowe pull` it, or `stowe push --force` over it).
`search` names what a commit did to a file: added, changed, moved or removed.
Run `stowe <command> --help` for a command's details.",
    "\n\n",
    env!("CARGO_PKG_REPOSITORY"),
    "\ncontributors: ",
    env!("CARGO_PKG_AUTHORS"),
);

/// `-V` stays a bare version string for scripts; `--version` spells out the
/// license, where it lives, and who's contributed. Every field comes from
/// Cargo.toml, so none of it can drift from the manifest.
const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\n",
    env!("CARGO_PKG_LICENSE"),
    "  ",
    env!("CARGO_PKG_REPOSITORY"),
    "\ncontributors: ",
    env!("CARGO_PKG_AUTHORS"),
);

#[derive(Parser)]
#[command(
    name = "stowe",
    version,
    long_version = LONG_VERSION,
    about,
    after_help = AFTER,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new repo (.stowe/) in the current folder
    Init,
    /// Show what changed since the last commit
    ///   -s                        one line for a shell prompt: +added -removed ~changed ⇄moved ↑unpushed
    #[command(verbatim_doc_comment)]
    Status {
        /// Print one line for a shell prompt instead: `+N -N ~N ⇄N ↑N`, zero counts left out, and an empty line when clean and pushed
        #[arg(short, long)]
        short: bool,
    },
    /// Stage files for the next commit  [PATHS]...
    ///   -A                        stage the entire working tree
    #[command(verbatim_doc_comment)]
    Add {
        /// Files or directories to stage. Omit and pass `-A` to stage the whole tree.
        paths: Vec<std::path::PathBuf>,
        /// Stage the entire working tree.
        #[arg(short = 'A', long)]
        all: bool,
    },
    /// Unstage paths, or everything when none are named (files untouched)  [PATHS]...
    Unstage {
        /// Files or directories to put back as the last commit has them. Omit to drop
        /// everything staged.
        paths: Vec<std::path::PathBuf>,
    },
    /// Record the staged snapshot as a commit
    ///   -m MSG                    what it is (without it, your editor opens to write it)
    ///   -a                        stage the entire working tree first, new files included
    #[command(verbatim_doc_comment)]
    Commit {
        /// What this snapshot is, shown by `stowe log`. Without it, `$VISUAL`, else
        /// `$EDITOR`, else `vi` opens to write it, which needs a terminal
        #[arg(short = 'm', long)]
        message: Option<String>,
        /// Stage the entire working tree first, as `stowe add -A` does, new files included
        #[arg(short = 'a', long)]
        all: bool,
    },
    /// Show commit history (newest first)
    Log,
    /// Manage remotes - no subcommand lists them  [COMMAND]
    ///   remote add <NAME> <URL>   add or update a remote
    ///     --format mirror|backup  on-disk shape (default: local -> mirror)
    ///     --mount CMD             command that mounts it, run when unreachable
    ///   remote list               list them, the same as bare `remote`
    ///   remote remove <NAME>      forget one here, never touching it
    #[command(verbatim_doc_comment)]
    Remote {
        /// Accepted for git muscle memory; stowe always shows URLs anyway.
        #[arg(short, long)]
        verbose: bool,
        #[command(subcommand)]
        cmd: Option<RemoteCmd>,
    },
    /// Sync remote(s) to the latest commit  [REMOTES]...
    ///   --force                   push over changes made there outside this repo, or into an empty folder
    #[command(verbatim_doc_comment)]
    Push {
        /// Remotes to push to. Omit for `origin`; list several to fan out.
        remotes: Vec<String>,
        /// Overwrite what changed on the remote outside this repo: files changed
        /// on a mirror by hand, or commits pushed there from somewhere else. Also
        /// pushes into an empty folder never pushed to, which a drive not mounted
        /// there leaves behind, and which a push otherwise refuses.
        #[arg(long)]
        force: bool,
    },
    /// Rebuild the working tree from a remote  [REMOTE]
    Pull {
        /// The remote to rebuild from
        #[arg(default_value = "origin")]
        remote: String,
    },
    /// Pull a mirror's by-hand changes into local (remote -> local)  [REMOTE]
    Adapt {
        /// The mirror remote to adopt changes from
        #[arg(default_value = "origin")]
        remote: String,
    },
    /// Recover committed file(s) from a remote  [PATHS]...
    ///   -A                        restore the whole snapshot
    ///   --from C                  the version from commit C (default: HEAD)
    ///   --remote R                which remote to fetch from (default: origin)
    #[command(verbatim_doc_comment)]
    Restore {
        /// Files to restore. Omit and pass `-A` for the whole snapshot.
        paths: Vec<std::path::PathBuf>,
        /// Restore every file in the target commit.
        #[arg(short = 'A', long)]
        all: bool,
        /// Restore the version from this commit (hash or unique prefix) instead
        /// of HEAD.
        #[arg(long)]
        from: Option<String>,
        /// Remote to fetch object bytes from.
        #[arg(long, default_value = "origin")]
        remote: String,
    },
    /// Flip a remote between mirror and backup, in place  [REMOTE]
    ///   --to mirror|backup        target format (default: the other one)
    #[command(verbatim_doc_comment)]
    Convert {
        /// The remote to convert
        #[arg(default_value = "origin")]
        remote: String,
        /// Target format. Omit to flip to the other one.
        #[arg(long, value_parser = ["mirror", "backup"])]
        to: Option<String>,
    },
    /// Every commit that touched a file whose name matches  <PATTERN>
    Search {
        /// Part of a file name, old or new; every word has to be in it
        pattern: String,
    },
    /// Every stowe repo under a folder, its changes and its remotes  [PATH]
    ///   -d N                      how many folders down to look (3)
    #[command(verbatim_doc_comment)]
    Repos {
        /// Where to look, the current folder unless given
        #[arg(default_value = ".")]
        path: std::path::PathBuf,
        /// How many folders down to look for repos
        #[arg(short, long, default_value_t = 3)]
        depth: usize,
    },
    /// Manage stowe itself: `self update` reinstalls, `self check` looks for a newer release
    #[command(name = "self", subcommand)]
    Selfie(selfcmd::Cmd),
    /// Interactive status (TUI)
    ///   this repo's changes, each text file's diff · ↵ git difftool, s/u stage, c commits
    #[command(verbatim_doc_comment)]
    Istatus,
    /// Interactive history (TUI)
    ///   commits, their changes, each file's versions · ↵ restores one
    #[command(verbatim_doc_comment)]
    Ilog,
    /// Interactive remotes (TUI)
    ///   every remote and what a push would do · ↵ pushes, c/e/d manage
    #[command(verbatim_doc_comment)]
    Iremote,
    /// Interactive name search (TUI)
    ///   type part of a file name · ↵ opens the commit that touched it
    #[command(verbatim_doc_comment)]
    Iscan,
    /// Interactive repo explorer (TUI)  [PATH]
    ///   every repo under PATH · ↵ opens its history
    ///   --changed                 start on the repos with changes
    ///   -d N                      how many folders down to look (3)
    #[command(verbatim_doc_comment)]
    Irepos {
        /// Where to look, the current folder unless given
        #[arg(default_value = ".")]
        path: std::path::PathBuf,
        /// How many folders down to look for repos
        #[arg(short, long, default_value_t = 3)]
        depth: usize,
        /// Start on the repos with changes since their last commit
        #[arg(long)]
        changed: bool,
    },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {}", errors::words(&e));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // The command list is rendered here rather than by clap, which has no way
    // to give one subcommand a different heading from another. Everything in
    // it still comes from clap's own metadata.
    let mut built = Cli::command();
    built.build();
    let template = help_template(&built);
    let matches = Cli::command().help_template(template).get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    match cli.cmd {
        Cmd::Init => commands::init::run(),
        Cmd::Status { short } => commands::status::run(short),
        Cmd::Add { paths, all } => commands::add::run(paths, all),
        Cmd::Unstage { paths } => commands::unstage::run(paths),
        Cmd::Commit { message, all } => commands::commit::run(message.as_deref(), all),
        Cmd::Log => commands::log::run(),
        Cmd::Remote { cmd, .. } => commands::remote::run(cmd),
        Cmd::Push { remotes, force } => commands::push::run(&remotes, force),
        Cmd::Pull { remote } => commands::pull::run(&remote),
        Cmd::Restore {
            paths,
            all,
            from,
            remote,
        } => commands::restore::run(paths, all, from.as_deref(), &remote),
        Cmd::Adapt { remote } => commands::adapt::run(&remote),
        Cmd::Convert { remote, to } => commands::convert::run(&remote, to.as_deref()),
        Cmd::Search { pattern } => commands::search::run(&pattern),
        Cmd::Repos { path, depth } => commands::repos::run(&path, depth),
        Cmd::Selfie(cmd) => selfcmd::run(cmd),
        Cmd::Istatus => commands::istatus::run(),
        Cmd::Ilog => commands::ilog::run(),
        Cmd::Iremote => commands::iremote::run(),
        Cmd::Iscan => commands::iscan::run(),
        Cmd::Irepos {
            path,
            depth,
            changed,
        } => commands::irepos::run(&path, depth, changed),
    }
}

/// `stowe --help` with the commands split in two: the ones that print and
/// exit, then the ones that take over the terminal, each the twin of one
/// before it (`status`/`istatus`, `log`/`ilog`, `remote`/`iremote`,
/// `search`/`iscan`, `repos`/`irepos`).
fn help_template(cmd: &clap::Command) -> String {
    let subs: Vec<&clap::Command> = cmd.get_subcommands().filter(|c| !c.is_hide_set()).collect();
    // One column across both groups, so the two lists line up.
    let width = subs
        .iter()
        .map(|c| c.get_name().chars().count())
        .max()
        .unwrap_or(0);
    let (tui, plain): (Vec<&&clap::Command>, Vec<&&clap::Command>) =
        subs.iter().partition(|c| is_interactive(c));
    let styles = cmd.get_styles();
    let header = styles.get_header();
    let literal = styles.get_literal();
    let mut list = String::new();
    for (heading, group) in [("Commands:", plain), ("Interactive (TUI):", tui)] {
        list.push_str(&format!("{header}{heading}{header:#}\n"));
        for c in group {
            list.push_str(&entry(c, width, literal));
        }
        list.push('\n');
    }
    format!(
        "{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{list}{header}Options:{header:#}\n{{options}}{{after-help}}\n"
    )
}

/// One command's row: its name, then its description, whose extra lines are
/// indented to the same column.
fn entry(cmd: &clap::Command, width: usize, literal: &clap::builder::styling::Style) -> String {
    let about = cmd.get_about().map(ToString::to_string).unwrap_or_default();
    let mut lines = about.lines();
    let name = cmd.get_name();
    // Padded outside its styling: escape codes are zero-width to a reader and
    // full-width to `{:<width$}`.
    let pad = " ".repeat(width - name.chars().count());
    let mut row = format!(
        "  {literal}{name}{literal:#}{pad}  {}\n",
        lines.next().unwrap_or("")
    );
    for line in lines {
        row.push_str(&format!("  {:<width$}  {line}\n", ""));
    }
    row
}

/// A command is interactive when its name starts with `i`, which is how the
/// interactive ones are named.
fn is_interactive(cmd: &clap::Command) -> bool {
    cmd.get_name().starts_with('i') && cmd.get_name() != "init"
}

/// What `out!` and `outln!` expand to.
fn emit(args: std::fmt::Arguments) {
    use std::io::Write;
    match std::io::stdout().lock().write_fmt(args) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => std::process::exit(0),
        Err(_) => {
            eprintln!("error: could not write the output; is the disk it goes to full?");
            std::process::exit(1);
        }
    }
}
