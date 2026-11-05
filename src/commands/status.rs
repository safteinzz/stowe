//! `stowe status`: what changed since the last commit, without hashing audio.

use anyhow::{Result, bail};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::fs::TryLockError;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::repo::Repo;
use crate::repos::Behind;
use crate::{repos, scan};

/// How long `--prompt` waits for a scan before it settles for the last line,
/// well inside the half second a shell prompt gives a command by default.
const PROMPT_WAIT: Duration = Duration::from_millis(300);

/// `short` prints `status -s`, whose form `--help` fixes for scripts, `prompt`
/// the same line within [`PROMPT_WAIT`], and `refresh` is the scan `prompt`
/// leaves running.
pub fn run(short: bool, prompt: bool, refresh: bool) -> Result<()> {
    let repo = Repo::find()?;
    if refresh {
        return refresh_prompt(&repo);
    }
    if prompt {
        return print_prompt(&repo);
    }
    if short {
        outln!("{}", short_line(&repo)?);
        return Ok(());
    }
    let head = repo.head_manifest()?;
    let index = repo.read_index()?;
    // `status` is a quick "what changed?": no audio decoding.
    let working = scan::working(&repo, &head, index.as_ref())?;
    let history = repos::hashes(&repo)?;
    let staged = scan::diff(&head, index.as_ref().unwrap_or(&head));
    print_remotes(&remotes(&repo, &history)?);
    scan::print_status(&staged, &working.since_staged(), &working.since_head());
    Ok(())
}

/// Each remote and the commits it lacks.
fn remotes(repo: &Repo, history: &[String]) -> Result<Vec<(String, Behind)>> {
    Ok(repo
        .config()?
        .remotes
        .keys()
        .map(|name| (name.clone(), repos::behind(repo, name, history)))
        .collect())
}

/// `status -s`'s line: each count that is not zero, in a fixed order.
fn short_line(repo: &Repo) -> Result<String> {
    let c = scan::changes(repo)?;
    let history = repos::hashes(repo)?;
    let remotes = remotes(repo, &history)?;
    let counts = [
        ('+', c.added),
        ('-', c.removed),
        ('~', c.modified),
        ('⇄', c.moved),
        ('↑', repos::unpushed(&remotes, history.len())),
        ('⇡', repos::elsewhere(&remotes, history.len())),
    ];
    let line: Vec<String> = counts
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(mark, n)| format!("{mark}{n}"))
        .collect();
    Ok(line.join(" "))
}

/// What the last `--refresh` worked out, kept in `.stowe/cache/prompt`.
#[derive(Serialize, Deserialize)]
struct Answer {
    /// When its scan began, in nanoseconds since the epoch.
    began: u64,
    /// `status -s`'s line, or the words of what stopped it.
    line: std::result::Result<String, String>,
}

/// `--prompt`: start a refresh and print its line when it lands within
/// [`PROMPT_WAIT`], else the last line known with ` …` after it (`…` alone
/// before the first), so a slow disk costs the prompt a stale line rather than
/// the whole segment.
fn print_prompt(repo: &Repo) -> Result<()> {
    let asked = now_nanos();
    let deadline = Instant::now() + PROMPT_WAIT;
    // A `.stowe/` this cannot write to keeps no answer, so the line is worked out here.
    let Ok(mut child) = repo
        .cache_lock("prompt.run")
        .and_then(|_| start_refresh(repo))
    else {
        outln!("{}", short_line(repo)?);
        return Ok(());
    };
    loop {
        let last = read_answer(repo);
        if let Some(answer) = &last
            && answer.began >= asked
        {
            match &answer.line {
                Ok(line) => outln!("{line}"),
                Err(words) => bail!("{words}"),
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            match last.and_then(|a| a.line.ok()).filter(|l| !l.is_empty()) {
                Some(line) => outln!("{line} …"),
                None => outln!("…"),
            }
            return Ok(());
        }
        let _ = child.try_wait();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Start `status --refresh` in a process group of its own with its output
/// going nowhere: a shell prompt reads a command's output until every holder
/// of it has closed it, and a Ctrl-C at the shell is not for it.
fn start_refresh(repo: &Repo) -> Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    Ok(Command::new(std::env::current_exe()?)
        .args(["status", "--refresh"])
        .current_dir(&repo.root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?)
}

/// `--refresh`: work `status -s`'s line out and keep it for `--prompt`. One
/// runs at a time and at most one waits behind it, so a prompt redrawn again
/// and again over a slow disk never piles scans up: a refresh that finds
/// another already waiting leaves, since that one begins after it would have.
fn refresh_prompt(repo: &Repo) -> Result<()> {
    let waiting = repo.cache_lock("prompt.wait")?;
    match waiting.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }
    let running = repo.cache_lock("prompt.run")?;
    running.lock()?;
    drop(waiting);
    let began = now_nanos();
    let line = short_line(repo).map_err(|e| crate::errors::words(&e));
    repo.write_cache("prompt", &serde_json::to_vec(&Answer { began, line })?)?;
    Ok(())
}

fn read_answer(repo: &Repo) -> Option<Answer> {
    let bytes = fs_err::read(repo.cache_dir().join("prompt")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// One line for each remote that lacks something, and what catches it up.
fn print_remotes(remotes: &[(String, Behind)]) {
    let mut any = false;
    for (name, b) in remotes {
        let fix = match b {
            Behind::By(0) => continue,
            Behind::By(_) | Behind::Never => format!("`stowe push {name}`"),
            Behind::Elsewhere => {
                format!("`stowe pull {name}`, or `stowe push {name} --force` over it")
            }
        };
        outln!("{} {}: {fix}", name.bold(), b.words().yellow());
        any = true;
    }
    if any {
        outln!();
    }
}
