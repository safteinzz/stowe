//! Handing two files to the user's `git difftool`, or one to their editor, and
//! taking the terminal back.
//!
//! stowe has no git repo to name revisions in, so git is handed two files with
//! `--no-index`. git writes a complaint to the normal screen, which is taken
//! away again the instant the TUI comes back, so what it said is kept and
//! handed back as a modal.

use super::suspended;
use super::widgets::Modal;
use ratatui::DefaultTerminal;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

/// How many lines of the tool's complaint the modal keeps: enough to explain a
/// failure, never enough to fill the screen with it.
const MAX_ERR_LINES: usize = 40;

/// What came back from handing the terminal over.
pub enum DiffTool {
    /// The tool ran and had nothing to say.
    Quiet,
    /// A one-liner for the status line: something happened, but there is
    /// nothing to read about it.
    Note(String),
    /// It failed and said why. The caller shows this until it is dismissed.
    Failed(Modal),
}

/// Is a difftool configured? `git difftool` uses `diff.tool`, falling back to
/// `merge.tool`. Checked first, so a missing one is explained without the
/// screen going away and coming back for nothing.
fn has_difftool() -> bool {
    ["diff.tool", "merge.tool"].iter().any(|k| {
        Command::new("git")
            .args(["config", k])
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
    })
}

/// Suspend the TUI, run `git difftool -y --no-index <old> <new>` in `dir` with
/// the terminal handed over (so a terminal tool like vimdiff works), then take
/// it back.
pub fn run_difftool(
    terminal: &mut DefaultTerminal,
    enhanced: bool,
    dir: &Path,
    old: &Path,
    new: &Path,
) -> DiffTool {
    if !has_difftool() {
        return DiffTool::Failed(Modal::new(
            "no difftool configured",
            "Enter hands the file to `git difftool`, but neither diff.tool nor \
             merge.tool is set, so there is nothing to open it with.\n\n\
             Set one and try again, for example:\n\n    \
             git config --global diff.tool vimdiff\n\n\
             `git difftool --tool-help` lists the tools your git can drive.",
        ));
    }

    let mut git = Command::new("git");
    git.current_dir(dir)
        .args(["difftool", "-y", "--no-index", "--"])
        .arg(old)
        .arg(new);
    let outcome = suspended(terminal, enhanced, || handoff(git));
    let _ = terminal.clear();

    match outcome {
        Err(e) if e.kind() == io::ErrorKind::NotFound => DiffTool::Failed(Modal::new(
            "difftool would not start",
            "Enter hands the file to `git difftool`, and `git` is not installed here.",
        )),
        Err(e) => DiffTool::Failed(Modal::new(
            "difftool would not start",
            format!(
                "Enter hands the file to `git difftool`, and `git` {}: check that \
                 `git difftool` runs from your shell.",
                refused(&e)
            ),
        )),
        // `--no-index` exits 1 whenever the two files differ, which they do.
        Ok((status, _)) if matches!(status.code(), Some(0 | 1)) => DiffTool::Quiet,
        Ok((status, said)) => {
            let code = match status.code() {
                Some(c) => c.to_string(),
                None => "a signal".to_string(),
            };
            if said.is_empty() {
                // A tool that ran fine and was quit with a non-zero code (vim's
                // `:cq`) has nothing to explain, so it doesn't get a box.
                DiffTool::Note(format!("difftool exited with status {code}"))
            } else {
                DiffTool::Failed(Modal::new(
                    "difftool failed",
                    format!("git difftool exited with status {code}.\n\n{said}"),
                ))
            }
        }
    }
}

/// Suspend the TUI and open `path` in the user's editor, `$VISUAL`, else
/// `$EDITOR`, else `vi`, for a file with only one version to look at.
pub fn run_editor(terminal: &mut DefaultTerminal, enhanced: bool, path: &Path) -> DiffTool {
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "vi".to_string());
    // Through `sh`, since the variable may carry flags (`code -w`).
    let mut sh = Command::new("sh");
    sh.arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path);
    let outcome = suspended(terminal, enhanced, || handoff(sh));
    let _ = terminal.clear();

    match outcome {
        Err(e) => DiffTool::Failed(Modal::new(
            "editor would not start",
            format!(
                "Enter opens the file in `{editor}` through `sh`, and `sh` {}: check \
                 that `sh` runs from your shell.",
                refused(&e)
            ),
        )),
        Ok((status, _)) if status.success() => DiffTool::Quiet,
        Ok((status, said)) => {
            let code = match status.code() {
                Some(c) => c.to_string(),
                None => "a signal".to_string(),
            };
            if said.is_empty() {
                DiffTool::Note(format!("`{editor}` exited with status {code}"))
            } else {
                DiffTool::Failed(Modal::new(
                    "editor failed",
                    format!("`{editor}` exited with status {code}.\n\n{said}"),
                ))
            }
        }
    }
}

/// Why a program would not start, in words, since the OS's own is an errno.
fn refused(e: &io::Error) -> &'static str {
    match e.kind() {
        io::ErrorKind::NotFound => "is not installed",
        io::ErrorKind::PermissionDenied => "is not allowed to run",
        _ => "could not be started",
    }
}

/// Run `cmd` with the terminal handed over, keeping a copy of its stderr.
/// A thread drains the pipe while the tool runs, so a chatty one can't fill it
/// and hang waiting for someone to read.
fn handoff(mut cmd: Command) -> io::Result<(ExitStatus, String)> {
    let mut child = cmd.stderr(Stdio::piped()).spawn()?;

    let mut pipe = child.stderr.take().expect("stderr is piped");
    let drain = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    });
    let status = child.wait()?;
    let said = drain.join().unwrap_or_default();
    Ok((status, tail(&String::from_utf8_lossy(&said))))
}

/// The tail of what the tool printed: blanks dropped and consecutive repeats
/// collapsed.
fn tail(said: &str) -> String {
    let mut lines: Vec<&str> = said
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect();
    lines.dedup();
    let start = lines.len().saturating_sub(MAX_ERR_LINES);
    lines[start..].join("\n")
}
