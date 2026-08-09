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

/// Writes `content` to `path` as root. When `path` falls under
/// `TRACKED_DIR`, the write is wrapped in a git commit (initializing a
/// repo there — with a baseline snapshot of whatever already existed —
/// on the very first write this app ever makes) rather than a plain
/// `pkexec tee`; a write outside that directory (there currently are
/// none, but nothing enforces it) just writes the file.
pub fn write_file_as_root(path: &str, content: &str) -> Result<()> {
    let message = default_commit_message(path);
    let mut child = Command::new("pkexec")
        .arg("bash")
        .arg("-c")
        .arg(WRITE_SCRIPT)
        .arg("bash") // $0 — conventionally the script's own name, unused here
        .arg(path)
        .arg(&message)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to launch pkexec")?;

    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(content.as_bytes())
        .context("failed to write to pkexec stdin")?;

    let out = child.wait_with_output().context("pkexec did not exit cleanly")?;
    if !out.status.success() {
        bail!(
            "failed to write {path} as root: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// `$1` is the target path, `$2` the git commit message to use if the
/// target falls under `TRACKED_DIR`.
///
/// The baseline-snapshot init deliberately runs *before* `cat > "$TARGET"`
/// writes anything — staging and committing "whatever's already there"
/// only captures pre-existing state if it runs before this write adds
/// anything new to capture. Doing it the other way around (write first,
/// init after) was tried and is wrong: the brand new file would already
/// exist on disk by the time the baseline commit's `git add -A` runs, so
/// the "baseline" would silently absorb this write's own content instead
/// of representing only what came before it, leaving nothing left to
/// commit as *this* write's own change.
const WRITE_SCRIPT: &str = r#"set -euo pipefail
TARGET="$1"
MESSAGE="$2"
REPO_DIR="/etc/portage"

case "$TARGET" in
  "$REPO_DIR"/*)
    if [ ! -d "$REPO_DIR/.git" ]; then
        git -C "$REPO_DIR" init -q
        git -C "$REPO_DIR" config user.name "Portage Store"
        git -C "$REPO_DIR" config user.email "portage-store@localhost"
        git -C "$REPO_DIR" add -A
        git -C "$REPO_DIR" commit -q -m "Baseline snapshot" --allow-empty
    fi
    ;;
esac

cat > "$TARGET"

case "$TARGET" in
  "$REPO_DIR"/*)
    git -C "$REPO_DIR" add -A
    if ! git -C "$REPO_DIR" diff --cached --quiet; then
        git -C "$REPO_DIR" commit -q -m "$MESSAGE"
    fi
    ;;
esac
"#;

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
/// one privileged operation instead of two separate `pkexec` calls, so
/// resolving a pending config update (write the accepted/merged content,
/// clear the `._cfgNNNN_name` file that prompted it) needs only one
/// polkit prompt. Paths are passed as `bash` positional arguments rather
/// than interpolated into the script source, so nothing about either path
/// needs shell-escaping.
pub fn write_then_remove_as_root(live_path: &Path, content: &str, discard_path: &Path) -> Result<()> {
    let mut child = Command::new("pkexec")
        .arg("bash")
        .arg("-c")
        .arg(r#"cat > "$1" && rm -f "$2""#)
        .arg("bash") // $0 — conventionally the script's own name, unused here
        .arg(live_path)
        .arg(discard_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to launch pkexec")?;

    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(content.as_bytes())
        .context("failed to write to pkexec stdin")?;

    let out = child.wait_with_output().context("pkexec did not exit cleanly")?;
    if !out.status.success() {
        bail!("failed to update {}: {}", live_path.display(), String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// Removes `path` as root — used to discard a pending config update
/// (`._cfgNNNN_name`) while leaving the current live file untouched.
pub fn remove_file_as_root(path: &Path) -> Result<()> {
    let out =
        Command::new("pkexec").args(["rm", "-f"]).arg(path).output().context("failed to launch pkexec")?;
    if !out.status.success() {
        bail!("failed to remove {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// Applies `pairs` (each a `(live_path, proposed_path)`, in the shape
/// `write_then_remove_as_root` already handles one at a time) as a
/// *single* privileged operation — one `pkexec` call, one polkit
/// authentication, for however many files there are. Calling
/// `write_then_remove_as_root` once per file for a bulk "accept
/// everything" action means one polkit prompt *per file*, back to back —
/// which reads as broken (the first prompt appears, then nothing
/// visibly happens for the rest) rather than as N legitimate requests.
/// Paths are passed as positional arguments to the script, not
/// interpolated into its source, so nothing about any of them needs
/// shell-escaping.
pub fn write_then_remove_many_as_root(pairs: &[(&Path, &Path)]) -> Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let mut command = Command::new("pkexec");
    command.arg("bash").arg("-c").arg(BULK_WRITE_THEN_REMOVE_SCRIPT).arg("bash");
    for (live_path, proposed_path) in pairs {
        command.arg(live_path).arg(proposed_path);
    }
    let out = command.output().context("failed to launch pkexec")?;
    if !out.status.success() {
        bail!("failed to apply {} update(s): {}", pairs.len(), String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// `$@` is `live_path proposed_path` pairs, flattened — `cp` (not a
/// stdin pipe, since there's no single content stream for N files) the
/// proposed content onto the live file, then remove the now-applied
/// `._cfgNNNN_` file, once per pair.
const BULK_WRITE_THEN_REMOVE_SCRIPT: &str = r#"set -euo pipefail
while [ "$#" -ge 2 ]; do
    live="$1"
    proposed="$2"
    shift 2
    cp -- "$proposed" "$live"
    rm -f -- "$proposed"
done
"#;
