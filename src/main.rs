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

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use model::{Commit, Entry, Manifest};
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
    /// Stage files for the next commit: specific paths, or `-A` for everything.
    Add {
        /// Files or directories to stage. Omit and pass `-A` to stage the whole tree.
        paths: Vec<std::path::PathBuf>,
        /// Stage the entire working tree.
        #[arg(short = 'A', long)]
        all: bool,
    },
    /// Discard the staging index (the working tree is left untouched).
    Unstage,
    /// Record the staged snapshot as a commit.
    Commit {
        #[arg(short = 'm', long)]
        message: String,
    },
    /// Show commit history (newest first).
    Log,
    /// Manage remotes. With no subcommand, lists them (name + URL).
    Remote {
        /// Accepted for git muscle memory; stowe always shows URLs anyway.
        #[arg(short, long)]
        verbose: bool,
        #[command(subcommand)]
        cmd: Option<RemoteCmd>,
    },
    /// Upload new objects + history to one or more remotes (default: origin).
    Push {
        /// Remotes to push to. Omit for `origin`; list several to fan out.
        remotes: Vec<String>,
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
        Cmd::Add { paths, all } => cmd_add(paths, all),
        Cmd::Unstage => cmd_unstage(),
        Cmd::Commit { message } => cmd_commit(&message),
        Cmd::Log => cmd_log(),
        Cmd::Remote { cmd, .. } => cmd_remote(cmd),
        Cmd::Push { remotes } => cmd_push(&remotes),
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
    // `status` is a quick "what changed?" — hash only, no audio decoding.
    let working = scan::scan(&repo, &head, false)?;
    // The staging baseline is the index if anything's staged, else HEAD.
    let base = repo.read_index()?.unwrap_or_else(|| head.clone());

    let staged = scan::diff(&head, &base); // Changes to be committed
    let unstaged = scan::diff(&base, &working); // not staged + untracked (its .added)
    let summary = scan::diff(&head, &working); // net change, for the summary line
    scan::print_status(&staged, &unstaged, &summary);
    Ok(())
}

fn cmd_add(paths: Vec<PathBuf>, all: bool) -> Result<()> {
    let repo = Repo::find()?;
    let head = repo.head_manifest()?;

    // `-A`: stage a fresh snapshot of the whole tree (fingerprinting audio).
    if all {
        let current = scan::scan(&repo, &head, true)?;
        let d = scan::diff(&head, &current);
        if d.is_empty() {
            println!("nothing to stage; working tree matches the last commit.");
            return Ok(());
        }
        repo.write_index(&current)?;
        println!("staged snapshot of {} files.", current.len());
        scan::print_diff(&d);
        return Ok(());
    }

    if paths.is_empty() {
        bail!("specify files/directories to stage, or `-A` to stage everything");
    }

    // Per-path staging. Start from what's already staged (or HEAD if nothing is)
    // and upsert / remove just the named paths, keyed by repo-relative path.
    let mut index: BTreeMap<String, Entry> = repo
        .read_index()?
        .unwrap_or_else(|| head.clone())
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();

    let root = repo.root.canonicalize()?;
    let cwd = std::env::current_dir()?;
    let mut staged = 0usize;
    let mut removed = 0usize;

    for arg in &paths {
        let lexical = if arg.is_absolute() { arg.clone() } else { cwd.join(arg) };
        // Resolve to an absolute path inside the repo. `canonicalize` handles
        // existing paths; for a path that was deleted, resolve via its parent.
        let abs = match lexical.canonicalize() {
            Ok(c) => c,
            Err(_) => {
                let parent = lexical.parent().unwrap_or_else(|| Path::new("."));
                let name = lexical
                    .file_name()
                    .ok_or_else(|| anyhow!("bad path: {}", arg.display()))?;
                parent
                    .canonicalize()
                    .with_context(|| format!("no such path: {}", arg.display()))?
                    .join(name)
            }
        };
        let rel = scan::rel_path(&root, &abs);
        if abs.strip_prefix(&root).is_err() {
            bail!("{} is outside the repo", arg.display());
        }

        if abs.is_dir() {
            // Stage every file under the directory (in parallel)...
            let entries: Vec<Entry> = scan::files_under(&abs)?
                .par_iter()
                .map(|f| scan::entry_for(&root, f, true))
                .collect::<Result<_>>()?;
            let present: HashSet<String> = entries.iter().map(|e| e.path.clone()).collect();
            for e in entries {
                index.insert(e.path.clone(), e);
                staged += 1;
            }
            // ...and stage the removal of files that used to be under it but are gone.
            let gone: Vec<String> = index
                .keys()
                .filter(|p| under_prefix(p, &rel) && !present.contains(*p))
                .cloned()
                .collect();
            for p in gone {
                index.remove(&p);
                removed += 1;
            }
        } else if abs.is_file() {
            let e = scan::entry_for(&root, &abs, true)?;
            index.insert(rel, e);
            staged += 1;
        } else if index.remove(&rel).is_some() {
            // Path is gone from disk → stage its deletion.
            removed += 1;
        } else {
            bail!("no such path, and nothing staged to remove: {}", arg.display());
        }
    }

    let manifest: Manifest = index.into_values().collect();
    repo.write_index(&manifest)?;

    let mut summary = format!("staged {staged} file(s)");
    if removed > 0 {
        summary += &format!(", {removed} removal(s)");
    }
    println!("{summary}");
    scan::print_diff(&scan::diff(&head, &manifest));
    Ok(())
}

/// True if `path` is `prefix` itself or sits beneath it (or `prefix` is the
/// repo root, i.e. empty).
fn under_prefix(path: &str, prefix: &str) -> bool {
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

fn cmd_unstage() -> Result<()> {
    let repo = Repo::find()?;
    match repo.read_index()? {
        None => println!("nothing staged."),
        Some(staged) => {
            repo.clear_index()?;
            println!("unstaged {} file(s); working tree left as-is.", staged.len());
        }
    }
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

fn cmd_remote(cmd: Option<RemoteCmd>) -> Result<()> {
    let repo = Repo::find()?;
    match cmd {
        Some(RemoteCmd::Add { name, url }) => {
            let mut cfg = repo.config()?;
            cfg.remotes.insert(name.clone(), url.clone());
            repo.save_config(&cfg)?;
            println!("remote `{name}` -> {url}");
        }
        // Bare `stowe remote` and `stowe remote list` both just list.
        None | Some(RemoteCmd::List) => {
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

fn cmd_push(names: &[String]) -> Result<()> {
    let repo = Repo::find()?;
    let head = repo
        .head()?
        .ok_or_else(|| anyhow!("nothing committed yet — `stowe commit` first"))?;
    let history = repo.history()?;
    let head_commit = &history[0].1;

    // stowe keeps no local object store, so an object's bytes are read from the
    // working tree. Index the tree by *content hash* rather than path, so a file
    // renamed since the commit is still found (its content lives under the new
    // name). Hash-only + cached, so this scan is cheap.
    let working = scan::scan(&repo, &repo.head_manifest()?, false)?;
    let mut by_hash: HashMap<&str, &str> = HashMap::new();
    for e in &working {
        by_hash.entry(&e.hash).or_insert(&e.path);
    }

    let mut seen = HashSet::new();
    let mut to_upload = Vec::new();
    for e in &head_commit.files {
        if !seen.insert(e.hash.as_str()) {
            continue;
        }
        match by_hash.get(e.hash.as_str()) {
            Some(rel) => to_upload.push((remote::object_key(&e.hash), repo.root.join(rel))),
            None => bail!(
                "content for `{}` is no longer in the working tree (modified or deleted \
                 since the commit) — restore it or commit the change before pushing",
                e.path
            ),
        }
    }

    // Resolve all targets up front so a bad name fails before any upload.
    let targets = if names.is_empty() {
        vec!["origin".to_string()]
    } else {
        names.to_vec()
    };
    let mut backends = Vec::new();
    for name in &targets {
        let url = remote_url(&repo, name)?;
        backends.push((name.clone(), remote::open(&url)?));
    }

    for (name, backend) in &backends {
        // 1. Upload object contents (deduped; the remote skips ones it has).
        let new_objects = backend.put_files(to_upload.clone())?;

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
    }
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
