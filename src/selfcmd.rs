//! `stowe self` - manage the installed binary itself.
//!
//! `self update` shells out to `cargo install stowe --force`.
//! `self check` asks the registry for the latest release through `cargo search`,
//! so there is no HTTP client in the dependency tree and the answer comes from
//! the same registry `cargo install` would pull from.

use anyhow::{Result, bail};
use colored::Colorize;
use std::io::Write;
use std::process::Command;

const CRATE: &str = "stowe";

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Reinstall the latest release from crates.io
    ///   -y   skip the confirmation prompt
    #[command(verbatim_doc_comment)]
    Update {
        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
    /// Ask crates.io whether a newer release exists, without installing anything
    Check,
}

pub fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Update { yes } => update(yes),
        Cmd::Check => check(),
    }
}

fn update(yes: bool) -> Result<()> {
    if !yes && !confirm() {
        println!("{}", "Aborted.".dimmed());
        return Ok(());
    }

    println!(
        "{} {}\n",
        "Updating stowe via".dimmed(),
        "cargo install stowe --force".bold()
    );

    match Command::new("cargo")
        .args(["install", CRATE, "--force"])
        .status()
    {
        Ok(status) if status.success() => {
            println!("\n{}", "✓ stowe is up to date.".green());
            Ok(())
        }
        Ok(status) => bail!("update failed (cargo exited {})", status.code().unwrap_or(1)),
        Err(e) => bail!("could not run cargo: {e} - is it installed and on PATH? (https://rustup.rs)"),
    }
}

/// Compare the installed version with the newest one on crates.io. Nothing is
/// downloaded or written, so this is safe to run on a machine you do not want to
/// change.
fn check() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let latest = latest()?;
    if newer(&latest, current) {
        println!(
            "{} {} {}",
            format!("stowe {latest}").bold(),
            "is available, you have".dimmed(),
            current.bold()
        );
        println!("{} {}", "run".dimmed(), "stowe self update".bold());
    } else {
        println!(
            "{} {}",
            format!("stowe {current}").bold(),
            "is the latest release.".dimmed()
        );
    }
    Ok(())
}

/// `cargo search` prints `stowe = "X.Y.Z"    # description` for an exact name
/// match, which is the whole reason no HTTP client is needed here.
fn latest() -> Result<String> {
    let out = Command::new("cargo")
        .args(["search", CRATE, "--limit", "1"])
        .output();
    let out = match out {
        Ok(out) => out,
        Err(e) => bail!("could not run cargo: {e} - is it installed and on PATH? (https://rustup.rs)"),
    };
    if !out.status.success() {
        bail!(
            "could not reach crates.io: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let prefix = format!("{CRATE} = \"");
    text.lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .and_then(|rest| rest.split('"').next())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("the registry did not list `{CRATE}`"))
}

/// Compare dotted versions field by field, so `0.10.0` correctly beats `0.9.9`
/// where a plain string compare would not.
fn newer(a: &str, b: &str) -> bool {
    let fields = |v: &str| {
        v.split(['.', '-'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    fields(a) > fields(b)
}

/// Ask the user to confirm. Defaults to No, so a bare Enter cancels.
fn confirm() -> bool {
    print!(
        "{} {} ",
        "Update stowe to the latest release via cargo?".bold(),
        "[y/N]".dimmed()
    );
    std::io::stdout().flush().ok();

    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}
