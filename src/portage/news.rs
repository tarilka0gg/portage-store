use anyhow::{Context, Result, bail};
use std::process::Command;

/// One GLEP-42 news item, as `eselect news list all` reports it — Gentoo's
/// only channel for "this profile change will break your system unless
/// you do X first" announcements. Portage prints a one-line "N news items
/// need reading" reminder after every sync/update, easy to miss in a wall
/// of terminal output and, before this, simply not surfaced anywhere in
/// this app at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsItem {
    /// The number `eselect news read <n>` expects — stable for a given
    /// item within one `eselect` invocation, but not a permanent id (see
    /// `id` below for that).
    pub number: u32,
    pub posted: String,
    pub title: String,
    pub unread: bool,
}

fn eselect(args: &[&str]) -> Result<String> {
    let output = Command::new("eselect").args(args).output().context("failed to run eselect")?;
    if !output.status.success() {
        bail!("eselect {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parses one line of `eselect news list all` output:
/// `  [5]   N  2020-06-24  xorg-server dropping default suid`
/// — a bracketed number, an optional `N` for unread, a date, then the
/// title (which may itself contain any of those characters, so it's taken
/// as everything left after the fixed-shape prefix rather than split on
/// whitespace throughout).
fn parse_list_line(line: &str) -> Option<NewsItem> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix('[')?;
    let (number_str, rest) = rest.split_once(']')?;
    let number: u32 = number_str.trim().parse().ok()?;

    let rest = rest.trim_start();
    let (unread, rest) = match rest.strip_prefix('N') {
        Some(after) => (true, after.trim_start()),
        None => (false, rest),
    };

    let (posted, title) = rest.split_once(char::is_whitespace)?;
    Some(NewsItem { number, posted: posted.trim().to_string(), unread, title: title.trim().to_string() })
}

/// Every news item known to this system, newest and oldest alike — the
/// unread ones are what matter, but showing only those would make an
/// already-read item impossible to look back up.
pub fn list() -> Result<Vec<NewsItem>> {
    let output = eselect(&["news", "list", "all"])?;
    Ok(output.lines().filter_map(parse_list_line).collect())
}

/// One item's title/posted-date/body, split out of `eselect news read
/// --raw`'s GLEP-42 output — a fixed `Key: value` header block, a blank
/// line, then the body proper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsBody {
    pub title: String,
    pub posted: String,
    pub body: String,
}

fn parse_raw(raw: &str) -> NewsBody {
    let mut title = String::new();
    let mut posted = String::new();
    let mut lines = raw.lines();
    for line in lines.by_ref() {
        if line.trim().is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Title:") {
            title = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("Posted:") {
            posted = value.trim().to_string();
        }
    }
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    NewsBody { title, posted, body }
}

/// Reads one item's content without marking it read — `--raw` is the one
/// `eselect news read` form that previews rather than consumes, which is
/// what a GUI reader wants: opening the list to glance at what's there
/// shouldn't silently mark everything as read before the user has
/// actually read any of it. Call `mark_read` once they actually dismiss
/// an item.
pub fn read(number: u32) -> Result<NewsBody> {
    let raw = eselect(&["news", "read", "--raw", &number.to_string()])?;
    Ok(parse_raw(&raw))
}

/// Marks one item read — the plain (non-`--raw`) form of `read`, which is
/// the one that actually updates `eselect`'s own read-tracking state, kept
/// in sync with whatever the terminal `eselect news` would show.
pub fn mark_read(number: u32) -> Result<()> {
    eselect(&["news", "read", "--quiet", &number.to_string()]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_unread_item() {
        let item = parse_list_line("  [5]   N  2020-06-24  xorg-server dropping default suid").unwrap();
        assert_eq!(
            item,
            NewsItem {
                number: 5,
                posted: "2020-06-24".to_string(),
                unread: true,
                title: "xorg-server dropping default suid".to_string(),
            }
        );
    }

    #[test]
    fn parses_a_read_item_with_no_marker() {
        let item = parse_list_line("  [1]      2018-08-07  Migration required for OpenSSH with LDAP").unwrap();
        assert!(!item.unread);
        assert_eq!(item.number, 1);
        assert_eq!(item.title, "Migration required for OpenSSH with LDAP");
    }

    #[test]
    fn header_and_blank_lines_are_not_items() {
        assert_eq!(parse_list_line("News items:"), None);
        assert_eq!(parse_list_line(""), None);
    }

    #[test]
    fn raw_body_splits_headers_from_content() {
        let raw = "Title: xorg-server dropping default suid\n\
Author: Piotr Karbowski <slashbeast@gentoo.org>\n\
Posted: 2020-06-24\n\
Revision: 3\n\
News-Item-Format: 2.0\n\
Display-If-Installed: x11-base/xorg-server\n\
\n\
Starting 2020-07-15, stable keyworded x11-base/xorg-server will default\n\
to using the logind interface instead of suid by default.\n";
        let parsed = parse_raw(raw);
        assert_eq!(parsed.title, "xorg-server dropping default suid");
        assert_eq!(parsed.posted, "2020-06-24");
        assert!(parsed.body.starts_with("Starting 2020-07-15"));
        assert!(!parsed.body.contains("Author:"));
    }
}
