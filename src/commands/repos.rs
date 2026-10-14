//! `stowe repos`: every stowe repo under a folder, what changed in each, and how
//! far each of its remotes is behind.

use anyhow::{Result, bail};
use colored::Colorize;
use rayon::prelude::*;
use std::path::Path;

use crate::repos::{self, Behind};

pub fn run(base: &Path, depth: usize) -> Result<()> {
    let found = repos::find(base, depth);
    if found.is_empty() {
        bail!(
            "no stowe repos in or under `{}` (looked {depth} levels down); run it inside one, \
             or point it at a folder that holds some",
            crate::paths::short(base)
        );
    }
    let states: Vec<_> = found
        .par_iter()
        .map(|p| (p, repos::state(p, base)))
        .collect();
    let name_w = states
        .iter()
        .map(|(p, _)| repos::relative_name(p, base).chars().count())
        .max()
        .unwrap_or(0);
    let mut failed = false;
    for (path, state) in states {
        let name = repos::relative_name(path, base);
        match state {
            Ok(s) => outln!("{}", line(&s, name_w)),
            Err(e) => {
                failed = true;
                eprintln!("{name}: {e}");
            }
        }
    }
    if failed {
        bail!("some repos could not be read");
    }
    Ok(())
}

/// `name  ✚3 ↑2  drive 2 behind · offsite in sync`, or `✓ clean` for nothing.
fn line(s: &repos::State, name_w: usize) -> String {
    let mut marks = Vec::new();
    if s.changed > 0 {
        marks.push(format!("✚{}", s.changed).yellow().to_string());
    }
    if s.unpushed() > 0 {
        marks.push(format!("↑{}", s.unpushed()).green().to_string());
    }
    let marks = if marks.is_empty() {
        format!("{:<8}", "✓ clean").green().to_string()
    } else {
        let width: usize = marks.len() - 1
            + [s.changed, s.unpushed()]
                .iter()
                .filter(|n| **n > 0)
                .map(|n| n.to_string().len() + 1)
                .sum::<usize>();
        format!(
            "{}{}",
            marks.join(" "),
            " ".repeat(8usize.saturating_sub(width))
        )
    };
    let remotes = if s.remotes.is_empty() {
        "no remotes".dimmed().to_string()
    } else {
        s.remotes
            .iter()
            .map(|(name, b)| {
                let words = b.words();
                match b {
                    Behind::By(0) => format!("{name} {}", words.dimmed()),
                    _ => format!("{name} {}", words.yellow()),
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    };
    format!("{:<name_w$}  {marks}  {remotes}", s.name.bold())
}
