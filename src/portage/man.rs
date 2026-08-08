use std::path::{Path, PathBuf};
use std::process::Command;

/// Finds and renders the most relevant man page an installed package
/// shipped, as plain text — used by the detail page's "Learn More" panel,
/// which is the one place in the app that wants the packager's own
/// documentation rather than upstream marketing prose.
pub fn lookup(category: &str, name: &str, version: &str) -> Option<String> {
    let path = find_man_page(category, name, version)?;
    render(&path)
}

/// Scans the package's recorded file list for every man page it installed
/// and picks the single best one: an exact filename match for the package
/// name wins over an unrelated helper binary's page, and among ties the
/// lowest section number wins (1 = user commands, the one people actually
/// mean by "the man page" — 8/5/etc. document config files and syscalls,
/// which is rarely what "Learn More" is being opened for).
fn find_man_page(category: &str, name: &str, version: &str) -> Option<PathBuf> {
    let contents_path = format!("/var/db/pkg/{category}/{name}-{version}/CONTENTS");
    let contents = std::fs::read_to_string(contents_path).ok()?;

    let mut candidates: Vec<(PathBuf, bool, u8)> = Vec::new();
    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("obj") {
            continue;
        }
        let Some(raw_path) = parts.next() else { continue };
        if !raw_path.contains("/man/man") {
            continue;
        }
        let path = PathBuf::from(raw_path);
        let Some((stem, section)) = parse_man_filename(&path) else { continue };
        if !path.exists() {
            continue;
        }
        candidates.push((path, stem == name, section));
    }

    candidates.sort_by_key(|(_, exact, section)| (!*exact, *section));
    candidates.into_iter().next().map(|(path, _, _)| path)
}

/// Splits `htop.1.gz` (or the uncompressed `htop.1`) into `("htop", 1)`.
fn parse_man_filename(path: &Path) -> Option<(String, u8)> {
    let filename = path.file_name()?.to_str()?;
    let without_gz = filename.strip_suffix(".gz").unwrap_or(filename);
    let (stem, section) = without_gz.rsplit_once('.')?;
    let section = section.chars().next()?.to_digit(10)? as u8;
    Some((stem.to_string(), section))
}

/// Renders a man page file to plain text via the system `man` command
/// (which transparently handles both compressed and plain troff source),
/// then strips the backspace-based bold/underline sequences `man` emits
/// even when its output isn't a terminal — otherwise every emphasised word
/// would show up doubled (`ccoommmmaanndd`-style) in a plain `GtkLabel`,
/// which has no concept of terminal formatting to interpret them with.
fn render(path: &Path) -> Option<String> {
    let output = Command::new("man")
        .env("MANWIDTH", "100")
        .arg("--local-file")
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = strip_overstrike(&String::from_utf8_lossy(&output.stdout));
    let text = text.trim();
    // Long man pages (coreutils-style ones especially) can run to
    // thousands of lines — capped here rather than truncated silently by
    // whatever text widget eventually shows it, so what's cut off is
    // predictable rather than however the layout happens to clip it.
    const MAX_CHARS: usize = 20_000;
    if text.chars().count() > MAX_CHARS {
        let cut: String = text.chars().take(MAX_CHARS).collect();
        Some(format!("{cut}\n\n… (truncated — see the full page with `man {})`", path.display()))
    } else {
        (!text.is_empty()).then(|| text.to_string())
    }
}

/// Collapses `man`'s overstrike formatting — a character repeated as
/// `c\u{8}c` for bold, or preceded by `_\u{8}` for underline — down to the
/// plain character it represents.
fn strip_overstrike(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if i + 2 < chars.len() && chars[i + 1] == '\u{8}' {
            out.push(chars[i + 2]);
            i += 3;
        } else if chars[i] != '\u{8}' {
            out.push(chars[i]);
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_exact_name_match_over_an_unrelated_helper() {
        assert_eq!(
            parse_man_filename(Path::new("/usr/share/man/man1/htop.1.gz")),
            Some(("htop".to_string(), 1))
        );
        assert_eq!(
            parse_man_filename(Path::new("/usr/share/man/man8/htop-helper.8")),
            Some(("htop-helper".to_string(), 8))
        );
    }

    #[test]
    fn bold_and_underline_overstrike_collapse_to_plain_text() {
        assert_eq!(strip_overstrike("N\u{8}NA\u{8}AM\u{8}ME\u{8}E"), "NAME");
        assert_eq!(strip_overstrike("_\u{8}h_\u{8}t_\u{8}o_\u{8}p"), "htop");
        assert_eq!(strip_overstrike("plain text"), "plain text");
    }
}
