use super::priv_write::HELPER_PATH;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::process::Command;

/// One entry from `eselect profile list` — a build profile symlink target
/// the system could be pointed at, whether or not it's the active one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The `[N]` index `eselect profile set` expects — not the path,
    /// since that's what the command itself actually takes.
    pub index: String,
    pub path: String,
    /// `"stable"`, `"exp"`, `"dev"` — whatever `eselect` prints in
    /// parentheses.
    pub status: String,
    pub active: bool,
}

/// Parses one line: `  [21]  default/linux/amd64/23.0/hardened (stable) *`
/// — index, path, status, and an optional trailing `*` marking the
/// currently active profile. Same shape as `overlays.rs`'s
/// `eselect repository list` parsing, just without a URL to handle.
fn parse_line(line: &str) -> Option<Profile> {
    let rest = line.trim_start().strip_prefix('[')?;
    let (index, rest) = rest.split_once(']')?;
    let rest = rest.trim();

    let paren_start = rest.find('(')?;
    let path = rest[..paren_start].trim().to_string();
    let after_paren = &rest[paren_start + 1..];
    let paren_end = after_paren.find(')')?;
    let status = after_paren[..paren_end].to_string();
    let active = after_paren[paren_end + 1..].trim() == "*";

    Some(Profile { index: index.trim().to_string(), path, status, active })
}

/// Every profile symlink target `eselect` knows about for this system's
/// architecture/subarch — unprivileged, synchronous (a local list, no
/// network round trip the way `overlays::list` can have on first use).
pub fn list() -> Result<Vec<Profile>> {
    let output = Command::new("eselect").args(["profile", "list", "--nocolor"]).output().context("failed to run eselect profile list")?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().filter_map(parse_line).collect())
}

/// The currently active profile's fully-resolved `USE=` — the real,
/// merged answer (profile chain + `make.conf`), not something this app
/// tries to recompute itself. Unprivileged, read-only.
pub fn resolved_use() -> Result<HashSet<String>> {
    let output = Command::new("portageq").args(["envvar", "USE"]).output().context("failed to run portageq envvar USE")?;
    if !output.status.success() {
        anyhow::bail!("portageq envvar USE exited with {:?}: {}", output.status.code(), String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).split_whitespace().map(str::to_string).collect())
}

/// Switches the active profile to `index` — a direct, blocking privileged
/// call (mirrors `config_history::revert`'s pattern) rather than a
/// queued `Job`: this is instant and deterministic, not a build worth
/// showing progress for. `eselect` is already in the priv-helper's `run`
/// binary allowlist (see `resources/priv-helper.sh`), so this needs no
/// helper script changes — just another `eselect` invocation through the
/// same passwordless path every other privileged `eselect` call already
/// uses (`overlays::disable_job`).
pub fn apply(index: &str) -> Result<()> {
    let output = Command::new("doas")
        .arg(HELPER_PATH)
        .arg("run")
        .arg("--")
        .arg("eselect")
        .arg("profile")
        .arg("set")
        .arg(index)
        .output()
        .context("failed to launch doas")?;
    if !output.status.success() {
        anyhow::bail!("failed to switch profile: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_active_profile() {
        let profile = parse_line("  [21]  default/linux/amd64/23.0/hardened (stable) *").unwrap();
        assert_eq!(
            profile,
            Profile { index: "21".to_string(), path: "default/linux/amd64/23.0/hardened".to_string(), status: "stable".to_string(), active: true }
        );
    }

    #[test]
    fn parses_an_inactive_profile() {
        let profile = parse_line("  [15]  default/linux/amd64/23.0/no-multilib/prefix (exp)").unwrap();
        assert!(!profile.active);
        assert_eq!(profile.status, "exp");
    }

    #[test]
    fn wide_index_columns_still_parse() {
        // Real `eselect` output right-pads the bracketed index differently
        // depending on how many digits the largest index has — single and
        // double-digit indices in the same listing don't line up, but the
        // parser shouldn't care either way.
        let profile = parse_line("  [1]   default/linux/amd64/23.0 (stable)").unwrap();
        assert_eq!(profile.index, "1");
        assert_eq!(profile.path, "default/linux/amd64/23.0");
    }

    #[test]
    fn non_entry_lines_are_ignored() {
        assert_eq!(parse_line("Available profile symlink targets:"), None);
    }
}
