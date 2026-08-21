//! stowe - git for files, any remote.
//!
//! A git-shaped CLI for versioning large/binary files. Linear history (one
//! "main", no branches), content-addressed dedup, and a pluggable remote that
//! is just a dumb file store. See the module docs for the on-disk layout.

mod audio;
mod commands;
mod diff;
mod ignore;
mod mirror;
mod model;
mod names;
mod prompt;
mod remote;
mod repo;
mod scan;
mod selfcmd;
mod time;

use anyhow::Result;
use clap::{Parser, Subcommand};

use commands::remote::RemoteCmd;

/// Shown at the bottom of `stowe --help`: the one distinction the command list
/// can't convey - that a remote is either a playable mirror or a blob backup.
const REMOTES_NOTE: &str = concat!(
    "Every remote is one of two shapes:
  mirror   real, playable folders - a drive or phone you can browse & play
  backup   deduped content-addressed blobs - S3, or a space-saving archive
Local remotes default to mirror, s3:// to backup; set it with `remote add --format`.

A `.stoweignore` at the repo root keeps paths out of every scan, local and
remote: bare names match anywhere, a trailing `/` means directories only, and a
pattern with a `/` is anchored at the root.
Run `stowe <command> --help` for the full detail of any command.",
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
    after_help = REMOTES_NOTE,
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
    Status,
    /// Stage files for the next commit  <paths...>
    ///   -A          stage the entire working tree
    #[command(verbatim_doc_comment)]
    Add {
        /// Files or directories to stage. Omit and pass `-A` to stage the whole tree.
        paths: Vec<std::path::PathBuf>,
        /// Stage the entire working tree.
        #[arg(short = 'A', long)]
        all: bool,
    },
    /// Discard the staging index (working tree untouched)
    Unstage,
    /// Record the staged snapshot as a commit
    ///   -m MSG      commit message
    #[command(verbatim_doc_comment)]
    Commit {
        #[arg(short = 'm', long)]
        message: String,
    },
    /// Show commit history (newest first)
    Log,
    /// Manage remotes - no subcommand lists them
    ///   add NAME URL            add or update a remote
    ///     --format mirror|backup   on-disk shape (default: local→mirror)
    ///     --mount CMD              command that mounts it, run when unreachable
    #[command(verbatim_doc_comment)]
    Remote {
        /// Accepted for git muscle memory; stowe always shows URLs anyway.
        #[arg(short, long)]
        verbose: bool,
        #[command(subcommand)]
        cmd: Option<RemoteCmd>,
    },
    /// Sync remote(s) to the latest commit  [remotes...]
    ///   --force     overwrite by-hand changes on a mirror
    #[command(verbatim_doc_comment)]
    Push {
        /// Remotes to push to. Omit for `origin`; list several to fan out.
        remotes: Vec<String>,
        /// For mirror remotes: overwrite changes made on the mirror outside stowe.
        #[arg(long)]
        force: bool,
    },
    /// Rebuild the working tree from a remote  [remote]
    Pull {
        #[arg(default_value = "origin")]
        remote: String,
    },
    /// Pull a mirror's by-hand changes into local (remote ➜ local)  [remote]
    Adapt {
        /// The mirror remote to adopt changes from (default: origin).
        #[arg(default_value = "origin")]
        remote: String,
    },
    /// Recover committed file(s) from a remote  <paths...>
    ///   -A          restore the whole snapshot
    ///   --from C    the version from commit C (else HEAD)
    ///   --remote R  which remote to fetch from (default: origin)
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
    /// Flip a remote between mirror and backup, in place  [remote]
    ///   --to mirror|backup   target format (omit to flip)
    #[command(verbatim_doc_comment)]
    Convert {
        /// The remote to convert (default: origin).
        #[arg(default_value = "origin")]
        remote: String,
        /// Target format. Omit to flip to the other one.
        #[arg(long, value_parser = ["mirror", "backup"])]
        to: Option<String>,
    },
    /// Manage stowe itself
    ///   update      reinstall the latest release   -y skips the prompt
    ///   check       is a newer release out? (installs nothing)
    #[command(name = "self", subcommand, verbatim_doc_comment)]
    Selfie(selfcmd::Cmd),
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init => commands::init::run(),
        Cmd::Status => commands::status::run(),
        Cmd::Add { paths, all } => commands::add::run(paths, all),
        Cmd::Unstage => commands::unstage::run(),
        Cmd::Commit { message } => commands::commit::run(&message),
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
        Cmd::Selfie(cmd) => selfcmd::run(cmd),
    }
}

// --- commands ---------------------------------------------------------------
