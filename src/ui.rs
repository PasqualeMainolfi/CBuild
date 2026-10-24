//! Terminal output: cargo-style status lines, colored only on a terminal.

use std::env;
use std::fmt::Display;
use std::io::{IsTerminal, stderr};

pub fn color_enabled() -> bool {
    stderr().is_terminal() && env::var_os("NO_COLOR").is_none()
}

fn paint(code: &str, text: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn status(label: &str, message: impl Display) {
    eprintln!("{} {message}", paint("1;32", &format!("{label:>12}")));
}

pub fn note(message: impl Display) {
    eprintln!("{} {message}", paint("1;36", "note:"));
}

pub fn hint(message: impl Display) {
    eprintln!("{} {message}", paint("1;36", "hint:"));
}

pub fn error(message: impl Display) {
    eprintln!("{} {message}", paint("1;31", "error:"));
}

/// Prints the lines removed from `old` and added in `new`.
pub fn diff(old: &str, new: &str) {
    for line in diff_lines(old, new) {
        let color = if line.starts_with('+') { "32" } else { "31" };
        eprintln!("{}", paint(color, &line));
    }
}

/// Changed lines only, as "- removed" / "+ added", from a longest-common-subsequence diff.
fn diff_lines(old: &str, new: &str) -> Vec<String> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // common[i][j]: length of the longest common subsequence of a[i..] and b[j..].
    let mut common = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            common[i][j] = if a[i] == b[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || common[i][j + 1] >= common[i + 1][j]) {
            out.push(format!("+ {}", b[j]));
            j += 1;
        } else {
            out.push(format!("- {}", a[i]));
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::diff_lines;

    #[test]
    fn diff_shows_only_changed_lines() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nc\nd\ne\n";
        assert_eq!(diff_lines(old, new), ["- b", "+ e"]);
        assert!(diff_lines(old, old).is_empty());
    }
}
