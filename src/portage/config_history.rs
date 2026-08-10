use super::priv_write::{HELPER_PATH, TRACKED_DIR};
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// One commit in `/etc/portage`'s own history — every change this app
/// has ever made there, in order, human-readable without needing to know
/// git at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub hash: String,
    pub message: String,
    pub relative_time: String,
}

fn git(args: &[&str]) -> Result<String> {
    let output =
        Command::new("git").arg("-C").arg(TRACKED_DIR).args(args).output().context("failed to run git")?;
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether this app has ever written anything under `TRACKED_DIR` —
/// `priv_write::write_file_as_root` initializes the repo lazily, on its
/// own first write, so a fresh install with no changes made yet
/// legitimately has no history to show.
pub fn is_tracked() -> bool {
    Path::new(TRACKED_DIR).join(".git").is_dir()
}

/// The most recent commits, newest first — read-only, no root needed
/// (`/etc/portage` and everything this app writes under it stay
/// world-readable, the same as the rest of `/etc/portage` already is).
pub fn history(limit: usize) -> Result<Vec<CommitInfo>> {
    if !is_tracked() {
        return Ok(Vec::new());
    }
    // `\x1f` (unit separator) as the field delimiter rather than a space
    // or comma — a commit message is free text and could contain either.
    let output = git(&["log", &format!("-n{limit}"), "--pretty=format:%h\x1f%s\x1f%cr"])?;
    Ok(output.lines().filter_map(parse_log_line).collect())
}

fn parse_log_line(line: &str) -> Option<CommitInfo> {
    let mut parts = line.split('\x1f');
    Some(CommitInfo {
        hash: parts.next()?.to_string(),
        message: parts.next()?.to_string(),
        relative_time: parts.next()?.to_string(),
    })
}

/// Whether there's a real change to revert — the repo's very first
/// commit is always the pre-app baseline snapshot (see
/// `priv_write::WRITE_SCRIPT`), and reverting *that* would mean undoing
/// something this app never did.
pub fn can_revert() -> bool {
    history(2).map(|commits| commits.len() > 1).unwrap_or(false)
}

/// Reverts `target` (a commit hash, or `"HEAD"` for the most recent
/// change) — as its own new commit (`git revert`, not a history
/// rewrite), so the fact that a revert happened is itself part of the
/// same permanent record everything else here is for. Privileged (writes
/// to `/etc/portage`); blocking, matching the other privileged actions
/// already called directly from the preferences dialog (`make.conf`
/// save, adding a binary repo) rather than routed through the app's
/// install/update job queue, which this isn't one of.
pub fn revert(target: &str) -> Result<()> {
    let output =
        Command::new("doas").arg(HELPER_PATH).arg("git-revert").arg(target).output().context("failed to launch doas")?;
    if !output.status.success() {
        bail!("git revert failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

/// The full patch for one commit — `git show`, read-only, no privilege
/// needed (same reasoning as `history`: `/etc/portage` stays
/// world-readable).
pub fn diff(hash: &str) -> Result<String> {
    git(&["show", hash])
}

/// Tags the current `HEAD` with a name derived from `label`, before a
/// big operation (an `@world` update) actually starts — a known-good
/// point the history browser can jump back to later, distinct from an
/// ordinary revert (which only undoes the single most recent commit).
/// Force-moves the tag if one with this exact generated name somehow
/// already exists (same run started twice in the same second), matching
/// `git tag -f`'s normal semantics. Best-effort by design: callers
/// should never let a tagging failure block the actual operation it was
/// meant to bookmark.
pub fn tag_before(label: &str) -> Result<()> {
    let unix_time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let name = format!("pre-{label}-{unix_time}");
    let output = Command::new("doas").arg(HELPER_PATH).arg("git-tag").arg(&name).output().context("failed to launch doas")?;
    if !output.status.success() {
        bail!("git tag failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_log_line() {
        let line = "a1b2c3d\x1fUpdate package.use/zz-portage-store\x1f3 minutes ago";
        let info = parse_log_line(line).unwrap();
        assert_eq!(info.hash, "a1b2c3d");
        assert_eq!(info.message, "Update package.use/zz-portage-store");
        assert_eq!(info.relative_time, "3 minutes ago");
    }

    #[test]
    fn a_short_line_is_not_a_commit() {
        assert_eq!(parse_log_line("only-one-field"), None);
    }
}
