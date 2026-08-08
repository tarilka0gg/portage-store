use super::emerge::Job;
use anyhow::{Context, Result};
use std::process::Command;

/// Gentoo's community-run "unofficial" overlay — the single most common
/// answer to "where do I find this package that isn't in the main tree",
/// and the one overlay worth calling out by name rather than leaving
/// buried alphabetically among several hundred personal ones.
pub const GURU: &str = "guru";

/// One entry from `eselect repository list` — a repo the app could sync
/// packages from, whether or not it's currently enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    pub name: String,
    pub url: Option<String>,
    /// Whether this repo is already enabled (`eselect repository list -i`
    /// would include it) — covers both marker forms `eselect` prints
    /// (`*` and `#`; both showed up in `-i` output on a real system, so
    /// both count as "already enabled" here).
    pub enabled: bool,
}

/// Strips ANSI escape sequences — `eselect` colorizes for a real
/// terminal, and while a piped, non-tty `Command::output()` capture
/// already came back plain in practice, stripping defensively costs
/// nothing and avoids depending on that staying true.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c2 in chars.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Parses one line: `  [161] guru * (https://wiki.gentoo.org/wiki/Project:GURU)`
/// — index (unused, `enable`/`disable` below use the name instead, which
/// doesn't shift if the master list is ever re-fetched), name, an
/// optional one-character enabled marker, and an optional URL (some
/// entries, mostly ones with no public repo listed, have none).
fn parse_line(line: &str) -> Option<Overlay> {
    let line = strip_ansi(line);
    let rest = line.trim_start().strip_prefix('[')?;
    let (_, rest) = rest.split_once(']')?;
    let rest = rest.trim();

    let (name_and_marker, url) = match rest.rfind('(') {
        Some(paren_start) if rest.trim_end().ends_with(')') => {
            let url_end = rest.trim_end().len() - 1;
            (rest[..paren_start].trim(), Some(rest[paren_start + 1..url_end].to_string()))
        }
        _ => (rest, None),
    };

    let mut parts = name_and_marker.split_whitespace();
    let name = parts.next()?.to_string();
    let enabled = parts.next().is_some();
    Some(Overlay { name, url, enabled })
}

/// Every overlay Gentoo's master list knows about — hundreds of entries,
/// most of them personal single-maintainer repos. `eselect` fetches
/// (and caches) this from `api.gentoo.org` on first use, so the very
/// first call can involve a real network round trip; every call after
/// that is local.
pub fn list() -> Result<Vec<Overlay>> {
    let output = Command::new("eselect").args(["repository", "list", "--nocolor"]).output().context("failed to run eselect repository list")?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().filter_map(parse_line).collect())
}

/// Enables `name` — privileged, since it writes to `/etc/portage/repos.conf`.
/// Only makes the repo *known*; its packages aren't actually fetchable
/// until it's synced (folded into the same job via `&&`, one polkit
/// prompt instead of two, since enabling an overlay with nothing synced
/// yet isn't useful on its own).
pub fn enable_and_sync_job(name: &str) -> Job {
    Job {
        privileged: true,
        binary: "bash".into(),
        args: vec![
            "-c".into(),
            "eselect repository enable -- \"$1\" && emerge --sync --repo \"$1\"".into(),
            "bash".into(),
            name.into(),
        ],
    }
}

/// Disables `name` — the repo's own local copy is left in place (matching
/// `eselect repository disable`'s own default, not `-f`/force removal),
/// so re-enabling it later doesn't need a fresh sync.
pub fn disable_job(name: &str) -> Job {
    Job { privileged: true, binary: "eselect".into(), args: vec!["repository".into(), "disable".into(), name.into()] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_enabled_overlay_with_a_url() {
        let overlay = parse_line("  [161] guru * (https://wiki.gentoo.org/wiki/Project:GURU)").unwrap();
        assert_eq!(
            overlay,
            Overlay { name: "guru".to_string(), url: Some("https://wiki.gentoo.org/wiki/Project:GURU".to_string()), enabled: true }
        );
    }

    #[test]
    fn parses_a_disabled_overlay() {
        let overlay = parse_line("  [1]   2xsaiko (https://git.sr.ht/~dblsaiko/ebuilds)").unwrap();
        assert!(!overlay.enabled);
        assert_eq!(overlay.name, "2xsaiko");
    }

    #[test]
    fn an_overlay_with_no_url_still_parses() {
        let overlay = parse_line("  [16]  ambasta").unwrap();
        assert_eq!(overlay.name, "ambasta");
        assert_eq!(overlay.url, None);
        assert!(!overlay.enabled);
    }

    #[test]
    fn the_hash_marker_also_counts_as_enabled() {
        let overlay = parse_line("  [63]  CachyOS-kernels # (https://github.com/Szowisz/CachyOS-kernels)").unwrap();
        assert!(overlay.enabled);
    }

    #[test]
    fn non_entry_lines_are_ignored() {
        assert_eq!(parse_line("Available repositories:"), None);
        assert_eq!(parse_line("warning: rosa-ebuilds: unsupported source type cvs"), None);
    }
}
