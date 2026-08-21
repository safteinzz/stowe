//! Asking the user a yes/no question on the terminal.

/// Ask a yes/no question that defaults to **yes** (bare Enter = yes). Yes on a
/// non-interactive stdin, so scripts aren't blocked.
pub(crate) fn confirm_default_yes(prompt: &str) -> bool {
    use std::io::Write;
    print!("{prompt} [Y/n] ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return true;
    }
    !matches!(input.trim().to_lowercase().as_str(), "n" | "no")
}
