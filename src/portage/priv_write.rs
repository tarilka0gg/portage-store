use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// The one directory this app ever tracks changes to (see
/// `config_history.rs`) — every file this app writes under here gets
/// committed on the way in, giving a real "what did the GUI do to my
/// system" history and a "revert last change" that actually means
/// something. Deliberately scoped to `/etc/portage` alone, not all of
/// `/etc`: it's the one directory this app owns the writes to end to end
/// (`package.use`, `package.accept_keywords`, `package.license`,
/// `make.conf`, `binrepos.conf`) — a `CONFIG_PROTECT` resolution can
/// target arbitrary paths elsewhere under `/etc` that this app doesn't
/// own the way it owns its own managed files there.
pub const TRACKED_DIR: &str = "/etc/portage";

/// Every privileged operation in this app goes through this one
/// root-owned script (see `resources/priv-helper.sh` in the repo for the
/// reviewable source; installed by hand to this path, not by the app
/// itself) via a passwordless `doas` rule scoped to exactly this command
/// (see `/etc/doas.conf`). Unlike the `pkexec bash -c "<script text>"`
/// pattern this replaced, script *content* never travels through the
/// privileged call as an argument — only a subcommand name and validated
/// paths do. The helper owns all the actual logic (git tracking, path
/// scoping, the `emerge`/sandbox-build binary allowlist) and is the only
/// thing a passwordless rule needs to trust, rather than trusting every
/// caller in this binary to never mishandle a script string.
pub const HELPER_PATH: &str = "/usr/local/libexec/portage-store/priv-helper";

/// Writes `content` to `path` as root. When `path` falls under
/// `TRACKED_DIR`, the helper wraps the write in a git commit
/// (initializing a repo there — with a baseline snapshot of whatever
/// already existed — on the very first write this app ever makes)
/// rather than a plain write; a write outside that directory is rejected
/// by the helper itself (`write-tracked` is scoped to `TRACKED_DIR`).
pub fn write_file_as_root(path: &str, content: &str) -> Result<()> {
    let message = default_commit_message(path);
    let mut child = Command::new("doas")
        .arg(HELPER_PATH)
        .arg("write-tracked")
        .arg(path)
        .arg(&message)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to launch doas")?;

    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(content.as_bytes())
        .context("failed to write to doas stdin")?;

    let out = child.wait_with_output().context("doas did not exit cleanly")?;
    if !out.status.success() {
        bail!(
            "failed to write {path} as root: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// A reasonable default commit message derived from the path alone
/// (`/etc/portage/package.use/zz-portage-store` -> "Update
/// package.use/zz-portage-store") — callers don't currently have a
/// richer description to pass in, and the commit's own diff (visible via
/// `git show`) carries the real detail regardless.
fn default_commit_message(path: &str) -> String {
    let relative = path.strip_prefix(TRACKED_DIR).map(|p| p.trim_start_matches('/')).unwrap_or(path);
    format!("Update {relative}")
}

/// Writes `content` to `live_path` as root, then removes `discard_path` —
/// one privileged operation instead of two separate calls, so resolving a
/// pending config update (write the accepted/merged content, clear the
/// `._cfgNNNN_name` file that prompted it) needs only one call into the
/// helper. Both paths are validated by the helper to be under `/etc` and
/// `discard_path` to actually look like a `._cfgNNNN_name` file.
pub fn write_then_remove_as_root(live_path: &Path, content: &str, discard_path: &Path) -> Result<()> {
    let mut child = Command::new("doas")
        .arg(HELPER_PATH)
        .arg("write-then-remove")
        .arg(live_path)
        .arg(discard_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to launch doas")?;

    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(content.as_bytes())
        .context("failed to write to doas stdin")?;

    let out = child.wait_with_output().context("doas did not exit cleanly")?;
    if !out.status.success() {
        bail!("failed to update {}: {}", live_path.display(), String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// Removes `path` as root — used to discard a pending config update
/// (`._cfgNNNN_name`) while leaving the current live file untouched, and
/// to delete an env override file. Validated by the helper to be under
/// `/etc`.
pub fn remove_file_as_root(path: &Path) -> Result<()> {
    let out = Command::new("doas")
        .arg(HELPER_PATH)
        .arg("remove-file")
        .arg(path)
        .output()
        .context("failed to launch doas")?;
    if !out.status.success() {
        bail!("failed to remove {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// Creates `dir` (and any missing parents) as root — used when a
/// `package.*`-style directory (e.g. `binrepos.conf`) doesn't exist yet
/// on a system that's never had one configured. Validated by the helper
/// to be under `/etc/portage`.
pub fn mkdir_p_as_root(dir: &str) -> Result<()> {
    let out = Command::new("doas").arg(HELPER_PATH).arg("mkdir-p").arg(dir).output().context("failed to launch doas")?;
    if !out.status.success() {
        bail!("failed to create {dir}: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// Applies `pairs` (each a `(live_path, proposed_path)`, in the shape
/// `write_then_remove_as_root` already handles one at a time) as a
/// *single* privileged operation — one `doas` call for however many
/// files there are. Calling `write_then_remove_as_root` once per file for
/// a bulk "accept everything" action would mean N separate privileged
/// calls in a row for no reason once batching is this cheap.
pub fn write_then_remove_many_as_root(pairs: &[(&Path, &Path)]) -> Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let mut command = Command::new("doas");
    command.arg(HELPER_PATH).arg("write-then-remove-many").arg("--");
    for (live_path, proposed_path) in pairs {
        command.arg(live_path).arg(proposed_path);
    }
    let out = command.output().context("failed to launch doas")?;
    if !out.status.success() {
        bail!("failed to apply {} update(s): {}", pairs.len(), String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}
