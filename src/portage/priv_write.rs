use super::command::{CommandRunner, RealCommandRunner};
use anyhow::{Context, Result, bail};
use std::path::Path;

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
pub const HELPER_PATH: &str = "/usr/libexec/portage-store/priv-helper";

/// Writes `content` to `path` as root. When `path` falls under
/// `TRACKED_DIR`, the helper wraps the write in a git commit
/// (initializing a repo there — with a baseline snapshot of whatever
/// already existed — on the very first write this app ever makes)
/// rather than a plain write; a write outside that directory is rejected
/// by the helper itself (`write-tracked` is scoped to `TRACKED_DIR`).
pub fn write_file_as_root(path: &str, content: &str) -> Result<()> {
    write_file_as_root_with(&RealCommandRunner, path, content)
}

fn write_file_as_root_with(runner: &impl CommandRunner, path: &str, content: &str) -> Result<()> {
    let message = default_commit_message(path);
    let out = runner
        .output_with_stdin("doas", &[HELPER_PATH, "write-tracked", path, &message], content.as_bytes())
        .context("failed to launch doas")?;
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
    write_then_remove_as_root_with(&RealCommandRunner, live_path, content, discard_path)
}

fn write_then_remove_as_root_with(
    runner: &impl CommandRunner,
    live_path: &Path,
    content: &str,
    discard_path: &Path,
) -> Result<()> {
    let live = live_path.to_string_lossy();
    let discard = discard_path.to_string_lossy();
    let out = runner
        .output_with_stdin("doas", &[HELPER_PATH, "write-then-remove", &live, &discard], content.as_bytes())
        .context("failed to launch doas")?;
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
    remove_file_as_root_with(&RealCommandRunner, path)
}

fn remove_file_as_root_with(runner: &impl CommandRunner, path: &Path) -> Result<()> {
    let path_str = path.to_string_lossy();
    let out = runner.output("doas", &[HELPER_PATH, "remove-file", &path_str]).context("failed to launch doas")?;
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
    mkdir_p_as_root_with(&RealCommandRunner, dir)
}

fn mkdir_p_as_root_with(runner: &impl CommandRunner, dir: &str) -> Result<()> {
    let out = runner.output("doas", &[HELPER_PATH, "mkdir-p", dir]).context("failed to launch doas")?;
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
    write_then_remove_many_as_root_with(&RealCommandRunner, pairs)
}

fn write_then_remove_many_as_root_with(runner: &impl CommandRunner, pairs: &[(&Path, &Path)]) -> Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let strings: Vec<String> =
        pairs.iter().flat_map(|(live, discard)| [live.to_string_lossy().into_owned(), discard.to_string_lossy().into_owned()]).collect();
    let mut args: Vec<&str> = vec![HELPER_PATH, "write-then-remove-many", "--"];
    args.extend(strings.iter().map(String::as_str));
    let out = runner.output("doas", &args).context("failed to launch doas")?;
    if !out.status.success() {
        bail!("failed to apply {} update(s): {}", pairs.len(), String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portage::command::fake::{FakeCommandRunner, err, ok};

    #[test]
    fn default_commit_message_strips_the_tracked_dir_prefix() {
        assert_eq!(default_commit_message("/etc/portage/package.use/zz-portage-store"), "Update package.use/zz-portage-store");
    }

    #[test]
    fn default_commit_message_falls_back_to_the_whole_path_outside_tracked_dir() {
        assert_eq!(default_commit_message("/etc/binrepos.conf/zz-portage-store.conf"), "Update /etc/binrepos.conf/zz-portage-store.conf");
    }

    #[test]
    fn write_file_as_root_succeeds_and_calls_the_right_subcommand() {
        let runner = FakeCommandRunner::new();
        runner.respond("doas", ok(""));
        write_file_as_root_with(&runner, "/etc/portage/make.conf", "USE=\"x\"").unwrap();
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "doas");
        assert_eq!(calls[0].1[1], "write-tracked");
        assert_eq!(calls[0].1[2], "/etc/portage/make.conf");
    }

    #[test]
    fn write_file_as_root_surfaces_the_helper_stderr_on_failure() {
        let runner = FakeCommandRunner::new();
        runner.respond("doas", err("priv-helper: /etc/shadow is not under /etc/portage"));
        let result = write_file_as_root_with(&runner, "/etc/shadow", "x");
        assert!(result.unwrap_err().to_string().contains("is not under /etc/portage"));
    }

    #[test]
    fn remove_file_as_root_surfaces_failure() {
        let runner = FakeCommandRunner::new();
        runner.respond("doas", err("priv-helper: not a plausible path"));
        let result = remove_file_as_root_with(&runner, Path::new("/etc/foo"));
        assert!(result.is_err());
    }

    #[test]
    fn mkdir_p_as_root_succeeds() {
        let runner = FakeCommandRunner::new();
        runner.respond("doas", ok(""));
        mkdir_p_as_root_with(&runner, "/etc/portage/binrepos.conf").unwrap();
    }

    #[test]
    fn write_then_remove_many_as_root_is_a_no_op_for_an_empty_list() {
        let runner = FakeCommandRunner::new();
        write_then_remove_many_as_root_with(&runner, &[]).unwrap();
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn write_then_remove_many_as_root_passes_every_pair() {
        let runner = FakeCommandRunner::new();
        runner.respond("doas", ok(""));
        let live1 = Path::new("/etc/portage/package.use/a");
        let discard1 = Path::new("/etc/portage/package.use/._cfg0000_a");
        let live2 = Path::new("/etc/portage/make.conf");
        let discard2 = Path::new("/etc/portage/._cfg0001_make.conf");
        write_then_remove_many_as_root_with(&runner, &[(live1, discard1), (live2, discard2)]).unwrap();
        let calls = runner.calls.borrow();
        assert_eq!(calls[0].1[1], "write-then-remove-many");
        assert_eq!(calls[0].1[2], "--");
        assert_eq!(calls[0].1.len(), 3 + 4);
    }
}
