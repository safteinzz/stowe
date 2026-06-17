//! stowe — git for files, any remote.
//!
//! A git-shaped CLI for versioning large/binary files. Linear history (one
//! "main", no branches), content-addressed dedup, and a pluggable remote that
//! is just a dumb file store. See the module docs for the on-disk layout.

mod audio;
mod model;
mod remote;
mod repo;
mod scan;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use model::Commit;
use repo::Repo;

#[derive(Parser)]
#[command(name = "stowe", version, about = "git for files, any remote")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new repo (`.stowe/`) in the current directory.
    Init,
    /// Show what changed in the working tree since the last commit.
    Status,
    /// Stage the current state of the working tree.
    Add {
        /// Stage everything (the only mode for now).
        #[arg(short = 'A', long, default_value_t = true)]
        all: bool,
    },
    /// Record the staged snapshot as a commit.
    Commit {
        #[arg(short = 'm', long)]
        message: String,
    },
    /// Show commit history (newest first).
    Log,
    /// Manage remotes.
    Remote {
        #[command(subcommand)]
        cmd: RemoteCmd,
    },
    /// Upload new objects + history to a remote.
    Push {
        #[arg(default_value = "origin")]
        remote: String,
    },
    /// Bring the working tree up to the remote's latest commit.
    Pull {
        #[arg(default_value = "origin")]
        remote: String,
    },
}

#[derive(Subcommand)]
enum RemoteCmd {
    /// Add or update a named remote: `stowe remote add origin local:/path`.
    Add { name: String, url: String },
    /// List configured remotes.
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init => cmd_init(),
        Cmd::Status => cmd_status(),
        Cmd::Add { .. } => cmd_add(),
        Cmd::Commit { message } => cmd_commit(&message),
        Cmd::Log => cmd_log(),
        Cmd::Remote { cmd } => cmd_remote(cmd),
        Cmd::Push { remote } => cmd_push(&remote),
        Cmd::Pull { remote } => cmd_pull(&remote),
    }
}

// --- commands ---------------------------------------------------------------

fn cmd_init() -> Result<()> {
    let cwd = std::env::current_dir()?;
    Repo::init(&cwd)?;
    println!("initialized empty stowe repo in {}/.stowe", cwd.display());
    Ok(())
}

fn cmd_status() -> Result<()> {
    let repo = Repo::find()?;
    let head = repo.head_manifest()?;
    let current = scan::scan(&repo, &head)?;
    let d = scan::diff(&head, &current);
    scan::print_diff(&d);
    if repo.read_index()?.is_some() {
        println!("\n(a snapshot is staged — `stowe commit -m ...` to record it)");
    }
    Ok(())
}

fn cmd_add() -> Result<()> {
    let repo = Repo::find()?;
    let head = repo.head_manifest()?;
    let current = scan::scan(&repo, &head)?;
    let d = scan::diff(&head, &current);
    if d.is_empty() {
        println!("nothing to stage; working tree matches the last commit.");
        return Ok(());
    }
    repo.write_index(&current)?;
    println!("staged snapshot of {} files.", current.len());
    scan::print_diff(&d);
    Ok(())
}

fn cmd_commit(message: &str) -> Result<()> {
    let repo = Repo::find()?;
    let staged = repo
        .read_index()?
        .ok_or_else(|| anyhow!("nothing staged — run `stowe add -A` first"))?;
    let head = repo.head_manifest()?;
    let d = scan::diff(&head, &staged);
    if d.is_empty() {
        bail!("staged snapshot is identical to the last commit; nothing to commit");
    }
    let commit = Commit {
        parent: repo.head()?,
        message: message.to_string(),
        time: now(),
        files: staged,
    };
    let hash = repo.write_commit(&commit)?;
    repo.set_head(&hash)?;
    repo.clear_index()?;
    println!("committed {} \"{message}\"", short(&hash));
    scan::print_diff(&d);
    println!("\n(remember: `stowe push` to back the file contents up to a remote)");
    Ok(())
}

fn cmd_log() -> Result<()> {
    let repo = Repo::find()?;
    let history = repo.history()?;
    if history.is_empty() {
        println!("no commits yet.");
        return Ok(());
    }
    for (hash, c) in history {
        println!("commit {hash}");
        println!("Date:  {}", format_time(c.time));
        println!("Files: {}", c.files.len());
        println!("\n    {}\n", c.message);
    }
    Ok(())
}

fn cmd_remote(cmd: RemoteCmd) -> Result<()> {
    let repo = Repo::find()?;
    match cmd {
        RemoteCmd::Add { name, url } => {
            let mut cfg = repo.config()?;
            cfg.remotes.insert(name.clone(), url.clone());
            repo.save_config(&cfg)?;
            println!("remote `{name}` -> {url}");
        }
        RemoteCmd::List => {
            let cfg = repo.config()?;
            if cfg.remotes.is_empty() {
                println!("no remotes. add one, e.g.:");
                println!("  stowe remote add origin local:/path/to/backup");
            }
            for (name, url) in &cfg.remotes {
                println!("{name}\t{url}");
            }
        }
    }
    Ok(())
}

fn cmd_push(name: &str) -> Result<()> {
    let repo = Repo::find()?;
    let head = repo
        .head()?
        .ok_or_else(|| anyhow!("nothing committed yet — `stowe commit` first"))?;
    let url = remote_url(&repo, name)?;
    let backend = remote::open(&url)?;
    let history = repo.history()?;

    // 1. Upload the file contents of the latest snapshot (deduped by hash).
    //    Build the unique (object-key, source-file) set, then let the remote
    //    upload them concurrently, skipping any the remote already has.
    let head_commit = &history[0].1;
    let mut seen = HashSet::new();
    let mut to_upload = Vec::new();
    for e in &head_commit.files {
        if seen.insert(e.hash.as_str()) {
            to_upload.push((remote::object_key(&e.hash), repo.root.join(&e.path)));
        }
    }
    let new_objects = backend.put_files(to_upload)?;

    // 2. Upload commit metadata for the whole history.
    let mut new_commits = 0;
    for (hash, _) in &history {
        let key = format!("commits/{hash}.json");
        if !backend.exists(&key)? {
            let bytes = std::fs::read(repo.dir.join("commits").join(format!("{hash}.json")))?;
            backend.put_bytes(&key, &bytes)?;
            new_commits += 1;
        }
    }

    // 3. Move the remote's pointer.
    backend.put_bytes("refs/main", head.as_bytes())?;

    println!(
        "pushed to `{name}`: {new_objects} new objects, {new_commits} new commits, refs/main -> {}",
        short(&head)
    );
    Ok(())
}

fn cmd_pull(name: &str) -> Result<()> {
    let repo = Repo::find()?;
    let url = remote_url(&repo, name)?;
    let backend = remote::open(&url)?;
    if !backend.exists("refs/main")? {
        bail!("remote `{name}` is empty — nothing to pull");
    }
    let remote_head = String::from_utf8(backend.get_bytes("refs/main")?)?
        .trim()
        .to_string();

    // Download the commit chain (metadata) we don't already have.
    let mut new_commits = 0;
    let mut cur = Some(remote_head.clone());
    while let Some(hash) = cur {
        let local = repo.dir.join("commits").join(format!("{hash}.json"));
        let bytes = if local.exists() {
            std::fs::read(&local)?
        } else {
            let b = backend.get_bytes(&format!("commits/{hash}.json"))?;
            std::fs::write(&local, &b)?;
            new_commits += 1;
            b
        };
        let commit: Commit = serde_json::from_slice(&bytes)?;
        cur = commit.parent;
    }
    repo.set_head(&remote_head)?;

    // Reconstruct the working tree for the remote's latest snapshot.
    let commit = repo.read_commit(&remote_head)?;
    let mut written = 0;
    for e in &commit.files {
        let dest = repo.root.join(&e.path);
        let need = !dest.exists() || scan::hash_file(&dest)? != e.hash;
        if need {
            backend.get_file(&remote::object_key(&e.hash), &dest)?;
            written += 1;
        }
    }
    repo.clear_index()?;

    println!(
        "pulled from `{name}`: now at {} ({new_commits} new commits, {written} files written)",
        short(&remote_head)
    );
    Ok(())
}

// --- helpers ----------------------------------------------------------------

fn remote_url(repo: &Repo, name: &str) -> Result<String> {
    repo.config()?
        .remotes
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow!("no remote named `{name}` — add one: stowe remote add {name} <url>"))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(10)]
}

/// Format Unix seconds as `YYYY-MM-DD HH:MM:SS UTC` without pulling in a date
/// crate (Howard Hinnant's civil-from-days algorithm).
fn format_time(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
